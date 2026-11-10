//! Drives vttest through the terminal and dumps every screen as text, so
//! the screens can be compared with what vttest says they should show.
//!
//! Unix only (it runs vttest on a pty): `cargo run -p ferossh-term
//! --example vttest -- 1 2 8 > screens.txt` runs menu entries 1, 2 and 8,
//! pressing Return after each screen until vttest is back at its menu.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    unix::run()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("This example needs a Unix pty.");
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;
    use std::io::{ErrorKind, Read, Write};
    use std::time::{Duration, Instant};

    use alacritty_terminal::event::WindowSize;
    use alacritty_terminal::tty::{self, Options, Shell};
    use ferossh_render::{Flags, Grid};
    use ferossh_term::{Action, Settings, Terminal};

    const COLS: usize = 80;
    const ROWS: usize = 24;
    /// Output is considered settled after this long without any.
    const QUIET: Duration = Duration::from_millis(400);
    const MAX_SCREENS_PER_ENTRY: usize = 60;

    struct Session {
        pty: tty::Pty,
        term: Terminal,
    }

    impl Session {
        /// Reads and processes output until it has been quiet for `QUIET`,
        /// answering the terminal's replies as they come.
        fn settle(&mut self) -> std::io::Result<()> {
            let mut buf = [0u8; 4096];
            let mut last = Instant::now();
            while last.elapsed() < QUIET {
                match self.pty.file().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        self.term.feed(buf.get(..n).unwrap_or_default());
                        for action in self.term.take_actions() {
                            if let Action::Reply(bytes) = action {
                                self.pty.file().write_all(&bytes)?;
                            }
                        }
                        last = Instant::now();
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // EIO when vttest has exited.
                    Err(_) => break,
                }
            }
            Ok(())
        }

        fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.pty.file().write_all(bytes)?;
            self.settle()
        }

        fn screen(&self) -> String {
            let mut grid = Grid::new(1, 1);
            let cursor = self.term.draw_into(&mut grid, true);
            let mut out = String::new();
            for y in 0..grid.rows() {
                let row: String = grid
                    .row(y)
                    .unwrap_or_default()
                    .iter()
                    .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
                    .map(|c| c.ch)
                    .collect();
                out.push_str(&format!("{y:2}|{}\n", row.trim_end()));
            }
            if let Some(c) = cursor {
                out.push_str(&format!("cursor {},{}\n", c.x, c.y));
            }
            out
        }
    }

    pub fn run() -> std::io::Result<()> {
        let entries: Vec<String> = std::env::args().skip(1).collect();
        let options = Options {
            shell: Some(Shell::new("vttest".to_owned(), Vec::new())),
            working_directory: None,
            drain_on_exit: false,
            env: HashMap::from([("TERM".to_owned(), "xterm-256color".to_owned())]),
        };
        let size = WindowSize {
            num_lines: ROWS as u16,
            num_cols: COLS as u16,
            cell_width: 8,
            cell_height: 16,
        };
        let pty = tty::new(&options, size, 0)?;
        let mut s = Session {
            pty,
            term: Terminal::new(COLS, ROWS, &Settings::default()),
        };
        s.settle()?;
        println!("=== main menu\n{}", s.screen());
        for entry in &entries {
            s.send(format!("{entry}\r").as_bytes())?;
            for n in 1..=MAX_SCREENS_PER_ENTRY {
                let screen = s.screen();
                println!("=== entry {entry}, screen {n}\n{screen}");
                if screen.contains("Enter choice number") && n > 1 {
                    break;
                }
                s.send(b"\r")?;
            }
        }
        s.send(b"0\r")?;
        Ok(())
    }
}
