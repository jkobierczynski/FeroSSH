//! Runs a session's login script, or a snippet with steps that wait:
//! waits for each `expect` text in what the host prints, types each `send`
//! line, and types passwords and named secrets that the window fetches
//! (see [`ferossh_config::login`]).
//!
//! [`Runner`] only returns [`Action`]s and keeps no clock of its own, so
//! it is tested on any host. A secret that no source has is asked for in
//! the pane, without echo, by [`Runner::prompt_input`]; the window routes
//! typing there while [`Runner::prompting`] is true, so the secret never
//! reaches the host's echo, the log or broadcast.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ferossh_config::login::{self, Cond, EXPECT_TIMEOUT_SECS, Step};
use ferossh_config::trigger::OutputText;
use zeroize::Zeroizing;

const DIM: &str = "\x1b[0;90m";
const RED: &str = "\x1b[0;31m";
const RESET: &str = "\x1b[0m";

/// Output kept for matching, after escape sequences are dropped.
const SEEN_MAX: usize = 8192;

/// The most steps a script runs in a row without waiting for the host,
/// a moment or a secret: a `goto` loop that never waits stops there
/// instead of hanging the window or flooding the host.
const MAX_BUSY_STEPS: usize = 1000;
/// The most variables a script sets, and the longest value one holds.
const MAX_VARS: usize = 64;
const MAX_VAR_LEN: usize = 256;

/// A secret the script needs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// The session's login password.
    Password,
    /// A named secret.
    Secret(String),
}

/// What the window should do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Type these bytes at the host.
    Send(Zeroizing<Vec<u8>>),
    /// Show this text in the pane (not sent, not logged).
    Show(String),
    /// Find the secret and hand it to [`Runner::secret`], or call
    /// [`Runner::ask`] to have it typed.
    Fetch(Need),
    /// Keep what was typed at the prompt for named secret `0`.
    Save(String, Zeroizing<String>),
    /// A `capture` step kept this value.
    Captured(String),
    /// The script is over (finished, stopped or failed).
    Done,
}

/// How a run ended, for the snippet report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Failed(String),
    /// The user typed into the pane.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Running the next step.
    Ready,
    Expect,
    Pause,
    Secret,
    Over,
}

/// A secret being typed in the pane.
#[derive(Debug)]
struct Prompt {
    typed: Zeroizing<String>,
    /// The name to save it under, for a named secret.
    save: Option<String>,
}

#[derive(Debug)]
pub struct Runner {
    /// What runs, for the messages: "the login script" or a snippet.
    what: String,
    steps: Vec<Step>,
    next: usize,
    wait: Wait,
    deadline: Option<Instant>,
    /// The host's text since the last `expect` matched.
    seen: String,
    text: OutputText,
    prompt: Option<Prompt>,
    outcome: Option<Outcome>,
    /// How long `expect`, `capture` and `on` wait, and where a timeout
    /// goes (`None`: the script fails).
    timeout: Duration,
    on_timeout: Option<String>,
    /// Variables set by `set`, `add` and `capture` (as `captured`).
    vars: BTreeMap<String, String>,
}

impl Runner {
    /// A session's login script.
    pub fn new(steps: Vec<Step>) -> Self {
        Self::named("the login script", steps)
    }

    /// A run of `steps` called `what` ("the snippet Backup").
    pub fn named(what: &str, steps: Vec<Step>) -> Self {
        Self {
            what: what.to_owned(),
            steps,
            next: 0,
            wait: Wait::Ready,
            deadline: None,
            seen: String::new(),
            text: OutputText::default(),
            prompt: None,
            outcome: None,
            timeout: Duration::from_secs(EXPECT_TIMEOUT_SECS),
            on_timeout: None,
            vars: BTreeMap::new(),
        }
    }

