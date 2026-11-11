//! The WS-Management (DSP0226) and Windows Remote Shell (MS-WSMV)
//! messages FeroSSH sends, and the parts of the replies it reads.

use super::xml::{self, Element, escape};

const NS: &str = concat!(
    r#"xmlns:s="http://www.w3.org/2003/05/soap-envelope" "#,
    r#"xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" "#,
    r#"xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" "#,
    r#"xmlns:p="http://schemas.microsoft.com/wbem/wsman/1/wsman.xsd" "#,
    r#"xmlns:rsp="http://schemas.microsoft.com/wbem/wsman/1/windows/shell""#
);
const SHELL_URI: &str = "http://schemas.microsoft.com/wbem/wsman/1/windows/shell/cmd";
const SHELL: &str = "http://schemas.microsoft.com/wbem/wsman/1/windows/shell";
const TRANSFER: &str = "http://schemas.xmlsoap.org/ws/2004/09/transfer";
const ANONYMOUS: &str = "http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous";
/// `CommandState` once the command has ended.
const DONE: &str = "http://schemas.microsoft.com/wbem/wsman/1/windows/shell/CommandState/Done";
/// The WSManFault code for "no output within the operation timeout".
const TIMED_OUT: u32 = 2_150_858_793;
/// The largest envelope FeroSSH asks the server to send.
pub const MAX_ENVELOPE: usize = 153_600;
/// Seconds a Receive waits for output before the server says it timed
/// out and FeroSSH asks again.
pub const RECEIVE_WAIT: u32 = 20;
/// UTF-8, so output needs no code page guessing.
const CODEPAGE: &str = "65001";

/// Ctrl+C for the running command.
pub const CTRL_C: &str = "http://schemas.microsoft.com/wbem/wsman/1/windows/shell/signal/ctrl_c";
/// Ends the command.
pub const TERMINATE: &str =
    "http://schemas.microsoft.com/wbem/wsman/1/windows/shell/signal/terminate";

/// What every request's header carries.
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    /// The endpoint, `http://host:port/wsman`.
    pub to: &'a str,
    pub message_id: &'a str,
    pub shell: Option<&'a str>,
}

fn envelope(
    h: &Header<'_>,
    action: &str,
    timeout: u32,
    options: &[(&str, &str)],
    body: &str,
) -> String {
    let mut out = format!(
        "<s:Envelope {NS}><s:Header>\
         <a:To>{to}</a:To>\
         <w:ResourceURI s:mustUnderstand=\"true\">{SHELL_URI}</w:ResourceURI>\
         <a:ReplyTo><a:Address s:mustUnderstand=\"true\">{ANONYMOUS}</a:Address></a:ReplyTo>\
         <a:Action s:mustUnderstand=\"true\">{action}</a:Action>\
         <w:MaxEnvelopeSize s:mustUnderstand=\"true\">{MAX_ENVELOPE}</w:MaxEnvelopeSize>\
         <a:MessageID>uuid:{id}</a:MessageID>\
         <w:Locale xml:lang=\"en-US\" s:mustUnderstand=\"false\"/>\
         <p:DataLocale xml:lang=\"en-US\" s:mustUnderstand=\"false\"/>\
         <w:OperationTimeout>PT{timeout}S</w:OperationTimeout>",
        to = escape(h.to),
        id = escape(h.message_id),
    );
    if let Some(shell) = h.shell {
        out.push_str(&format!(
            "<w:SelectorSet><w:Selector Name=\"ShellId\">{}</w:Selector></w:SelectorSet>",
            escape(shell)
        ));
    }
    if !options.is_empty() {
        out.push_str("<w:OptionSet>");
        for (name, value) in options {
            out.push_str(&format!(
                "<w:Option Name=\"{}\">{}</w:Option>",
                escape(name),
                escape(value)
            ));
        }
        out.push_str("</w:OptionSet>");
    }
    out.push_str("</s:Header><s:Body>");
    out.push_str(body);
    out.push_str("</s:Body></s:Envelope>");
    out
}

/// Creates the shell (`cmd`'s resource; the command runs in it).
pub fn create_shell(h: &Header<'_>) -> String {
    envelope(
        h,
        &format!("{TRANSFER}/Create"),
        60,
        &[("WINRS_NOPROFILE", "FALSE"), ("WINRS_CODEPAGE", CODEPAGE)],
        "<rsp:Shell><rsp:InputStreams>stdin</rsp:InputStreams>\
         <rsp:OutputStreams>stdout stderr</rsp:OutputStreams></rsp:Shell>",
    )
}

