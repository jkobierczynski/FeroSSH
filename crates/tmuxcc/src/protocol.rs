//! The lines tmux writes in control mode.

/// A tmux pane, `%3`.
pub type PaneId = u32;
/// A tmux window, `@1`.
pub type WindowId = u32;
/// A tmux session, `$0`.
pub type SessionId = u32;

/// Something that happened in tmux, outside any command's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// A pane wrote `data` (already unescaped).
    Output {
        pane: PaneId,
        data: Vec<u8>,
    },
    /// A window was made in the attached session.
    WindowAdd {
        window: WindowId,
    },
    /// A window closed (or was unlinked from the session).
    WindowClose {
        window: WindowId,
    },
    WindowRenamed {
        window: WindowId,
        name: String,
    },
    /// A window's panes were split, closed or resized; `layout` is tmux's
    /// layout string.
    LayoutChange {
        window: WindowId,
        layout: String,
        /// What shows: one pane at the window's size while it is zoomed.
        visible: String,
        /// The window's flags; `Z` while it is zoomed.
        flags: String,
    },
    /// This client is now attached to another session (also sent once
    /// when control mode starts).
    SessionChanged {
        session: SessionId,
        name: String,
    },
    /// Sessions were made or killed.
    SessionsChanged,
    /// Another client attached to a session, switched sessions or
    /// detached (`%client-session-changed`, `%client-detached`): how many
    /// clients a session has may have changed.
    ClientsChanged,
    /// A window was linked into a session this client does not show
    /// (`%unlinked-window-add`).
    LinksChanged,
    /// A window closed or was unlinked, and this client's session does
    /// not have it (any more): unlinking one from this session says so.
    UnlinkedWindowClose {
        window: WindowId,
    },
    /// The active window of a session changed.
    SessionWindowChanged {
        session: SessionId,
        window: WindowId,
    },
    /// The active pane of a window changed.
    WindowPaneChanged {
        window: WindowId,
        pane: PaneId,
    },
    /// tmux paused output for a pane (flow control): what the pane
    /// wrote since is dropped, and comes back only by asking for the
    /// screen again.
    Pause {
        pane: PaneId,
    },
    /// The client is leaving control mode; `reason` as tmux gives it
    /// ("detached", "server exited").
    Exit {
        reason: String,
    },
    /// A format this client subscribed to (`refresh-client -B`) has a
    /// new value: `%subscription-changed name $s @w i %p ... : value`.
    SubscriptionChanged {
        name: String,
        value: String,
    },
    /// A line FeroSSH does not use.
    Other,
}

fn id(word: &str, sigil: char) -> Option<u32> {
    word.strip_prefix(sigil)?.parse().ok()
}

