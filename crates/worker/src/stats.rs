//! The server status bar's samples: a fixed, read-only command on an exec
//! channel of a logged-in connection prints the server's `/proc` figures
//! and `df` every few seconds, and [`Sampler`] turns them into
//! [`ServerStats`]. The worker reads that output itself, so the UI only
//! ever gets the handful of numbers in [`Message::Stats`], never the
//! server's text.
//!
//! The command marks each part of a sample with a line of its own (`@u`
//! uptime, `@c` the CPU line of `/proc/stat`, `@m` memory, `@n`
//! `/proc/net/dev`, `@d` the root file system's `df` line) and ends the
//! sample with `@e`. A server without `/proc` (macOS, the BSDs) has only
//! `df`, so only the disk shows. The server is not trusted: every line is
//! capped, every number checked, and nothing is kept between lines but a
//! few counters, so no output makes the worker hold more than a line.

use std::sync::Arc;

use ferossh_ipc::{ChannelId, Message, ServerStats};
use russh::ChannelMsg;
use tokio::sync::mpsc;

use crate::session::{Connection, Connections};
use crate::sftp::release;

/// The longest line read; the rest of a longer one is skipped.
const MAX_LINE: usize = 512;
/// Network interfaces counted per sample; the rest are skipped.
const MAX_INTERFACES: usize = 256;
/// The longest interval asked for, in seconds.
const MAX_INTERVAL: u16 = 3600;

/// The command that prints a sample every `interval` seconds. `sh -c`, so
/// it runs the same under any login shell that has single quotes; the
/// loop stops when the channel closes (`echo` fails), or the `sleep`
/// does.
pub fn command(interval: u16) -> Vec<u8> {
    let interval = interval.clamp(1, MAX_INTERVAL);
    format!(
        "sh -c 'while :; do \
         echo @u; cat /proc/uptime 2>/dev/null; \
         echo @c; head -n 1 /proc/stat 2>/dev/null; \
         echo @m; grep -E \"^(MemTotal|MemFree|MemAvailable|Buffers|Cached):\" /proc/meminfo 2>/dev/null; \
         echo @n; cat /proc/net/dev 2>/dev/null; \
         echo @d; df -kP / 2>/dev/null | tail -n 1; \
         echo @e || exit; sleep {interval} || exit; done'"
    )
    .into_bytes()
}

/// Which part of a sample the lines belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    None,
    Uptime,
    Cpu,
    Memory,
    Network,
    Disk,
}

/// The CPU counters of `/proc/stat`'s first line, in clock ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cpu {
    total: u64,
    idle: u64,
}

/// What one sample said, before comparing it with the last.
#[derive(Clone, Copy, Debug, Default)]
struct Raw {
    /// Seconds since boot, in hundredths.
    uptime: Option<u64>,
    cpu: Option<Cpu>,
    mem_total: Option<u64>,
    mem_free: Option<u64>,
    mem_available: Option<u64>,
    buffers: Option<u64>,
    cached: Option<u64>,
    /// Received and sent, every interface but loopback, and how many
    /// interface lines were read.
    rx: u64,
    tx: u64,
    interfaces: usize,
    net_ok: bool,
    disk: Option<(u64, u64)>,
}

/// A complete sample's uptime, CPU and network counters.
type Last = (u64, Option<Cpu>, Option<(u64, u64)>);