/// Runs `command` in the shell, its input from the console.
pub fn command(h: &Header<'_>, command: &str) -> String {
    envelope(
        h,
        &format!("{SHELL}/Command"),
        60,
        &[
            ("WINRS_CONSOLEMODE_STDIN", "TRUE"),
            ("WINRS_SKIP_CMD_SHELL", "FALSE"),
        ],
        &format!(
            "<rsp:CommandLine><rsp:Command>{}</rsp:Command></rsp:CommandLine>",
            escape(command)
        ),
    )
}

/// Sends `data` to the command's input; `end` closes it.
pub fn send(h: &Header<'_>, command_id: &str, data: &[u8], end: bool) -> String {
    let end = if end { " End=\"true\"" } else { "" };
    envelope(
        h,
        &format!("{SHELL}/Send"),
        60,
        &[],
        &format!(
            "<rsp:Send><rsp:Stream Name=\"stdin\" CommandId=\"{}\"{end}>{}</rsp:Stream></rsp:Send>",
            escape(command_id),
            super::base64_encode(data)
        ),
    )
}

/// Waits for output, up to [`RECEIVE_WAIT`] seconds.
pub fn receive(h: &Header<'_>, command_id: &str) -> String {
    envelope(
        h,
        &format!("{SHELL}/Receive"),
        RECEIVE_WAIT,
        &[("WSMAN_CMDSHELL_OPTION_KEEPALIVE", "TRUE")],
        &format!(
            "<rsp:Receive><rsp:DesiredStream CommandId=\"{}\">stdout stderr</rsp:DesiredStream></rsp:Receive>",
            escape(command_id)
        ),
    )
}

/// Sends signal `code` ([`CTRL_C`], [`TERMINATE`]) to the command.
pub fn signal(h: &Header<'_>, command_id: &str, code: &str) -> String {
    envelope(
        h,
        &format!("{SHELL}/Signal"),
        60,
        &[],
        &format!(
            "<rsp:Signal CommandId=\"{}\"><rsp:Code>{}</rsp:Code></rsp:Signal>",
            escape(command_id),
            escape(code)
        ),
    )
}

/// Deletes the shell.
pub fn delete_shell(h: &Header<'_>) -> String {
    envelope(h, &format!("{TRANSFER}/Delete"), 60, &[], "")
}

/// A SOAP fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// The WSManFault code, when there is one.
    pub code: Option<u32>,
    pub text: String,
}

impl Fault {
    /// The Receive found no output in time: ask again.
    pub fn timed_out(&self) -> bool {
        self.code == Some(TIMED_OUT)
    }
}

/// The fault in reply `root`, if it is one.
pub fn fault(root: &Element) -> Option<Fault> {
    let f = root.find("Fault")?;
    let wsman = f.find("WSManFault");
    let code = wsman
        .and_then(|w| w.attr("Code"))
        .and_then(|c| c.trim().parse::<u32>().ok());
    let mut text = wsman
        .and_then(|w| w.child("Message"))
        .map(Element::text)
        .unwrap_or_default();
    if text.is_empty() {
        text = f.find("Text").map(Element::text).unwrap_or_default();
    }
    if text.is_empty() && f.find("TimedOut").is_none() {
        // A timeout without a WSManFault says so in its subcode.
        text = "the server refused the request".into();
    }
    let timed_out_subcode = f
        .find_all("Value")
        .iter()
        .any(|v| v.text().rsplit(':').next() == Some("TimedOut"));
    Some(Fault {
        code: code.or(timed_out_subcode.then_some(TIMED_OUT)),
        text,
    })
}

/// An id the server gave (a shell's, a command's): a GUID, but any short
/// printable text is taken. Anything else is refused, since it goes back
/// into requests and into the UI's messages.
fn id(text: String) -> Option<String> {
    (!text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'{' | b'}' | b'_' | b'.')))
    .then_some(text)
}

/// The id in a Create reply.
pub fn shell_id(root: &Element) -> Option<String> {
    id(root.find("ShellId")?.text())
}

/// The id in a Command reply.
pub fn command_id(root: &Element) -> Option<String> {
    id(root.find("CommandId")?.text())
}

/// What one Receive brought.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Received {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The command ended, with this exit code.
    pub exit: Option<i64>,
}

/// The output for `command_id` in a Receive reply.
pub fn received(root: &Element, command_id: &str) -> Result<Received, &'static str> {
    let mut r = Received::default();
    for s in root.find_all("Stream") {
        if s.attr("CommandId").is_some_and(|c| c != command_id) {
            continue;
        }
        let text = s.text();
        if text.is_empty() {
            continue;
        }
        let bytes = super::base64_decode(&text).ok_or("bad base64 in output")?;
        match s.attr("Name") {
            Some("stdout") => r.stdout.extend_from_slice(&bytes),
            Some("stderr") => r.stderr.extend_from_slice(&bytes),
            _ => {}
        }
    }
    for state in root.find_all("CommandState") {
        if state.attr("CommandId").is_some_and(|c| c != command_id) {
            continue;
        }
        if state.attr("State") == Some(DONE) {
            let code = state
                .child("ExitCode")
                .and_then(|c| c.text().parse::<i64>().ok())
                .unwrap_or(0);
            r.exit = Some(code);
        }
    }
    Ok(r)
}