/// Reads one line from outside a reply block.
pub fn parse_notification(line: &[u8]) -> Notification {
    // The command word and ids are ASCII; output data and names are not.
    let (word, rest) = match line.iter().position(|&b| b == b' ') {
        Some(i) => (
            line.get(..i).unwrap_or_default(),
            line.get(i + 1..).unwrap_or_default(),
        ),
        None => (line, &[][..]),
    };
    let text = || String::from_utf8_lossy(rest).into_owned();
    let mut words = rest.split(|&b| b == b' ').map(String::from_utf8_lossy);
    let mut next = || words.next().map(|w| w.into_owned()).unwrap_or_default();
    let n = match word {
        b"%output" => {
            let pane = rest.iter().position(|&b| b == b' ').and_then(|i| {
                let p = std::str::from_utf8(rest.get(..i)?).ok()?;
                Some((id(p, '%')?, rest.get(i + 1..)?))
            });
            pane.map(|(pane, data)| Notification::Output {
                pane,
                data: unescape(data),
            })
        }
        // With the `pause-after` flag on, tmux sends output this way:
        // `%extended-output %3 1200 : data`, the age (milliseconds) and
        // any words a later tmux adds before the ` : `.
        b"%extended-output" => {
            let pane = rest.iter().position(|&b| b == b' ').and_then(|i| {
                let p = std::str::from_utf8(rest.get(..i)?).ok()?;
                let after = rest.get(i..)?;
                let colon = after.windows(3).position(|w| w == b" : ")?;
                Some((id(p, '%')?, after.get(colon + 3..)?))
            });
            pane.map(|(pane, data)| Notification::Output {
                pane,
                data: unescape(data),
            })
        }
        b"%window-add" => id(&next(), '@').map(|window| Notification::WindowAdd { window }),
        b"%window-close" => id(&next(), '@').map(|window| Notification::WindowClose { window }),
        b"%unlinked-window-add" => Some(Notification::LinksChanged),
        b"%unlinked-window-close" => {
            id(&next(), '@').map(|window| Notification::UnlinkedWindowClose { window })
        }
        b"%window-renamed" => {
            let window = id(&next(), '@');
            let name = text().split_once(' ').map(|(_, n)| n.to_owned());
            window
                .zip(name)
                .map(|(window, name)| Notification::WindowRenamed { window, name })
        }
        b"%layout-change" => {
            let window = id(&next(), '@');
            let layout = next();
            let visible = next();
            let flags = next();
            window.map(|window| Notification::LayoutChange {
                window,
                layout,
                visible,
                flags,
            })
        }
        b"%session-changed" => {
            let session = id(&next(), '$');
            let name = text().split_once(' ').map(|(_, n)| n.to_owned());
            session
                .zip(name)
                .map(|(session, name)| Notification::SessionChanged { session, name })
        }
        b"%sessions-changed" => Some(Notification::SessionsChanged),
        b"%client-session-changed" | b"%client-detached" => Some(Notification::ClientsChanged),
        b"%session-window-changed" => {
            let session = id(&next(), '$');
            let window = id(&next(), '@');
            session
                .zip(window)
                .map(|(session, window)| Notification::SessionWindowChanged { session, window })
        }
        b"%window-pane-changed" => {
            let window = id(&next(), '@');
            let pane = id(&next(), '%');
            window
                .zip(pane)
                .map(|(window, pane)| Notification::WindowPaneChanged { window, pane })
        }
        b"%subscription-changed" => {
            let colon = rest.windows(3).position(|w| w == b" : ");
            colon.and_then(|c| {
                Some(Notification::SubscriptionChanged {
                    name: next(),
                    value: String::from_utf8_lossy(rest.get(c + 3..)?).into_owned(),
                })
            })
        }
        b"%pause" => id(&next(), '%').map(|pane| Notification::Pause { pane }),
        b"%exit" => Some(Notification::Exit { reason: text() }),
        _ => None,
    };
    n.unwrap_or(Notification::Other)
}

/// Decodes `%output` data: tmux writes bytes below space, and the
/// backslash, as `\ooo` octal escapes.
pub fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while let Some(&b) = data.get(i) {
        if b == b'\\'
            && let Some(digits) = data.get(i + 1..i + 4)
            && digits.iter().all(|d| (b'0'..=b'7').contains(d))
        {
            let v = digits
                .iter()
                .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
            if let Ok(v) = u8::try_from(v) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// The three numbers of a `%begin`, `%end` or `%error` line: time,
/// command number and flags (1 for a command this client sent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Guard {
    pub time: u64,
    pub number: u64,
    pub flags: u32,
}

/// Which block line, if any, `line` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Boundary {
    Begin(Guard),
    End(Guard),
    Error(Guard),
}

