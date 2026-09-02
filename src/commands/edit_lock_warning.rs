//! What a successful edit still has to tell the caller about its lock.
//!
//! Every mutating edit takes a lock and releases it. When the release fails but
//! the operation itself succeeded, the outcome is a success with a caveat:
//! reporting a failure would misstate what happened and invite a retry, which
//! would then fail on the very lock that is stuck. The lock still blocks the
//! next edit, so somebody has to clear it.

/// The caveat for a mutation that succeeded but left its lock behind.
///
/// One wording, used by every command that can leave one, so the advice cannot
/// drift between them.
#[must_use]
pub fn still_locked_warning(still_locked: bool) -> Option<String> {
    still_locked.then(|| {
        "The change was saved, but releasing the object's lock failed, so the object is still locked. Clear the lock before editing it again; do not repeat this command, because the change already landed."
            .to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_lock_says_nothing() {
        assert_eq!(still_locked_warning(false), None);
    }

    #[test]
    fn a_stuck_lock_says_what_landed_and_what_not_to_do() {
        let warning = still_locked_warning(true).unwrap();

        // The change is saved: the caller must not be told to try again.
        assert!(warning.contains("was saved"));
        assert!(warning.contains("do not repeat this command"));
        // And the lock is the thing they now have to deal with.
        assert!(warning.contains("still locked"));
    }
}
