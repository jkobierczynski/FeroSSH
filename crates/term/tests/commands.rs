//! Shell integration (OSC 133): prompt and command marks, jumping between
//! commands, selecting a command's output, failed commands.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use ferossh_term::{Action, Settings, Terminal};

fn term(cols: usize, rows: usize) -> Terminal {
    Terminal::new(cols, rows, &Settings::new(1000))
}

/// What a bash prompt with the marks in `PS1` and `PS0` sends: the end
/// of the last command, the prompt, then the command typed and echoed.
fn run(t: &mut Terminal, exit: Option<i32>, command: &str, output: &[&str]) {
    let done = match exit {
        Some(c) => format!("\x1b]133;D;{c}\x07"),
        None => String::new(),
    };
    t.feed(format!("{done}\x1b]133;A\x07user@box:~$ \x1b]133;B\x07").as_bytes());
    // Echo of the typing, then Enter.
    t.feed(format!("{command}\r\n\x1b]133;C\x07").as_bytes());
    for line in output {
        t.feed(format!("{line}\r\n").as_bytes());
    }
}

fn finished(t: &mut Terminal) -> Vec<(Option<i32>, Option<String>)> {
    t.take_actions()
        .into_iter()
        .filter_map(|a| match a {
            Action::CommandFinished(end) => Some((end.exit, end.command)),
            _ => None,
        })
        .collect()
}

