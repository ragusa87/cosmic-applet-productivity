use cosmic::iced::futures::StreamExt;

use crate::wayland::{WlEvent, run as wl_run};

/// CLI debug mode: stream every wayland subscription event to stdout. Runs
/// until SIGINT or the channel closes. Mirrors what the panel applet's iced
/// subscription receives, so you can verify what `app_ids` / titles your live
/// session is emitting before writing a rule.
pub fn run() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let stream = wl_run();
        tokio::pin!(stream);
        println!("listening to wayland subscription — Ctrl-C to quit");
        while let Some(ev) = stream.next().await {
            match ev {
                WlEvent::Ready { caps, .. } => {
                    println!("[ready] caps = {caps:?}");
                }
                WlEvent::Snapshot {
                    caps: _,
                    workspaces,
                    toplevels,
                } => {
                    println!(
                        "[snapshot] {} workspace(s), {} toplevel(s)",
                        workspaces.len(),
                        toplevels.len()
                    );
                    for w in &workspaces {
                        let pin = if w.is_pinned { " [pinned]" } else { "" };
                        println!(
                            "  workspace name={:?} index={} output={:?}{}",
                            w.name, w.index, w.output_name, pin
                        );
                    }
                    for t in &toplevels {
                        println!(
                            "  toplevel  app_id={:?} title={:?} id={:?}",
                            t.app_id, t.title, t.identifier
                        );
                    }
                }
                WlEvent::NewToplevel(t) => {
                    println!(
                        "[new toplevel] app_id={:?} title={:?} id={:?}",
                        t.app_id, t.title, t.identifier
                    );
                }
            }
        }
    });
}