pub(crate) fn parse_boundary(line: &[u8]) -> Option<Boundary> {
    let text = std::str::from_utf8(line).ok()?;
    let mut words = text.split(' ');
    let kind = words.next()?;
    let guard = Guard {
        time: words.next()?.parse().ok()?,
        number: words.next()?.parse().ok()?,
        flags: words.next()?.parse().ok()?,
    };
    if words.next().is_some() {
        return None;
    }
    match kind {
        "%begin" => Some(Boundary::Begin(guard)),
        "%end" => Some(Boundary::End(guard)),
        "%error" => Some(Boundary::Error(guard)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_output() {
        assert_eq!(unescape(br"a\033[0m\015\012\134"), b"a\x1b[0m\r\n\\");
        assert_eq!(unescape(br"\134\001x"), b"\\\x01x");
        // Not an escape: kept as it is.
        assert_eq!(unescape(br"\9\07\777"), br"\9\07\777");
        assert_eq!(unescape("é".as_bytes()), "é".as_bytes());
    }

    #[test]
    fn extended_output() {
        assert_eq!(
            parse_notification(br"%extended-output %7 1200 : a : b\015\012"),
            Notification::Output {
                pane: 7,
                data: b"a : b\r\n".to_vec()
            }
        );
        // Words a later tmux adds before the colon are skipped.
        assert_eq!(
            parse_notification(b"%extended-output %7 5 x y : z"),
            Notification::Output {
                pane: 7,
                data: b"z".to_vec()
            }
        );
        assert_eq!(
            parse_notification(b"%extended-output %7 0 : "),
            Notification::Output {
                pane: 7,
                data: Vec::new()
            }
        );
        assert_eq!(
            parse_notification(b"%extended-output %7 0"),
            Notification::Other
        );
    }

    #[test]
    fn notifications() {
        assert_eq!(
            parse_notification(br"%output %12 hi\015\012"),
            Notification::Output {
                pane: 12,
                data: b"hi\r\n".to_vec()
            }
        );
        assert_eq!(
            parse_notification(b"%output %1 "),
            Notification::Output {
                pane: 1,
                data: Vec::new()
            }
        );
        assert_eq!(
            parse_notification(b"%layout-change @3 b25f,80x24,0,0,1 b25f,80x24,0,0,1 *"),
            Notification::LayoutChange {
                window: 3,
                layout: "b25f,80x24,0,0,1".into(),
                visible: "b25f,80x24,0,0,1".into(),
                flags: "*".into(),
            }
        );
        assert_eq!(
            parse_notification(
                b"%layout-change @3 x,80x24,0,0{40x24,0,0,1,39x24,41,0,2} y,80x24,0,0,2 *Z"
            ),
            Notification::LayoutChange {
                window: 3,
                layout: "x,80x24,0,0{40x24,0,0,1,39x24,41,0,2}".into(),
                visible: "y,80x24,0,0,2".into(),
                flags: "*Z".into(),
            }
        );
        assert_eq!(
            parse_notification(b"%session-changed $2 my session"),
            Notification::SessionChanged {
                session: 2,
                name: "my session".into()
            }
        );
        assert_eq!(
            parse_notification(b"%window-renamed @1 vim x"),
            Notification::WindowRenamed {
                window: 1,
                name: "vim x".into()
            }
        );
        assert_eq!(
            parse_notification(b"%unlinked-window-close @4"),
            Notification::UnlinkedWindowClose { window: 4 }
        );
        assert_eq!(
            parse_notification(b"%exit"),
            Notification::Exit {
                reason: String::new()
            }
        );
        assert_eq!(
            parse_notification(b"%subscription-changed lay $0 - - - : x y : z"),
            Notification::SubscriptionChanged {
                name: "lay".into(),
                value: "x y : z".into()
            }
        );
        assert_eq!(
            parse_notification(b"%subscription-changed lay $0 - - - : "),
            Notification::SubscriptionChanged {
                name: "lay".into(),
                value: String::new()
            }
        );
        assert_eq!(
            parse_notification(b"%subscription-changed lay"),
            Notification::Other
        );
        assert_eq!(parse_notification(b"%output x1 y"), Notification::Other);
        assert_eq!(parse_notification(b"%window-add 1"), Notification::Other);
        assert_eq!(
            parse_notification(b"%client-detached c"),
            Notification::ClientsChanged
        );
        assert_eq!(
            parse_notification(b"%client-session-changed /dev/pts/3 $0 main"),
            Notification::ClientsChanged
        );
        assert_eq!(parse_notification(b""), Notification::Other);
    }

    #[test]
    fn boundaries() {
        assert_eq!(
            parse_boundary(b"%begin 1792899292 264 1"),
            Some(Boundary::Begin(Guard {
                time: 1_792_899_292,
                number: 264,
                flags: 1
            }))
        );
        assert!(matches!(
            parse_boundary(b"%error 1 2 0"),
            Some(Boundary::Error(_))
        ));
        assert_eq!(parse_boundary(b"%end 1 2"), None);
        assert_eq!(parse_boundary(b"%end 1 2 3 4"), None);
        assert_eq!(parse_boundary(b"%output 1 2 3"), None);
    }
}
