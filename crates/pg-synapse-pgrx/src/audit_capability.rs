//! F2 slice 2b: proving an audit write came from an entry point.
//!
//! Once `synapse.execute` runs as its caller it can only call a function that
//! caller may execute, so the audit writers have to be granted to
//! `synapse_user`. But a role that may call `synapse.record_run` may call it
//! directly, with any payload it likes, and an audit trail a caller can write
//! to is one they can forge. That is the same objection that ruled out
//! granting the tables, arriving one level further in.
//!
//! So the grant is not the authorisation. The entry point mints an
//! unguessable token before it starts, the audit writers refuse a call that
//! does not present it, and the token is retired the moment the run ends. A
//! direct caller holds no token and is refused by the function it was just
//! granted.
//!
//! **The token lives in backend memory and must never leave it.** Not in a
//! GUC (`current_setting` would hand it to the agent's own SQL), not in a
//! trace payload, not in an error message. Agent SQL runs as the caller inside
//! this same backend while the token is live, so a leaked token is a forged
//! audit trail. Every path out of this module returns a bool, never the value.
//!
//! Per backend, not per session-global: `RefCell` in a thread-local, because
//! SPI and the executor both run on the backend thread that owns the
//! transaction. A `Vec` rather than a single slot, since `execute` can nest
//! (the delegate tool lets an agent call another agent) and the inner run must
//! not retire the outer run's token.

use std::cell::RefCell;

thread_local! {
    /// Tokens for the runs currently in flight on this backend, outermost
    /// first. Empty means no entry point is executing, so any audit write
    /// arriving now is a direct call.
    static LIVE: RefCell<Vec<u128>> = const { RefCell::new(Vec::new()) };
}

/// A capability to write one run's audit rows.
///
/// Holding one is the proof; `Drop` retires it, so an early return or a
/// `pgrx::error!` longjmp cannot leave a usable token behind. Deliberately not
/// `Clone` and deliberately without a getter: the only thing callers can do
/// with it is spend it via [`token`], which is `pub(crate)`.
#[must_use = "the token is retired when this is dropped, so it must outlive the audit write"]
pub(crate) struct AuditGrant(u128);

impl AuditGrant {
    /// Mint a token for a run about to start.
    ///
    /// Randomness comes from `uuid::Uuid::new_v4`, which is the same CSPRNG
    /// (`getrandom`) the crate already relies on for execution ids. 122 bits
    /// of entropy, and a guess is only useful within the run it is guessed
    /// during.
    pub(crate) fn mint() -> Self {
        let t = uuid::Uuid::new_v4().as_u128();
        LIVE.with(|l| l.borrow_mut().push(t));
        AuditGrant(t)
    }

    /// The token, for handing to the audit writer. `pub(crate)` so it cannot
    /// escape the extension.
    pub(crate) fn token(&self) -> u128 {
        self.0
    }
}

impl Drop for AuditGrant {
    fn drop(&mut self) {
        // Retire by value, not by popping: a nested run that outlives its
        // parent in some future refactor must not retire the wrong token.
        LIVE.with(|l| l.borrow_mut().retain(|t| *t != self.0));
    }
}

/// Whether `token` is live on this backend right now.
///
/// Returns a bool and nothing else. Comparison is over the whole set because
/// a nested run presents its own token while its parent's is still live.
pub(crate) fn is_live(token: u128) -> bool {
    LIVE.with(|l| l.borrow().contains(&token))
}

#[cfg(any(test, feature = "pg_test"))]
pub(crate) fn live_count() -> usize {
    LIVE.with(|l| l.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_token_is_live_and_a_random_one_is_not() {
        let g = AuditGrant::mint();
        assert!(is_live(g.token()));
        assert!(
            !is_live(uuid::Uuid::new_v4().as_u128()),
            "an unminted token must be refused"
        );
    }

    #[test]
    fn dropping_the_grant_retires_the_token() {
        let stolen = {
            let g = AuditGrant::mint();
            g.token()
        };
        assert!(
            !is_live(stolen),
            "a token observed during a run must be useless after it"
        );
    }

    #[test]
    fn a_nested_run_does_not_retire_the_outer_token() {
        let outer = AuditGrant::mint();
        {
            let inner = AuditGrant::mint();
            assert!(is_live(inner.token()));
            assert!(is_live(outer.token()), "both are live while nested");
        }
        assert!(
            is_live(outer.token()),
            "the inner run ending must leave the outer run able to record itself"
        );
        assert_eq!(live_count(), 1);
    }
}