/// Reads the sampling command's output, a byte at a time if need be.
#[derive(Debug)]
pub struct Sampler {
    line: Vec<u8>,
    /// The current line ran past [`MAX_LINE`]: skip to its end.
    skipping: bool,
    part: Part,
    raw: Raw,
    /// The last complete sample's uptime, CPU and network counters.
    last: Option<Last>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            line: Vec::with_capacity(MAX_LINE),
            skipping: false,
            part: Part::None,
            raw: Raw::default(),
            last: None,
        }
    }

    /// Takes more output; every sample it completes goes to `done`.
    pub fn feed(&mut self, bytes: &[u8], mut done: impl FnMut(ServerStats)) {
        for &b in bytes {
            if b == b'\n' {
                if !self.skipping {
                    let line = std::mem::take(&mut self.line);
                    if let Some(stats) = self.line_done(&line) {
                        done(stats);
                    }
                    self.line = line;
                }
                self.line.clear();
                self.skipping = false;
            } else if self.skipping {
            } else if self.line.len() >= MAX_LINE {
                self.line.clear();
                self.skipping = true;
            } else {
                self.line.push(b);
            }
        }
    }

    fn line_done(&mut self, line: &[u8]) -> Option<ServerStats> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Ok(text) = std::str::from_utf8(line) else {
            return None;
        };
        let part = match text {
            "@u" => Some(Part::Uptime),
            "@c" => Some(Part::Cpu),
            "@m" => Some(Part::Memory),
            "@n" => Some(Part::Network),
            "@d" => Some(Part::Disk),
            "@e" => return Some(self.sample_done()),
            _ => None,
        };
        if let Some(p) = part {
            self.part = p;
            return None;
        }
        let raw = &mut self.raw;
        match self.part {
            Part::None => {}
            Part::Uptime => {
                if raw.uptime.is_none() {
                    raw.uptime = text.split_whitespace().next().and_then(hundredths);
                }
            }
            Part::Cpu => {
                if raw.cpu.is_none() {
                    raw.cpu = cpu_line(text);
                }
            }
            Part::Memory => {
                if let Some((name, kib)) = meminfo_line(text) {
                    let slot = match name {
                        "MemTotal" => &mut raw.mem_total,
                        "MemFree" => &mut raw.mem_free,
                        "MemAvailable" => &mut raw.mem_available,
                        "Buffers" => &mut raw.buffers,
                        "Cached" => &mut raw.cached,
                        _ => return None,
                    };
                    slot.get_or_insert(kib);
                }
            }
            Part::Network => {
                if raw.interfaces < MAX_INTERFACES
                    && let Some((name, rx, tx)) = net_line(text)
                {
                    raw.interfaces += 1;
                    raw.net_ok = true;
                    if name != "lo" {
                        raw.rx = raw.rx.saturating_add(rx);
                        raw.tx = raw.tx.saturating_add(tx);
                    }
                }
            }
            Part::Disk => {
                if raw.disk.is_none() {
                    raw.disk = df_line(text);
                }
            }
        }
        None
    }

    /// `@e`: the sample is whole; compare it with the last.
    fn sample_done(&mut self) -> ServerStats {
        let raw = std::mem::take(&mut self.raw);
        self.part = Part::None;
        let mem = raw.mem_total.filter(|&t| t > 0).and_then(|total| {
            let available = raw.mem_available.or_else(|| {
                // Before Linux 3.14: free, plus buffers and page cache.
                Some(
                    raw.mem_free?
                        .saturating_add(raw.buffers.unwrap_or(0))
                        .saturating_add(raw.cached.unwrap_or(0)),
                )
            })?;
            Some((total.saturating_sub(available), total))
        });
        let net_now = raw.net_ok.then_some((raw.rx, raw.tx));
        let mut stats = ServerStats {
            cpu: None,
            mem,
            disk: raw.disk,
            net: None,
        };
        if let (Some(now), Some((then, cpu_then, net_then))) = (raw.uptime, self.last)
            && let Some(elapsed) = now.checked_sub(then).filter(|&e| e > 0)
        {
            if let (Some(a), Some(b)) = (cpu_then, raw.cpu)
                && let (Some(total), Some(idle)) =
                    (b.total.checked_sub(a.total), b.idle.checked_sub(a.idle))
                && total > 0
                && idle <= total
            {
                let busy = u128::from(total - idle) * u128::from(ServerStats::CPU_FULL)
                    / u128::from(total);
                stats.cpu = Some(busy.min(u128::from(ServerStats::CPU_FULL)) as u16);
            }
            if let (Some((rx0, tx0)), Some((rx1, tx1))) = (net_then, net_now)
                && let (Some(rx), Some(tx)) = (rx1.checked_sub(rx0), tx1.checked_sub(tx0))
            {
                let rate = |bytes: u64| {
                    (u128::from(bytes) * 100 / u128::from(elapsed)).min(u128::from(u64::MAX)) as u64
                };
                stats.net = Some((rate(rx), rate(tx)));
            }
        }
        self.last = raw.uptime.map(|u| (u, raw.cpu, net_now));
        stats
    }
}

