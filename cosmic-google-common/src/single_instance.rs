//! Cross-process single-instance guard, shared by the applets.
//!
//! With the COSMIC panel set to span every output (`output = All`), the
//! compositor runs one applet process *per monitor*. Each process has its own
//! state, so anything user-visible fired from a `Tick` — a desktop notification,
//! a full-screen overlay — happens once per screen. This guard lets a single
//! process claim a named exclusive role so the action happens exactly once.
//!
//! It is backed by an abstract-namespace Unix socket (a Linux-only address that
//! lives in the network namespace, not the filesystem): only one process can
//! bind a given name at a time, and the kernel releases it automatically when
//! the owner exits — so there is no lock file and no stale lock to clean up.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};

/// Ownership token for a named role. Hold it for as long as the exclusivity
/// should last; dropping it (or the process exiting) frees the name for another
/// instance to claim.
pub struct InstanceLock {
    // Kept alive purely to hold the abstract address bound. Never accept()ed.
    _listener: UnixListener,
}

impl InstanceLock {
    /// Try to claim `key` for this user session. Returns `Some` if this process
    /// now holds it, `None` if another live process already owns it.
    ///
    /// `key` names the role (e.g. `"gmail-notify"`); pick a distinct key per
    /// role so unrelated locks don't contend. The bound name is scoped to the
    /// real uid, since the abstract namespace is shared across the whole network
    /// namespace (all users) — two users running the applet on one machine must
    /// not fight over a single name.
    pub fn try_acquire(key: &str) -> Option<Self> {
        // SAFETY: `getuid` is always successful and has no preconditions.
        let uid = unsafe { libc::getuid() };
        Self::try_acquire_named(format!("cosmic-google-common/{key}/{uid}").as_bytes())
    }

    /// Acquire a lock under an explicit abstract name. Split out so tests can use
    /// unique names without colliding with a running applet or with each other.
    fn try_acquire_named(name: &[u8]) -> Option<Self> {
        let addr = SocketAddr::from_abstract_name(name).ok()?;
        UnixListener::bind_addr(&addr).ok().map(|listener| Self {
            _listener: listener,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_held() {
        let name = b"cosmic-google-common/test/held";
        let held = InstanceLock::try_acquire_named(name).expect("first acquire should win");
        assert!(
            InstanceLock::try_acquire_named(name).is_none(),
            "a second acquire must fail while the first is held"
        );
        drop(held);
        assert!(
            InstanceLock::try_acquire_named(name).is_some(),
            "acquire must succeed again once the holder is dropped"
        );
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let a = InstanceLock::try_acquire_named(b"cosmic-google-common/test/k1");
        let b = InstanceLock::try_acquire_named(b"cosmic-google-common/test/k2");
        assert!(
            a.is_some() && b.is_some(),
            "different names are independent"
        );
    }
}
