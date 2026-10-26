//! Serial console settings: which COM port, and how the line is set up.
//!
//! A serial target is written like `COM3`, `COM3:115200` or
//! `COM3:115200,8N1,rtscts`: the port, then the speed, the framing (data
//! bits, parity N/O/E/M/S, stop bits 1, 1.5 or 2) and the flow control
//! (`none`, `xonxoff`, `rtscts` or `dsrdtr`), each optional and in any
//! order. What is left out is 9600 8N1 with no flow control, the usual
//! console default of switches, routers and boards. A saved serial
//! session keeps the line settings in its profile as
//! `Serial 115200,8N1,none`.

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
    Mark,
    Space,
}

impl Parity {
    fn letter(self) -> char {
        match self {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
            Parity::Mark => 'M',
            Parity::Space => 'S',
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StopBits {
    #[default]
    One,
    OneAndHalf,
    Two,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Flow {
    #[default]
    None,
    /// Software flow control: XON/XOFF characters in the data.
    XonXoff,
    /// Hardware flow control on the RTS and CTS lines.
    RtsCts,
    /// Hardware flow control on the DTR and DSR lines.
    DsrDtr,
}

impl Flow {
    pub const ALL: [Flow; 4] = [Flow::None, Flow::XonXoff, Flow::RtsCts, Flow::DsrDtr];

    pub fn name(self) -> &'static str {
        match self {
            Flow::None => "none",
            Flow::XonXoff => "xonxoff",
            Flow::RtsCts => "rtscts",
            Flow::DsrDtr => "dsrdtr",
        }
    }

    /// How the session form shows it.
    pub fn label(self) -> &'static str {
        match self {
            Flow::None => "none",
            Flow::XonXoff => "XON/XOFF (software)",
            Flow::RtsCts => "RTS/CTS (hardware)",
            Flow::DsrDtr => "DSR/DTR (hardware)",
        }
    }

    fn parse(text: &str) -> Option<Flow> {
        match text.to_ascii_lowercase().replace(['/', '-'], "").as_str() {
            "none" | "off" => Some(Flow::None),
            "xonxoff" | "xon" | "software" => Some(Flow::XonXoff),
            "rtscts" | "hardware" => Some(Flow::RtsCts),
            "dsrdtr" | "dtrdsr" => Some(Flow::DsrDtr),
            _ => None,
        }
    }
}

/// The speeds the session form offers; any other speed can be typed.
pub const SPEEDS: [u32; 9] = [
    1200, 2400, 4800, 9600, 19_200, 38_400, 57_600, 115_200, 230_400,
];
/// The highest speed accepted, above what USB serial adapters reach.
pub const MAX_SPEED: u32 = 4_000_000;
/// The framings the session form offers.
pub const FRAMINGS: [&str; 6] = ["8N1", "8E1", "8O1", "7E1", "7O1", "8N2"];

/// How a serial line is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSettings {
    pub speed: u32,
    /// 5 to 8.
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow: Flow,
}

impl Default for LineSettings {
    fn default() -> Self {
        Self {
            speed: 9600,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow: Flow::None,
        }
    }
}

impl LineSettings {
    /// The framing as written: `8N1`, `7E1`, `8N1.5`.
    pub fn framing(&self) -> String {
        let stop = match self.stop_bits {
            StopBits::One => "1",
            StopBits::OneAndHalf => "1.5",
            StopBits::Two => "2",
        };
        format!("{}{}{stop}", self.data_bits, self.parity.letter())
    }

    /// Sets the framing from text like `8N1`; `false` if it is not one.
    pub fn set_framing(&mut self, text: &str) -> bool {
        let text = text.to_ascii_uppercase();
        let mut chars = text.chars();
        let (Some(bits), Some(parity)) = (chars.next(), chars.next()) else {
            return false;
        };
        let data_bits = match bits {
            '5'..='8' => bits as u8 - b'0',
            _ => return false,
        };
        let parity = match parity {
            'N' => Parity::None,
            'O' => Parity::Odd,
            'E' => Parity::Even,
            'M' => Parity::Mark,
            'S' => Parity::Space,
            _ => return false,
        };
        let stop_bits = match chars.as_str() {
            "1" => StopBits::One,
            "1.5" => StopBits::OneAndHalf,
            "2" => StopBits::Two,
            _ => return false,
        };
        self.data_bits = data_bits;
        self.parity = parity;
        self.stop_bits = stop_bits;
        true
    }