/// A decimal number of seconds (`12345.67`) in hundredths.
fn hundredths(text: &str) -> Option<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let whole: u64 = digits(whole)?;
    let frac = frac.as_bytes();
    if !frac.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let tenths = frac.first().map_or(0, |d| u64::from(d - b'0'));
    let hundredths = frac.get(1).map_or(0, |d| u64::from(d - b'0'));
    whole
        .checked_mul(100)?
        .checked_add(tenths * 10 + hundredths)
}

/// An unsigned decimal of ASCII digits only.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `cpu  user nice system idle iowait irq softirq steal ...`: all of it,
/// and idle plus I/O wait. Guest time is already in user time, so the
/// guest columns (the ninth and tenth) are left out of the total.
fn cpu_line(text: &str) -> Option<Cpu> {
    let mut fields = text.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let mut cpu = Cpu::default();
    let mut count = 0;
    for (i, f) in fields.enumerate() {
        let v = digits(f)?;
        if i < 8 {
            cpu.total = cpu.total.checked_add(v)?;
        }
        if i == 3 || i == 4 {
            cpu.idle = cpu.idle.checked_add(v)?;
        }
        count += 1;
    }
    (count >= 4).then_some(cpu)
}

/// `MemTotal:       16314660 kB`.
fn meminfo_line(text: &str) -> Option<(&str, u64)> {
    let (name, rest) = text.split_once(':')?;
    let mut fields = rest.split_whitespace();
    let kib = digits(fields.next()?)?;
    match fields.next() {
        Some("kB") | None => Some((name, kib)),
        _ => None,
    }
}

/// `  eth0: 1234 5 0 0 0 0 0 0 5678 9 ...`: the interface, bytes received
/// (first column) and sent (ninth).
fn net_line(text: &str) -> Option<(&str, u64, u64)> {
    let (name, rest) = text.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let fields: Vec<&str> = rest.split_whitespace().take(16).collect();
    if fields.len() < 9 {
        return None;
    }
    Some((name, digits(fields.first()?)?, digits(fields.get(8)?)?))
}

/// `/dev/sda1  41152736 12345678 26697112  32% /`: size and used, in KiB.
/// The file system's name may hold spaces, so the numbers are read from
/// the end.
fn df_line(text: &str) -> Option<(u64, u64)> {
    let mut back = text.split_whitespace().rev();
    let (mount, capacity, _available) = (back.next()?, back.next()?, back.next()?);
    let (used, size) = (back.next()?, back.next()?);
    if mount != "/" || !capacity.ends_with('%') || back.next().is_none() {
        return None;
    }
    let size = digits(size)?;
    let used = digits(used)?;
    Some((used.min(size), size))
}