    /// `text` with each `${name}` of a variable the script set replaced
    /// by its value; other `$` text stays as it is.
    fn subst(&self, text: &str) -> String {
        if self.vars.is_empty() || !text.contains("${") {
            return text.to_owned();
        }
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("${") {
            out.push_str(rest.get(..at).unwrap_or_default());
            let after = rest.get(at + 2..).unwrap_or_default();
            match after
                .find('}')
                .and_then(|end| Some((self.vars.get(after.get(..end)?)?, end)))
            {
                Some((value, end)) => {
                    out.push_str(value);
                    rest = after.get(end + 1..).unwrap_or_default();
                }
                None => {
                    out.push_str("${");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Keeps a variable's value, one line of at most [`MAX_VAR_LEN`]
    /// characters; `false` when the script has too many.
    fn set_var(&mut self, name: &str, value: &str) -> bool {
        if !self.vars.contains_key(name) && self.vars.len() >= MAX_VARS {
            return false;
        }
        let value = value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(MAX_VAR_LEN)
            .collect();
        self.vars.insert(name.to_owned(), value);
        true
    }

    /// Where label `name` is.
    fn label(&self, name: &str) -> Option<usize> {
        self.steps
            .iter()
            .position(|s| matches!(s, Step::Label(l) if l.eq_ignore_ascii_case(name)))
    }

    /// The `on` steps from `at` on that wait together, as (text, label).
    fn on_group(&self, at: usize) -> Vec<(String, Option<String>)> {
        self.steps
            .get(at..)
            .unwrap_or_default()
            .iter()
            .map_while(|s| match s {
                Step::On(t, l) => Some((self.subst(t), l.clone())),
                _ => None,
            })
            .collect()
    }

    /// Starts the script.
    pub fn start(&mut self, now: Instant) -> Vec<Action> {
        let mut out = vec![Action::Show(format!(
            "{DIM}Running {}; any key stops it.{RESET}\r\n",
            crate::ssh::sanitize(&self.what)
        ))];
        self.advance(now, &mut out);
        out
    }

    #[cfg(test)]
    fn is_over(&self) -> bool {
        self.wait == Wait::Over
    }

    /// When [`Self::tick`] has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// How it ended, once it is over.
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// What it waits for now, for the snippet report.
    pub fn waiting_for(&self) -> Option<String> {
        match (self.wait, self.steps.get(self.next)) {
            (Wait::Expect, Some(Step::Expect(t) | Step::Capture(t))) => {
                Some(crate::ssh::sanitize(&self.subst(t)))
            }
            (Wait::Expect, Some(Step::On(..))) => Some(
                self.on_group(self.next)
                    .iter()
                    .map(|(t, _)| crate::ssh::sanitize(t))
                    .collect::<Vec<_>>()
                    .join(" or "),
            ),
            (Wait::Pause, _) => Some("a pause".into()),
            (Wait::Secret, _) => Some("a secret".into()),
            _ => None,
        }
    }

    /// Whether a secret is being typed in the pane.
    pub fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Runs steps until one has to wait.
    fn advance(&mut self, now: Instant, out: &mut Vec<Action>) {
        self.deadline = None;
        self.wait = Wait::Ready;
        let mut busy = 0;
        while let Some(step) = self.steps.get(self.next).cloned() {
            busy += 1;
            if busy > MAX_BUSY_STEPS {
                self.fail(
                    &format!(
                        "it ran {MAX_BUSY_STEPS} steps without waiting for the host; a goto \
                         loop needs an expect, on, pause or delay."
                    ),
                    out,
                );
                return;
            }
            match step {
                Step::Expect(text) => {
                    let text = self.subst(&text);
                    let Some(at) = self.seen.find(text.as_str()) else {
                        self.wait_for_host(now);
                        return;
                    };
                    self.seen.drain(..at + text.len());
                }
                Step::Capture(pattern) => {
                    let pattern = self.subst(&pattern);
                    let Some((end, value)) = login::capture(&self.seen, &pattern) else {
                        self.wait_for_host(now);
                        return;
                    };
                    self.seen.drain(..end);
                    self.set_var("captured", &value);
                    out.push(Action::Captured(value));
                }
                Step::On(..) => {
                    let group = self.on_group(self.next);
                    // The text that shows up first wins; at the same
                    // place, the one listed first.
                    let hit = group
                        .iter()
                        .enumerate()
                        .filter_map(|(i, (t, _))| Some((self.seen.find(t.as_str())?, i)))
                        .min();
                    let Some((at, i)) = hit else {
                        self.wait_for_host(now);
                        return;
                    };
                    let Some((text, label)) = group.get(i) else {
                        return;
                    };
                    self.seen.drain(..at + text.len());
                    match label {
                        Some(l) => {
                            if !self.jump(l, out) {
                                return;
                            }
                            continue;
                        }
                        None => {
                            self.next += group.len();
                            continue;
                        }
                    }
                }
                Step::Send(_) | Step::Type(_) => {
                    if let Some(bytes) = step.map_text(|t| self.subst(t)).bytes() {
                        out.push(Action::Send(Zeroizing::new(bytes)));
                    }
                }
                Step::Key(_) => {
                    if let Some(bytes) = step.bytes() {
                        out.push(Action::Send(Zeroizing::new(bytes)));
                    }
                }
                Step::Pause(secs) => {
                    self.next += 1;
                    self.wait = Wait::Pause;
                    self.deadline = Some(now + Duration::from_secs(u64::from(secs)));
                    return;
                }
                // Moves are carried out by the window, between the parts
                // of a snippet; a runner that meets one goes on.
                Step::Pane(_) | Step::Tab(_) | Step::Label(_) => {}
                Step::Delay(ms) => {
                    self.next += 1;
                    self.wait = Wait::Pause;
                    self.deadline = Some(now + Duration::from_millis(u64::from(ms)));
                    return;
                }
                Step::Password => {
                    self.wait = Wait::Secret;
                    out.push(Action::Fetch(Need::Password));
                    return;
                }
                Step::Secret(name) => {
                    self.wait = Wait::Secret;
                    out.push(Action::Fetch(Need::Secret(name)));
                    return;
                }
                Step::Goto(l) => {
                    if !self.jump(&l, out) {
                        return;
                    }
                    continue;
                }
                Step::Timeout(secs, label) => {
                    self.timeout = Duration::from_secs(u64::from(secs));
                    self.on_timeout = label;
                }
                Step::Set(name, value) => {
                    let value = self.subst(&value);
                    if !self.set_var(&name, &value) {
                        self.fail(&format!("it sets more than {MAX_VARS} variables."), out);
                        return;
                    }
                }
                Step::Add(name, n) => {
                    let old = self.vars.get(&name).map_or("0", String::as_str);
                    let Some(sum) = old
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .and_then(|v| v.checked_add(n))
                    else {
                        let why = format!(
                            "${{{name}}} is \"{}\", not a whole number to add to.",
                            crate::ssh::sanitize(old)
                        );
                        self.fail(&why, out);
                        return;
                    };
                    if !self.set_var(&name, &sum.to_string()) {
                        self.fail(&format!("it sets more than {MAX_VARS} variables."), out);
                        return;
                    }
                }
                Step::If(Cond {
                    left,
                    op,
                    right,
                    label,
                }) => match Cond::holds(op, &self.subst(&left), &self.subst(&right)) {
                    Ok(true) => {
                        if !self.jump(&label, out) {
                            return;
                        }
                        continue;
                    }
                    Ok(false) => {}
                    Err(why) => {
                        self.fail(&crate::ssh::sanitize(&why), out);
                        return;
                    }
                },
                Step::Show(text) => {
                    let text = crate::ssh::sanitize(&self.subst(&text));
                    out.push(Action::Show(format!("{DIM}{text}{RESET}\r\n")));
                }
                Step::Stop => {
                    self.next = self.steps.len();
                    break;
                }
                Step::Fail(why) => {
                    let why = crate::ssh::sanitize(&self.subst(&why));
                    self.fail(&why, out);
                    return;
                }
            }
            self.next += 1;
        }
        self.finish(out);
    }

    /// Waits for the host's text, until the timeout.
    fn wait_for_host(&mut self, now: Instant) {
        self.wait = Wait::Expect;
        self.deadline = Some(now + self.timeout);
    }

    /// Goes on at label `name`; `false` (and the script fails) when there
    /// is none, which a checked script never has.
    fn jump(&mut self, name: &str, out: &mut Vec<Action>) -> bool {
        match self.label(name) {
            Some(i) => {
                self.next = i;
                true
            }
            None => {
                let why = format!("there is no label {}.", crate::ssh::sanitize(name));
                self.fail(&why, out);
                false
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<Action>) {
        if self.outcome.is_none() {
            self.outcome = Some(Outcome::Finished);
        }
        self.wait = Wait::Over;
        self.deadline = None;
        self.prompt = None;
        out.push(Action::Done);
    }

    /// Stops with `why` shown in the pane.
    fn fail(&mut self, why: &str, out: &mut Vec<Action>) {
        let what = crate::ssh::sanitize(&self.what);
        let what = what.strip_prefix("the ").unwrap_or(&what);
        out.push(Action::Show(format!(
            "\r\n{RED}The {what} stopped: {why}{RESET}\r\n"
        )));
        self.outcome = Some(Outcome::Failed(why.to_owned()));
        self.finish(out);
    }

    /// What the host printed.
    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        if self.wait == Wait::Over {
            return Vec::new();
        }
        self.text.feed(bytes, &mut self.seen);
        if self.seen.len() > SEEN_MAX {
            let mut cut = self.seen.len() - SEEN_MAX / 2;
            while !self.seen.is_char_boundary(cut) {
                cut += 1;
            }
            self.seen.drain(..cut);
        }
        let mut out = Vec::new();
        if self.wait == Wait::Expect {
            self.advance(now, &mut out);
        }
        out
    }

    /// Acts on a passed deadline: an `expect` that timed out, or a pause
    /// that ended.
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if self.deadline.is_none_or(|d| now < d) {
            return out;
        }
        match self.wait {
            Wait::Expect => {
                if let Some(label) = self.on_timeout.clone() {
                    if self.jump(&label, &mut out) {
                        self.advance(now, &mut out);
                    }
                    return out;
                }
                let text = self.waiting_for().unwrap_or_default();
                self.fail(
                    &format!(
                        "\"{text}\" did not show up within {} seconds.",
                        self.timeout.as_secs()
                    ),
                    &mut out,
                );
            }
            Wait::Pause => self.advance(now, &mut out),
            _ => self.deadline = None,
        }
        out
    }

    /// Types the secret the window found for the step waiting on it.
    pub fn secret(&mut self, secret: &str, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if self.wait != Wait::Secret {
            return out;
        }
        let mut bytes = Zeroizing::new(secret.as_bytes().to_vec());
        bytes.push(b'\r');
        out.push(Action::Send(bytes));
        self.next += 1;
        self.advance(now, &mut out);
        out
    }

    /// The window could not get the secret: stops with `why`.
    pub fn secret_failed(&mut self, why: &str) -> Vec<Action> {
        let mut out = Vec::new();
        if self.wait == Wait::Secret {
            self.fail(why, &mut out);
        }
        out
    }

    /// Asks for the secret in the pane with `text`; a named secret typed
    /// there is kept under `save`.
    pub fn ask(&mut self, text: &str, save: Option<String>) -> Vec<Action> {
        if self.wait != Wait::Secret {
            return Vec::new();
        }
        self.prompt = Some(Prompt {
            typed: Zeroizing::new(String::new()),
            save,
        });
        vec![Action::Show(text.to_owned())]
    }

    /// Typing at the secret prompt: Enter types the secret at the host,
    /// Escape or Ctrl+C stops the script.
    pub fn prompt_input(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(prompt) = self.prompt.as_mut() else {
            return out;
        };
        match bytes {
            b"\r" | b"\n" | b"\r\n" => {
                let Some(prompt) = self.prompt.take() else {
                    return out;
                };
                out.push(Action::Show("\r\n".into()));
                if prompt.typed.is_empty() {
                    self.fail("nothing was typed.", &mut out);
                    return out;
                }
                if let Some(name) = prompt.save {
                    out.push(Action::Save(name, prompt.typed.clone()));
                }
                out.extend(self.secret(&prompt.typed, now));
            }
            [0x7f] | [0x08] => {
                prompt.typed.pop();
            }
            [0x1b] | [0x03] => {
                self.prompt = None;
                out.push(Action::Show("\r\n".into()));
                self.fail("stopped at the prompt.", &mut out);
            }
            // Other keys (arrows, function keys) are not text.
            [0x1b, ..] => {}
            _ => {
                let text = String::from_utf8_lossy(bytes);
                prompt
                    .typed
                    .extend(text.chars().filter(|c| !c.is_control()));
            }
        }
        out
    }

    /// Pasting at the secret prompt.
    pub fn prompt_paste(&mut self, text: &str) {
        if let Some(p) = self.prompt.as_mut() {
            p.typed.extend(text.chars().filter(|c| !c.is_control()));
        }
    }

    /// The user typed into the pane: the script stops.
    pub fn cancel(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        if self.wait != Wait::Over {
            let what = crate::ssh::sanitize(&self.what);
            let what = what.strip_prefix("the ").unwrap_or(&what);
            let mut what = what.to_owned();
            if let Some(first) = what.get(..1) {
                what.replace_range(..1, &first.to_uppercase());
            }
            out.push(Action::Show(format!("{DIM}[{what} stopped]{RESET}\r\n")));
            self.outcome = Some(Outcome::Stopped);
            self.finish(&mut out);
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn steps(lines: &[&str]) -> Vec<Step> {
        lines.iter().map(|l| Step::parse(l).unwrap()).collect()
    }

    fn sent(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(b) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn answers_prompts_in_order() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&[
            "expect Username:",
            "send admin",
            "expect >",
            "send enable",
            "expect Password:",
            "secret enable",
            "expect #",
            "send terminal length 0",
        ]));
        let a = r.start(now);
        assert!(sent(&a).is_empty());
        assert!(r.deadline().is_some());
        // The prompt comes in pieces, coloured.
        assert!(r.feed(b"\x1b[1mUser", now).is_empty());
        let a = r.feed(b"name:\x1b[0m ", now);
        assert_eq!(sent(&a), ["admin\r"]);
        let a = r.feed(b"admin\r\nsw1>", now);
        assert_eq!(sent(&a), ["enable\r"]);
        let a = r.feed(b"enable\r\nPassword: ", now);
        assert_eq!(a, [Action::Fetch(Need::Secret("enable".into()))]);
        let a = r.secret("s3cret", now);
        assert_eq!(sent(&a), ["s3cret\r"]);
        let a = r.feed(b"\r\nsw1#", now);
        assert_eq!(sent(&a), ["terminal length 0\r"]);
        assert_eq!(a.last(), Some(&Action::Done));
        assert!(r.is_over());
    }

    #[test]
    fn an_old_prompt_does_not_match_twice() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["expect >", "send a", "expect >", "send b"]));
        r.start(now);
        let a = r.feed(b"sw1>", now);
        assert_eq!(sent(&a), ["a\r"], "the second expect waits for a new >");
        let a = r.feed(b"a\r\nsw1>", now);
        assert_eq!(sent(&a), ["b\r"]);
    }

    #[test]
    fn expect_times_out_and_pause_waits() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["pause 2", "send x", "expect never"]));
        let a = r.start(now);
        assert!(sent(&a).is_empty());
        assert!(r.tick(now + Duration::from_secs(1)).is_empty());
        let a = r.tick(now + Duration::from_secs(2));
        assert_eq!(sent(&a), ["x\r"]);
        let later = now + Duration::from_secs(2 + EXPECT_TIMEOUT_SECS);
        let a = r.tick(later);
        assert!(matches!(&a[0], Action::Show(t) if t.contains("\"never\" did not show up")));
        assert_eq!(a.last(), Some(&Action::Done));
    }

    #[test]
    fn osc_titles_are_not_matched() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["expect host#", "send ok"]));
        r.start(now);
        assert!(sent(&r.feed(b"\x1b]0;host#\x07", now)).is_empty());
        assert!(sent(&r.feed(b"\x1b]2;host#\x1b\\", now)).is_empty());
        assert_eq!(sent(&r.feed("é host#".as_bytes(), now)), ["ok\r"]);
    }

    #[test]
    fn typed_secrets_are_saved_and_never_shown() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["secret enable", "send done"]));
        assert_eq!(
            r.start(now).last(),
            Some(&Action::Fetch(Need::Secret("enable".into())))
        );
        let a = r.ask("Secret enable: ", Some("enable".into()));
        assert!(r.prompting());
        assert_eq!(a, [Action::Show("Secret enable: ".into())]);
        for k in [&b"p"[..], b"x", b"\x7f", b"w", b"\x1b[A"] {
            assert!(r.prompt_input(k, now).is_empty(), "nothing is echoed");
        }
        r.prompt_paste("d\n");
        let a = r.prompt_input(b"\r", now);
        assert!(a.contains(&Action::Save("enable".into(), Zeroizing::new("pwd".into()))));
        assert_eq!(sent(&a), ["pwd\r", "done\r"]);
        assert!(!r.prompting());
    }

    #[test]
    fn escape_at_the_prompt_and_typing_stop_the_script() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["password", "send x"]));
        r.start(now);
        r.ask("Password: ", None);
        let a = r.prompt_input(b"\x1b", now);
        assert_eq!(a.last(), Some(&Action::Done));
        assert!(sent(&a).is_empty());

        let mut r = Runner::new(steps(&["expect $", "send x"]));
        r.start(now);
        assert_eq!(r.cancel().last(), Some(&Action::Done));
        assert!(r.feed(b"$", now).is_empty());
        assert!(r.cancel().is_empty());
    }

    #[test]
    fn a_secret_that_cannot_be_had_stops_it() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["password", "send x"]));
        r.start(now);
        let a = r.secret_failed("the command failed.");
        assert!(
            matches!(&a[0], Action::Show(t) if t.contains("login script stopped: the command failed."))
        );
        assert!(r.is_over());
        assert!(r.secret("late", now).is_empty());
    }

    #[test]
    fn snippets_type_keys_and_say_their_name() {
        let now = Instant::now();
        let mut r = Runner::named(
            "the script Save",
            steps(&["send copy run start", "expect ]?", "key enter", "type y"]),
        );
        let a = r.start(now);
        assert!(matches!(&a[0], Action::Show(t) if t.contains("Running the script Save")));
        assert_eq!(sent(&a), ["copy run start\r"]);
        let a = r.feed(b"Destination filename [startup-config]?", now);
        assert_eq!(sent(&a), ["\r", "y"]);
        assert_eq!(a.last(), Some(&Action::Done));
        let mut r = Runner::named("the script Save", steps(&["expect #"]));
        r.start(now);
        assert!(matches!(&r.cancel()[0], Action::Show(t) if t.contains("[Script Save stopped]")));
    }

    #[test]
    fn captures_report_values_and_outcomes() {
        let now = Instant::now();
        let mut r = Runner::named(
            "the script Version",
            steps(&["send show version", "capture Version *,", "expect #"]),
        );
        r.start(now);
        assert_eq!(r.waiting_for().as_deref(), Some("Version *,"));
        let a = r.feed(
            b"Cisco IOS Software, Version 15.2(4)E10, RELEASE\r\nsw1#",
            now,
        );
        assert!(a.contains(&Action::Captured("15.2(4)E10".into())));
        assert_eq!(r.outcome(), Some(&Outcome::Finished));

        let mut r = Runner::named("the script V", steps(&["capture Version *,"]));
        r.start(now);
        r.tick(now + Duration::from_secs(EXPECT_TIMEOUT_SECS));
        assert!(matches!(r.outcome(), Some(Outcome::Failed(w)) if w.contains("Version *,")));
        let mut r = Runner::named("the script V", steps(&["expect x"]));
        r.start(now);
        r.cancel();
        assert_eq!(r.outcome(), Some(&Outcome::Stopped));
    }

    fn outcome_why(r: &Runner) -> String {
        match r.outcome() {
            Some(Outcome::Failed(w)) => w.clone(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn on_waits_for_whichever_text_comes_first() {
        let now = Instant::now();
        let script = [
            "label login",
            "on (yes/no)? goto hostkey",
            "on assword: goto pw",
            "on $",
            "send whoami",
            "stop",
            "label hostkey",
            "send yes",
            "goto login",
            "label pw",
            "password",
            "goto login",
        ];
        let mut r = Runner::new(steps(&script));
        r.start(now);
        assert_eq!(
            r.waiting_for().as_deref(),
            Some("(yes/no)? or assword: or $")
        );
        let a = r.feed(b"Are you sure you want to continue (yes/no)? ", now);
        assert_eq!(sent(&a), ["yes\r"]);
        let a = r.feed(b"Password: ", now);
        assert_eq!(a, [Action::Fetch(Need::Password)]);
        let a = r.secret("pw", now);
        assert_eq!(sent(&a), ["pw\r"]);
        // Both show up: the first one in the text wins.
        let a = r.feed(b"\r\nuser@host:~$ cat Password:", now);
        assert_eq!(sent(&a), ["whoami\r"]);
        assert_eq!(r.outcome(), Some(&Outcome::Finished));
    }

    #[test]
    fn variables_counters_and_ifs() {
        let now = Instant::now();
        let mut r = Runner::named(
            "the script Retry",
            steps(&[
                "set tries 0",
                "label again",
                "add tries 1",
                "if ${tries} > 3 goto give_up",
                "send ping -c1 ${host2}",
                "timeout 5 goto again",
                "capture time=* ms",
                "if ${captured} contains . goto done",
                "label done",
                "show took ${captured} ms after ${tries} tries",
                "stop",
                "label give_up",
                "fail no answer after ${tries} tries",
            ]),
        );
        let a = r.start(now);
        assert_eq!(
            sent(&a),
            ["ping -c1 ${host2}\r"],
            "unset variables stay as written"
        );
        let t = now + Duration::from_secs(5);
        assert_eq!(sent(&r.tick(t)), ["ping -c1 ${host2}\r"]);
        let a = r.feed(b"64 bytes: time=0.42 ms\r\n", t);
        assert!(a.contains(&Action::Captured("0.42".into())));
        assert!(
            a.iter()
                .any(|x| matches!(x, Action::Show(s) if s.contains("took 0.42 ms after 2 tries")))
        );
        assert_eq!(r.outcome(), Some(&Outcome::Finished));

        let mut r = Runner::named(
            "the script Retry",
            steps(&[
                "timeout 1 goto x",
                "label x",
                "add n 1",
                "if ${n} >= 3 goto out",
                "expect never",
                "label out",
                "fail gave up at ${n}",
            ]),
        );
        r.start(now);
        r.tick(now + Duration::from_secs(1));
        r.tick(now + Duration::from_secs(2));
        assert_eq!(outcome_why(&r), "gave up at 3");
    }

    #[test]
    fn a_loop_that_never_waits_stops() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["label a", "send x", "goto a"]));
        let a = r.start(now);
        assert!(sent(&a).len() < MAX_BUSY_STEPS);
        assert!(outcome_why(&r).contains("without waiting"));
    }

    #[test]
    fn bad_numbers_and_timeouts_fail_clearly() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["set v abc", "add v 1"]));
        r.start(now);
        assert!(outcome_why(&r).contains("not a whole number"));
        let mut r = Runner::new(steps(&["set v abc", "if ${v} < 2 goto x", "label x"]));
        r.start(now);
        assert!(outcome_why(&r).contains("compares whole numbers"));
        let mut r = Runner::new(steps(&["timeout 30", "on a", "on b"]));
        r.start(now);
        assert!(r.tick(now + Duration::from_secs(29)).is_empty());
        r.tick(now + Duration::from_secs(30));
        assert!(outcome_why(&r).contains("\"a or b\" did not show up within 30 seconds"));
    }

    #[test]
    fn host_text_never_becomes_a_key_or_a_step() {
        let now = Instant::now();
        let mut r = Runner::new(steps(&["capture name=*;", "send echo ${captured}"]));
        r.start(now);
        let a = r.feed(b"name=a\x1b[2Jb;", now);
        // Escape sequences are dropped from what is matched; a control
        // character never reaches what is typed.
        assert_eq!(sent(&a), ["echo ab\r"]);
    }
}