    /// Settings like `115200,8N1,rtscts`; each part is optional and the
    /// parts may come in any order, split by commas or spaces.
    pub fn parse(text: &str) -> Option<Self> {
        let mut out = Self::default();
        for part in text
            .split([',', ' ', '\t'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            if let Ok(speed) = part.parse::<u32>() {
                if !(50..=MAX_SPEED).contains(&speed) {
                    return None;
                }
                out.speed = speed;
            } else if let Some(flow) = Flow::parse(part) {
                out.flow = flow;
            } else if !out.set_framing(part) {
                return None;
            }
        }
        Some(out)
    }
}

impl fmt::Display for LineSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{}", self.speed, self.framing(), self.flow.name())
    }
}

/// A COM port and its line settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialTarget {
    /// `COM1` to `COM256`, in capitals.
    pub port: String,
    pub settings: LineSettings,
}

impl SerialTarget {
    /// The name to open: `\\.\COM3`, which also reaches COM10 and above.
    pub fn device_path(&self) -> String {
        format!(r"\\.\{}", self.port)
    }

    /// What a tab and a log are named after: `COM3 115200 8N1`.
    pub fn label(&self) -> String {
        format!(
            "{} {} {}",
            self.port,
            self.settings.speed,
            self.settings.framing()
        )
    }
}

impl fmt::Display for SerialTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.port, self.settings)
    }
}

/// `COM3` (any case, `\\.\` in front allowed) as `COM3`; `None` for
/// anything else.
pub fn port_name(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text.strip_prefix(r"\\.\").unwrap_or(text);
    let digits = text
        .get(..3)
        .filter(|p| p.eq_ignore_ascii_case("com"))
        .and(text.get(3..))?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u16 = digits.parse().ok()?;
    (1..=256).contains(&n).then(|| format!("COM{n}"))
}

/// A serial target as typed at the connect prompt or on the command line:
/// `COM3`, `COM3:115200`, `com3 115200 7E1 xonxoff`.
pub fn parse_target(text: &str) -> Option<SerialTarget> {
    let text = text.trim();
    let split = text.find([':', ',', ' ']).unwrap_or(text.len());
    let (port, rest) = text.split_at(split);
    let port = port_name(port)?;
    let rest = rest.strip_prefix([':', ',', ' ']).unwrap_or(rest);
    Some(SerialTarget {
        port,
        settings: LineSettings::parse(rest)?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn port_names() {
        assert_eq!(port_name("com3").as_deref(), Some("COM3"));
        assert_eq!(port_name(r"\\.\COM12").as_deref(), Some("COM12"));
        for bad in [
            "COM0", "COM", "COM03", "COM257", "COMX", "LPT1", "COM1a", "web",
        ] {
            assert_eq!(port_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn targets_with_and_without_settings() {
        let t = parse_target("COM3").unwrap();
        assert_eq!(t.settings, LineSettings::default());
        assert_eq!(t.to_string(), "COM3:9600,8N1,none");
        assert_eq!(t.device_path(), r"\\.\COM3");

        let t = parse_target("com4:115200,7E1,rtscts").unwrap();
        assert_eq!(t.port, "COM4");
        assert_eq!(t.settings.speed, 115_200);
        assert_eq!(t.settings.data_bits, 7);
        assert_eq!(t.settings.parity, Parity::Even);
        assert_eq!(t.settings.flow, Flow::RtsCts);
        assert_eq!(t.label(), "COM4 115200 7E1");

        let t = parse_target("COM1 xon-xoff 8N1.5 57600").unwrap();
        assert_eq!(t.settings.stop_bits, StopBits::OneAndHalf);
        assert_eq!(t.settings.flow, Flow::XonXoff);
        assert_eq!(t.settings.framing(), "8N1.5");
    }

    #[test]
    fn hosts_and_bad_settings_are_not_targets() {
        for text in [
            "web",
            "admin@com3",
            "COM3:9X1",
            "COM3:10",
            "COM3:5000000",
            "COM3:fast",
            "comtest.example.org",
        ] {
            assert_eq!(parse_target(text), None, "{text}");
        }
    }

    #[test]
    fn settings_round_trip() {
        for text in ["9600,8N1,none", "115200,7O2,dsrdtr", "1200,5S1.5,xonxoff"] {
            assert_eq!(LineSettings::parse(text).unwrap().to_string(), text);
        }
        for f in FRAMINGS {
            let mut s = LineSettings::default();
            assert!(s.set_framing(f));
            assert_eq!(s.framing(), f);
        }
    }
}
