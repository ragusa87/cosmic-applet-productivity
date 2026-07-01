//! Screen-lock / suspend detection.
//!
//! On COSMIC the two edges come from *different* sources:
//!
//! * **Lock** — logind D-Bus. `org.freedesktop.login1` fires `Session.Lock` on a
//!   manual lock (`Super+L`, `loginctl lock-session`) and `Manager` fires
//!   `PrepareForSleep(start=true)` on suspend. We mirror what cosmic-greeter
//!   itself uses (the `logind-zbus` crate), resolving our own session by PID.
//!
//! * **Unlock** — journald. logind never fires `Session.Unlock` on COSMIC
//!   (cosmic-greeter unlocks via the `ext-session-lock` Wayland protocol and
//!   does not round-trip back through logind), so we follow the journal instead
//!   and watch for cosmic-greeter's per-unlock marker. See [`UNLOCK_MARKER`].

use std::pin::Pin;
use std::process::Stdio;

use futures_util::stream::{self, Stream};
use futures_util::{SinkExt, StreamExt};
use logind_zbus::manager::ManagerProxy;
use logind_zbus::session::SessionProxy;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockEvent {
    Locked,
    Unlocked,
}

/// Substring cosmic-greeter logs (via the gnome-keyring PAM module) on a
/// *successful* unlock — empirically the only repeatable per-unlock marker on
/// COSMIC. Failed unlock attempts log `authentication failure` instead, so this
/// line specifically means the session was actually unlocked.
///
/// NOTE: brittle by nature. It depends on the gnome-keyring PAM module being in
/// the login stack and on cosmic-greeter's log wording; if unlock detection
/// stops working, this string (or the `journalctl` invocation below) is the
/// first thing to check.
const UNLOCK_MARKER: &str = "unlocked login keyring";

pub fn stream() -> impl cosmic::iced::futures::Stream<Item = LockEvent> {
    cosmic::iced::stream::channel(8, |sender| async move {
        run(sender).await;
    })
}

async fn run(mut sender: cosmic::iced::futures::channel::mpsc::Sender<LockEvent>) {
    let conn = match zbus::Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "no system bus; lock/suspend detection disabled");
            return;
        }
    };
    let manager = match ManagerProxy::new(&conn).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "no login1 Manager proxy; lock detection disabled");
            return;
        }
    };
    let session = match session_proxy(&conn, &manager).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "could not resolve login1 session; lock detection disabled");
            return;
        }
    };

    // Both lock sources normalise to a bare "lock edge" () so they can be
    // merged into one stream. Session.Lock is required; PrepareForSleep(start)
    // is a bonus (suspend == lock) and its absence is non-fatal.
    let lock_edges: Pin<Box<dyn Stream<Item = ()> + Send>> = match session.receive_lock().await {
        Ok(s) => Box::pin(s.map(|_| ())),
        Err(e) => {
            tracing::warn!(error = %e, "no Session.Lock signal; lock detection disabled");
            return;
        }
    };
    let sleep_edges: Pin<Box<dyn Stream<Item = ()> + Send>> =
        match manager.receive_prepare_for_sleep().await {
            Ok(s) => Box::pin(s.filter_map(|sig| async move {
                sig.args().ok().and_then(|a| a.start.then_some(()))
            })),
            Err(e) => {
                tracing::info!(error = %e, "no PrepareForSleep signal; suspend won't pause");
                Box::pin(stream::pending())
            }
        };
    let mut lock_edges = stream::select(lock_edges, sleep_edges);

    // Unlock is detected by a journald follower spawned only while locked; it
    // reports back over this channel. `follower` holds the task so we can stop
    // it (and, via `kill_on_drop`, its `journalctl` child) once unlocked.
    let (unlock_tx, mut unlock_rx) = tokio::sync::mpsc::channel::<()>(4);
    let mut follower: Option<tokio::task::JoinHandle<()>> = None;
    let mut locked = false;

    loop {
        tokio::select! {
            got = lock_edges.next() => {
                if got.is_none() { break; }
                if !locked {
                    locked = true;
                    if sender.send(LockEvent::Locked).await.is_err() { break; }
                    follower = Some(tokio::spawn(follow_journal(unlock_tx.clone())));
                }
            }
            Some(()) = unlock_rx.recv() => {
                if locked {
                    locked = false;
                    if let Some(h) = follower.take() { h.abort(); }
                    if sender.send(LockEvent::Unlocked).await.is_err() { break; }
                }
            }
        }
    }

    if let Some(h) = follower.take() {
        h.abort();
    }
}

async fn session_proxy<'a>(
    conn: &zbus::Connection,
    manager: &ManagerProxy<'a>,
) -> anyhow::Result<SessionProxy<'a>> {
    let path = manager.get_session_by_PID(std::process::id()).await?;
    let session = SessionProxy::builder(conn).path(path)?.build().await?;
    Ok(session)
}

/// Follow the journal for cosmic-greeter's unlock marker and signal once on the
/// first match, then return (the `journalctl` child is killed on drop). Spawned
/// only while the screen is locked.
async fn follow_journal(tx: tokio::sync::mpsc::Sender<()>) {
    let mut child = match Command::new("journalctl")
        .args([
            "--user",
            "-t",
            "cosmic-greeter",
            "-f",
            "-n",
            "0",
            "-o",
            "cat",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "could not spawn journalctl; unlock won't be detected");
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        tracing::warn!("journalctl produced no stdout; unlock won't be detected");
        return;
    };
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if line.contains(UNLOCK_MARKER) => {
                let _ = tx.send(()).await;
                return;
            }
            Ok(Some(_)) => {}
            Ok(None) => return, // journalctl exited
            Err(e) => {
                tracing::warn!(error = %e, "journalctl read error; unlock won't be detected");
                return;
            }
        }
    }
}
