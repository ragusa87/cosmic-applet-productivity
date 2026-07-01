//! Thin wrapper over `notify_rust` for firing desktop notifications from the
//! applets. Shared by the Gmail and Agenda applets (both GPL); the MIT-licensed
//! quotabar applet keeps its own copy to avoid depending on this crate.

/// Fire a desktop notification on a blocking thread. Best-effort: failures are
/// logged and swallowed so a missing notification daemon can't take the applet
/// down. `icon` is a freedesktop icon name (e.g. the applet's App ID).
pub fn show(summary: &str, body: &str, icon: &str) {
    let summary = summary.to_owned();
    let body = body.to_owned();
    let icon = icon.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut n = notify_rust::Notification::new();
        n.summary(&summary).body(&body).icon(&icon);
        if let Err(e) = n.show() {
            tracing::warn!(error = %e, "failed to show notification");
        }
    });
}
