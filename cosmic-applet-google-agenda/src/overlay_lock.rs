//! Single-instance guard for the full-screen meeting overlay.
//!
//! With the COSMIC panel set to span every output (`output = All`), the
//! compositor runs one applet process *per monitor*. Each process has its own
//! state and would raise its own overlay, so a reminder pops on both screens
//! and dismissing it on one screen leaves the other one up — looking exactly
//! like the reminder "coming back".
//!
//! This guard makes a single process the sole overlay owner. It is backed by an
//! abstract-namespace Unix socket (a Linux-only address that lives in the
//! network namespace, not the filesystem): only one process can bind a given
//! name at a time, and the kernel releases it automatically when the owner
//! exits — so there is no lock file and no stale lock to clean up.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};

/// Ownership token for the overlay. Hold it for the process lifetime; dropping
/// it (or the process exiting) frees the name for another instance to claim.
pub struct OverlayLock {
    // Kept alive purely to hold the abstract address bound. Never accept()ed.
    _listener: UnixListener,
}

impl OverlayLock {
    /// Try to become the sole overlay owner for this user session. Returns
    /// `Some` if this process now holds the lock, `None` if another live process
    /// already owns it.
    pub fn try_acquire() -> Option<Self> {
        // Scope the name to the real uid: the abstract namespace is shared
        // across the whole network namespace (all users), so two users running
        // the applet on one machine must not fight over a single name.
        //
        // SAFETY: `getuid` is always successful and has no preconditions.
        let uid = unsafe { libc::getuid() };
        Self::try_acquire_named(format!("cosmic-applet-google-agenda/overlay/{uid}").as_bytes())
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
        let name = b"cosmic-applet-google-agenda/test/held";
        let held = OverlayLock::try_acquire_named(name).expect("first acquire should win");
        assert!(
            OverlayLock::try_acquire_named(name).is_none(),
            "a second acquire must fail while the first is held"
        );
        drop(held);
        assert!(
            OverlayLock::try_acquire_named(name).is_some(),
            "acquire must succeed again once the holder is dropped"
        );
    }

    #[test]
    fn distinct_names_do_not_collide() {
        let a = OverlayLock::try_acquire_named(b"cosmic-applet-google-agenda/test/n1");
        let b = OverlayLock::try_acquire_named(b"cosmic-applet-google-agenda/test/n2");
        assert!(
            a.is_some() && b.is_some(),
            "different names are independent"
        );
    }
}