/// Samples the load of the server `connection` is logged in to, as
/// channel `channel`, until the UI closes it or the command ends.
pub(crate) async fn run(
    channel: ChannelId,
    connection: ChannelId,
    interval: u16,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let reason = match connections.get(connection) {
        Some((conn, ..)) => {
            let reason = serve(channel, &conn, interval, &mut inbox, &out).await;
            release(conn).await;
            reason
        }
        None => "That connection is closed; connect again.".to_owned(),
    };
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

async fn serve(
    channel: ChannelId,
    conn: &Arc<Connection>,
    interval: u16,
    inbox: &mut mpsc::Receiver<Message>,
    out: &mpsc::Sender<Message>,
) -> String {
    // A server slow to answer must not keep the UI's Close waiting.
    let start = async {
        let ch = conn
            .channel_open_session()
            .await
            .map_err(|_| "The server would not open a channel for its statistics.")?;
        ch.exec(true, command(interval))
            .await
            .map_err(|_| "The server would not report its statistics.")?;
        Ok::<_, &str>(ch)
    };
    let ch = tokio::select! {
        started = start => match started {
            Ok(ch) => ch,
            Err(why) => return why.into(),
        },
        _ = closed(inbox) => return "Closed.".into(),
    };
    let (mut read, write) = ch.split();
    let mut sampler = Sampler::new();
    loop {
        tokio::select! {
            msg = read.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    let mut latest = None;
                    sampler.feed(&data, |s| latest = Some(s));
                    if let Some(stats) = latest
                        && out.send(Message::Stats { channel, stats }).await.is_err()
                    {
                        break;
                    }
                }
                Some(ChannelMsg::Failure) => {
                    return "The server would not report its statistics.".into();
                }
                Some(ChannelMsg::Close) | None => break,
                // Standard error and the exit status say nothing the
                // bar shows.
                Some(_) => {}
            },
            msg = inbox.recv() => match msg {
                None | Some(Message::Close { .. }) => {
                    let _ = write.eof().await;
                    let _ = write.close().await;
                    return "Closed.".into();
                }
                Some(_) => {}
            },
        }
    }
    "The server stopped reporting its statistics.".into()
}

