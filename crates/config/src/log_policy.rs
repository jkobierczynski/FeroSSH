//! What machine policy says about the connection log, under the same key
//! as the team library ([`crate::team_policy::KEY`]):
//!
//! - `ConnectionLogDelete` (DWORD): `0` and nobody on the PC can delete
//!   entries from the connection log in FeroSSH; `1` and everyone can.
//!   Not set: deleting is up to the user (it is allowed, after a
//!   question).
//! - `ConnectionLogEventLog` (DWORD): not `0` and every entry is also
//!   written to the Windows Application event log (source `FeroSSH`),
//!   which a user who is not an administrator cannot clear or edit, and
//!   which event forwarding or a SIEM agent can collect.
//!
//! The policy binds FeroSSH, not the files: a user can still delete
//! `connections.log` itself. The event log copy is the record the user
//! cannot remove.

/// The policy values, as the ADMX template writes them.
pub const DELETE: &str = "ConnectionLogDelete";
pub const EVENT_LOG: &str = "ConnectionLogEventLog";

/// What the machine policy says about the connection log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogPolicy {
    /// `Some(false)`: deleting entries is turned off for everyone;
    /// `Some(true)`: allowed; `None`: the policy says nothing.
    pub delete: Option<bool>,
    /// Copy each entry to the Windows event log.
    pub event_log: bool,
}

impl LogPolicy {
    /// The policy from the values read under the policy key.
    pub fn from_values(delete: Option<u32>, event_log: Option<u32>) -> Self {
        Self {
            delete: delete.map(|v| v != 0),
            event_log: event_log.is_some_and(|v| v != 0),
        }
    }

    /// Whether the user may delete entries.
    pub fn may_delete(self) -> bool {
        self.delete != Some(false)
    }

    /// Whether the policy decides deleting (the user cannot change it).
    pub fn delete_set(self) -> bool {
        self.delete.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_set_leaves_it_to_the_user() {
        let p = LogPolicy::from_values(None, None);
        assert!(p.may_delete());
        assert!(!p.delete_set());
        assert!(!p.event_log);
    }

    #[test]
    fn zero_forbids_and_one_allows() {
        let off = LogPolicy::from_values(Some(0), None);
        assert!(!off.may_delete());
        assert!(off.delete_set());
        let on = LogPolicy::from_values(Some(1), Some(1));
        assert!(on.may_delete());
        assert!(on.delete_set());
        assert!(on.event_log);
        assert!(!LogPolicy::from_values(None, Some(0)).event_log);
    }
}