#[test]
fn finished_commands_come_out_with_their_line_and_code() {
    let mut t = term(40, 10);
    run(&mut t, None, "ls -l", &["a", "b"]);
    assert!(t.shell_integration());
    assert_eq!(finished(&mut t), []);
    let now = t.last_command().unwrap();
    assert!(now.running);
    assert_eq!(now.command.as_deref(), Some("ls -l"));
    run(&mut t, Some(2), "false", &[]);
    assert_eq!(finished(&mut t), [(Some(2), Some("ls -l".into()))]);
    t.feed(b"\x1b]133;D;1\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    assert_eq!(finished(&mut t), [(Some(1), Some("false".into()))]);
    let last = t.last_command().unwrap();
    assert!(!last.running && last.failed());
}

#[test]
fn a_terminal_without_marks_changes_nothing() {
    let mut t = term(40, 10);
    t.feed(b"$ ls\r\na\r\n$ ");
    assert!(!t.shell_integration());
    assert!(t.last_command().is_none());
    assert!(!t.select_command_output(true));
    assert!(!t.jump_to_command(true));
    assert_eq!(t.failed_rows(), Vec::<usize>::new());
}

#[test]
fn selects_the_last_output() {
    let mut t = term(40, 10);
    run(&mut t, None, "echo hi", &["hi", "there"]);
    run(&mut t, Some(0), "pwd", &["/home/user"]);
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    assert!(t.select_command_output(true));
    assert_eq!(t.selection_text().unwrap().trim_end(), "/home/user");
}

#[test]
fn selects_output_that_still_runs() {
    let mut t = term(40, 10);
    run(&mut t, None, "make", &["cc a.c", "cc b.c"]);
    assert!(t.select_command_output(true));
    assert_eq!(t.selection_text().unwrap().trim_end(), "cc a.c\ncc b.c");
}

#[test]
fn a_command_with_no_output_has_none_to_select() {
    let mut t = term(40, 10);
    run(&mut t, None, "true", &[]);
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    assert!(!t.select_command_output(true));
}

#[test]
fn output_scrolled_into_the_history_is_still_found() {
    let mut t = term(40, 5);
    let long: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
    let long: Vec<&str> = long.iter().map(String::as_str).collect();
    run(&mut t, None, "seq", &long);
    run(&mut t, Some(0), "echo x", &["x"]);
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    // Back to the first command (the second's prompt is still on the
    // screen), then select what it printed.
    assert!(t.jump_to_command(true));
    assert!(t.display_offset() > 0);
    assert!(!t.jump_to_command(true), "no command before the first");
    assert!(t.select_command_output(false));
    let text = t.selection_text().unwrap();
    assert!(text.starts_with("line 0\n"), "{text:?}");
    assert!(text.trim_end().ends_with("line 29"), "{text:?}");
    // And forward again: the next prompt is on the live screen.
    assert!(t.jump_to_command(false));
    assert_eq!(t.display_offset(), 0);
    assert!(!t.jump_to_command(false));
}

#[test]
fn jumps_put_the_prompt_at_the_top() {
    let mut t = term(40, 4);
    for i in 0..4 {
        let out = format!("out {i}");
        run(
            &mut t,
            if i == 0 { None } else { Some(0) },
            &format!("cmd {i}"),
            &[&out],
        );
    }
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    assert!(t.jump_to_command(true));
    let offset = t.display_offset();
    assert!(offset > 0);
    assert!(t.jump_to_command(true));
    assert!(t.display_offset() > offset);
    // Its output is that command's.
    assert!(t.select_command_output(false));
    let text = t.selection_text().unwrap();
    assert!(text.starts_with("out "), "{text:?}");
}

#[test]
fn failed_commands_are_marked_on_their_line() {
    let mut t = term(40, 10);
    run(&mut t, None, "ok", &["fine"]);
    run(&mut t, Some(0), "bad", &["oops"]);
    t.feed(b"\x1b]133;D;3\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    // Rows: 0 prompt+ok, 1 fine, 2 prompt+bad, 3 oops, 4 prompt.
    assert_eq!(t.failed_rows(), vec![2]);
}

#[test]
fn marks_follow_a_resize() {
    let mut t = term(40, 10);
    run(&mut t, None, "echo a", &["a"]);
    run(&mut t, Some(5), "echo b", &["b"]);
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07user@box:~$ \x1b]133;B\x07");
    t.resize(30, 6);
    t.resize(50, 12);
    assert_eq!(t.failed_rows(), vec![0]);
    assert!(t.select_command_output(true));
    assert_eq!(t.selection_text().unwrap().trim_end(), "b");
}

#[test]
fn marks_split_across_reads_still_land_in_place() {
    let mut t = term(40, 10);
    let stream = b"\x1b]133;A\x07$ \x1b]133;B\x07uname\r\n\x1b]133;C\x07Linux\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07";
    for b in stream {
        t.feed(&[*b]);
    }
    assert_eq!(finished(&mut t), [(Some(0), Some("uname".into()))]);
    assert!(t.select_command_output(true));
    assert_eq!(t.selection_text().unwrap().trim_end(), "Linux");
}

#[test]
fn a_shell_without_input_marks_still_works() {
    // Only A, C and D (some PowerShell and cmd setups).
    let mut t = term(40, 10);
    t.feed(b"\x1b]133;A\x07PS C:\\> dir\r\n\x1b]133;C\x07file\r\n\x1b]133;D;1\x07\x1b]133;A\x07PS C:\\> ");
    assert_eq!(finished(&mut t), [(Some(1), None)]);
    assert!(t.select_command_output(true));
    assert_eq!(t.selection_text().unwrap().trim_end(), "file");
    assert_eq!(t.failed_rows(), vec![0]);
}

#[test]
fn marks_do_not_show_as_links() {
    let mut t = term(40, 10);
    run(&mut t, None, "ls", &["a"]);
    for x in 0..8 {
        for y in 0..3 {
            assert_eq!(t.link_at(x, y), None);
        }
    }
}

#[test]
fn a_command_typed_over_two_rows_is_read_whole() {
    let mut t = term(20, 10);
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    // 18 columns left on the first row: the line wraps.
    t.feed(b"echo 0123456789abcdefghij\r\n\x1b]133;C\x070123456789abcdefghij\r\n\x1b]133;D;0\x07");
    assert_eq!(
        finished(&mut t),
        [(Some(0), Some("echo 0123456789abcdefghij".into()))]
    );
}

#[test]
fn a_full_screen_program_leaves_the_marks_alone() {
    let mut t = term(40, 10);
    run(&mut t, None, "vim", &[]);
    t.feed(b"\x1b[?1049h\x1b]133;A\x07fake\x1b[?1049l");
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    assert_eq!(finished(&mut t).len(), 1);
}

#[test]
fn what_bash_sends_with_the_guide_s_lines() {
    // Recorded from bash 5.2 with the lines from the user guide: `echo
    // hi`, `false`, an empty line, then `exit`.
    let p = "\x1b[?2004h\x1b]133;A\x07root@vm:~# \x1b]133;B\x07";
    let stream = format!(
        "\x1b]133;D;0\x07{p}echo hi\r\n\x1b[?2004l\r\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07\
         {p}false\r\n\x1b[?2004l\r\x1b]133;C\x07\x1b]133;D;1\x07\
         {p}\r\n\x1b[?2004l\r\x1b]133;D;1\x07{p}"
    );
    let mut t = term(40, 10);
    t.feed(stream.as_bytes());
    assert_eq!(
        finished(&mut t),
        [
            (Some(0), Some("echo hi".into())),
            (Some(1), Some("false".into()))
        ],
        "the empty line ran nothing"
    );
    // Rows: 0 echo hi, 1 hi, 2 false, 3 empty prompt, 4 prompt.
    assert_eq!(t.failed_rows(), vec![2]);
    // The last command, `false`, printed nothing to copy.
    assert!(!t.select_command_output(true));
}

#[test]
fn prompts_alone_are_enough_to_jump() {
    // cmd marks its prompts only (`PROMPT $e]133;A$e\$P$G$e]133;B$e\`).
    let mut t = term(40, 4);
    for i in 0..6 {
        t.feed(
            format!("\x1b]133;A\x1b\\C:\\>\x1b]133;B\x1b\\dir {i}\r\nfile {i}\r\n\r\n").as_bytes(),
        );
    }
    t.feed(b"\x1b]133;A\x1b\\C:\\>\x1b]133;B\x1b\\");
    assert!(t.shell_integration());
    assert!(t.jump_to_command(true));
    let one = t.display_offset();
    assert!(t.jump_to_command(true));
    assert!(t.display_offset() > one);
    // Nothing ran, as far as the marks tell: no output to select.
    assert!(!t.select_command_output(true));
    assert!(t.last_command().is_none());
}
