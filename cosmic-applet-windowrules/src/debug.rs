use cosmic::iced::futures::StreamExt;

use crate::wayland::{WlEvent, run as wl_run};

/// CLI debug mode: stream every wayland subscription event to stdout.
pub fn run() {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("failed to build tokio runtime: {e}");
            return;
        }
    };
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
                        let act = if w.is_active { " [active]" } else { "" };
                        println!(
                            "  workspace name={:?} index={} output={:?}{}{}",
                            w.name, w.index, w.output_name, pin, act
                        );
                    }
                    for t in &toplevels {
                        let on: Vec<String> = t
                            .workspaces
                            .iter()
                            .map(|w| format!("{:?}#{}", w.output_name, w.index))
                            .collect();
                        println!(
                            "  toplevel  app_id={:?} title={:?} id={:?} on=[{}]",
                            t.app_id,
                            t.title,
                            t.identifier,
                            on.join(", ")
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