/// Waits for the UI to close the channel (or go away), dropping anything
/// else it sends.
async fn closed(inbox: &mut mpsc::Receiver<Message>) {
    while let Some(msg) = inbox.recv().await {
        if matches!(msg, Message::Close { .. }) {
            return;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    const FIRST: &str = "@u\n100.00 380.00\n@c\n\
        cpu  100 0 100 700 100 0 0 0 0 0\n@m\n\
        MemTotal:        8000000 kB\nMemFree:         1000000 kB\n\
        MemAvailable:    6000000 kB\nBuffers:          100000 kB\nCached:          2000000 kB\n\
        @n\nInter-|   Receive                                                |  Transmit\n \
        face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
        lo: 5000 50 0 0 0 0 0 0 5000 50 0 0 0 0 0 0\n  \
        eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\n\
        @d\n/dev/sda1         41152736 12345678 26697112      32% /\n@e\n";

    const SECOND: &str = "@u\r\n102.00 383.00\r\n@c\r\n\
        cpu  200 0 200 1300 100 0 0 0 0 0\r\n@m\r\n\
        MemTotal:        8000000 kB\r\nMemAvailable:    5000000 kB\r\n\
        @n\r\n  eth0: 201000 10 0 0 0 0 0 0 42000 20 0 0 0 0 0 0\r\n\
        @d\r\n/dev/sda1         41152736 12345679 26697112      32% /\r\n@e\r\n";

    fn samples(text: &[u8], sampler: &mut Sampler) -> Vec<ServerStats> {
        let mut got = Vec::new();
        sampler.feed(text, |s| got.push(s));
        got
    }

    #[test]
    fn reads_a_linux_server() {
        let mut s = Sampler::new();
        let first = samples(FIRST.as_bytes(), &mut s);
        assert_eq!(
            first,
            [ServerStats {
                cpu: None,
                mem: Some((2_000_000, 8_000_000)),
                disk: Some((12_345_678, 41_152_736)),
                net: None,
            }]
        );
        // Byte by byte, with Windows line ends, gives the same.
        let mut second = Vec::new();
        for b in SECOND.bytes() {
            second.extend(samples(&[b], &mut s));
        }
        assert_eq!(
            second,
            [ServerStats {
                // 200 of 800 ticks busy.
                cpu: Some(250),
                mem: Some((3_000_000, 8_000_000)),
                disk: Some((12_345_679, 41_152_736)),
                // 200000 and 40000 bytes in two seconds.
                net: Some((100_000, 20_000)),
            }]
        );
    }

    #[test]
    fn old_kernels_without_mem_available() {
        let mut s = Sampler::new();
        let got = samples(
            b"@m\nMemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 250 kB\n@e\n",
            &mut s,
        );
        assert_eq!(got[0].mem, Some((600, 1000)));
    }

    #[test]
    fn a_server_without_proc_shows_only_its_disk() {
        let mut s = Sampler::new();
        let text =
            "@u\n@c\n@m\n@n\n@d\n/dev/disk3s1s1  482797652 10215568 230000000    5%    /\n@e\n";
        let got = samples(text.as_bytes(), &mut s);
        let again = samples(text.as_bytes(), &mut s);
        let want = ServerStats {
            disk: Some((10_215_568, 482_797_652)),
            ..ServerStats::default()
        };
        assert_eq!(got, [want]);
        assert_eq!(again, [want]);
    }

    #[test]
    fn counters_going_back_show_nothing() {
        let mut s = Sampler::new();
        samples(SECOND.as_bytes(), &mut s);
        // A reboot: uptime and counters start again.
        let got = samples(FIRST.as_bytes(), &mut s);
        assert_eq!((got[0].cpu, got[0].net), (None, None));
    }

    #[test]
    fn hostile_output_stays_in_bounds() {
        let mut s = Sampler::new();
        let mut text = Vec::new();
        text.extend_from_slice(b"@c\ncpu ");
        text.extend(std::iter::repeat_n(b'9', 10_000));
        text.extend_from_slice(b"\n@m\nMemTotal: 5 kB\nMemAvailable: 99 kB\n");
        text.extend_from_slice(b"@d\nfs 10 20 0 200% /\n@n\n");
        for _ in 0..1000 {
            text.extend_from_slice(b"x: 18446744073709551615 0 0 0 0 0 0 0 1 0\n");
        }
        text.extend_from_slice(b"@e\n");
        let got = samples(&text, &mut s);
        assert_eq!(
            got,
            [ServerStats {
                mem: Some((0, 5)),
                disk: Some((10, 10)),
                ..ServerStats::default()
            }]
        );
        assert!(s.line.capacity() <= MAX_LINE * 2);
    }

    /// What the fuzz target checks, over pieces of real output shuffled.
    #[test]
    fn shuffled_output_splits_the_same_and_stays_in_bounds() {
        let pieces: Vec<&str> = FIRST
            .split_inclusive('\n')
            .chain(SECOND.split_inclusive('\n'))
            .chain([
                "cpu 0 0 0 99999999999 0\n",
                "MemTotal: 1 kB\n",
                "@e",
                "9 /\n",
            ])
            .collect();
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..300 {
            let mut text = Vec::new();
            for _ in 0..next() % 80 {
                text.extend_from_slice(pieces[(next() % pieces.len() as u64) as usize].as_bytes());
            }
            let mut whole = Vec::new();
            Sampler::new().feed(&text, |s| whole.push(s));
            let mut chunked = Vec::new();
            let mut s = Sampler::new();
            let step = (next() % 17 + 1) as usize;
            for c in text.chunks(step) {
                s.feed(c, |x| chunked.push(x));
            }
            assert_eq!(whole, chunked);
            for x in whole {
                assert!(x.cpu.is_none_or(|c| c <= ServerStats::CPU_FULL));
                assert!(x.mem.is_none_or(|(u, t)| u <= t));
                assert!(x.disk.is_none_or(|(u, t)| u <= t));
            }
        }
    }

    #[test]
    fn parses_numbers_strictly() {
        assert_eq!(hundredths("12345.67"), Some(1_234_567));
        assert_eq!(hundredths("3.5"), Some(350));
        assert_eq!(hundredths("7"), Some(700));
        assert_eq!(hundredths("-1.00"), None);
        assert_eq!(hundredths("1.x"), None);
        assert_eq!(hundredths("184467440737095516.16"), None);
        assert_eq!(digits("+5"), None);
        assert_eq!(cpu_line("cpu0 1 2 3 4"), None);
        assert_eq!(df_line("a b c d e f"), None);
    }

    #[test]
    fn the_command_has_no_quote_to_break_out_of() {
        let c = String::from_utf8(command(0)).unwrap();
        assert_eq!(c.matches('\'').count(), 2);
        assert!(c.contains("sleep 1 "));
        assert!(
            String::from_utf8(command(u16::MAX))
                .unwrap()
                .contains("sleep 3600 ")
        );
    }
}