/// Parses a reply body.
pub fn parse(body: &[u8]) -> Result<Element, xml::Error> {
    xml::parse(body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn header() -> Header<'static> {
        Header {
            to: "http://srv:5985/wsman",
            message_id: "11111111-2222-3333-4444-555555555555",
            shell: Some("SHELL-1"),
        }
    }

    #[test]
    fn requests_are_well_formed_and_escaped() {
        let h = header();
        for req in [
            create_shell(&Header { shell: None, ..h }),
            command(&h, "powershell -c \"a<b & c\""),
            send(&h, "CMD-1", b"dir\r\n", false),
            receive(&h, "CMD-1"),
            signal(&h, "CMD-1", CTRL_C),
            delete_shell(&h),
        ] {
            let root = parse(req.as_bytes()).unwrap();
            assert_eq!(root.name, "Envelope");
            assert!(root.at(&["Header", "Action"]).is_some());
        }
        let c = parse(command(&h, "a<b").as_bytes()).unwrap();
        assert_eq!(c.find("Command").unwrap().text(), "a<b");
        let s = parse(send(&h, "CMD-1", b"dir\r\n", true).as_bytes()).unwrap();
        let stream = s.find("Stream").unwrap();
        assert_eq!(stream.attr("End"), Some("true"));
        assert_eq!(
            super::super::base64_decode(&stream.text()).unwrap(),
            b"dir\r\n"
        );
        let sel = parse(receive(&h, "C").as_bytes()).unwrap();
        assert_eq!(sel.find("Selector").unwrap().text(), "SHELL-1");
    }

    #[test]
    fn reads_ids_output_and_exit() {
        let created = br#"<s:Envelope xmlns:s="x" xmlns:rsp="y"><s:Body><rsp:Shell><rsp:ShellId>0A2B-FF</rsp:ShellId></rsp:Shell></s:Body></s:Envelope>"#;
        assert_eq!(
            shell_id(&parse(created).unwrap()).as_deref(),
            Some("0A2B-FF")
        );
        let bad = br#"<Envelope><ShellId>a"b</ShellId></Envelope>"#;
        assert_eq!(shell_id(&parse(bad).unwrap()), None);
        let got = br#"<s:Envelope xmlns:s="x" xmlns:rsp="y"><s:Body><rsp:ReceiveResponse>
            <rsp:Stream Name="stdout" CommandId="C">aGVs</rsp:Stream>
            <rsp:Stream Name="stdout" CommandId="C">bG8=</rsp:Stream>
            <rsp:Stream Name="stderr" CommandId="C">ZXJy</rsp:Stream>
            <rsp:Stream Name="stdout" CommandId="OTHER">eA==</rsp:Stream>
            <rsp:Stream Name="stdout" CommandId="C" End="true"></rsp:Stream>
            <rsp:CommandState CommandId="C" State="http://schemas.microsoft.com/wbem/wsman/1/windows/shell/CommandState/Done"><rsp:ExitCode>3</rsp:ExitCode></rsp:CommandState>
            </rsp:ReceiveResponse></s:Body></s:Envelope>"#;
        let r = received(&parse(got).unwrap(), "C").unwrap();
        assert_eq!(r.stdout, b"hello");
        assert_eq!(r.stderr, b"err");
        assert_eq!(r.exit, Some(3));
    }

    #[test]
    fn reads_faults_and_timeouts() {
        let timeout = br#"<s:Envelope xmlns:s="x" xmlns:w="w"><s:Body><s:Fault><s:Code><s:Value>s:Receiver</s:Value><s:Subcode><s:Value>w:TimedOut</s:Value></s:Subcode></s:Code><s:Reason><s:Text xml:lang="">The WS-Management service cannot complete the operation within the time specified in OperationTimeout.</s:Text></s:Reason><s:Detail><f:WSManFault xmlns:f="z" Code="2150858793" Machine="srv"><f:Message>timeout</f:Message></f:WSManFault></s:Detail></s:Fault></s:Body></s:Envelope>"#;
        let f = fault(&parse(timeout).unwrap()).unwrap();
        assert!(f.timed_out());
        assert_eq!(f.text, "timeout");
        let denied = br#"<s:Envelope xmlns:s="x"><s:Body><s:Fault><s:Reason><s:Text>Access is denied.</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>"#;
        let f = fault(&parse(denied).unwrap()).unwrap();
        assert!(!f.timed_out());
        assert_eq!(f.text, "Access is denied.");
        assert_eq!(fault(&parse(b"<a/>").unwrap()), None);
    }
}
