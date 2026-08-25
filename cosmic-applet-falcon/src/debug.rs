use anyhow::{Context, Result};

use crate::process;
use crate::systemd;

pub fn run() -> Result<()> {
    // zbus 5's tokio backend needs the reactor on a separate thread or
    // property reads hang — same constraint as the slack applet's debug mode.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(run_async())
}

async fn run_async() -> Result<()> {
    println!("=== cosmic-applet-falcon --debug ===");
    println!();
    println!("Unit: {}", systemd::UNIT);
    println!();

    let conn = zbus::Connection::system()
        .await
        .context("connect to system bus")?;
    match systemd::fetch_snapshot(&conn).await {
        Ok(snapshot) => {
            println!(
                "ActiveState:   {} ({})",
                snapshot.active_state.label(),
                snapshot.sub_state
            );
            println!("LoadState:     {}", snapshot.load_state);
            println!(
                "UnitFileState: {}",
                snapshot.unit_file_state.as_deref().unwrap_or("(unknown)")
            );
            println!();
            if snapshot.processes.is_empty() {
                println!("No falcon process found in /proc.");
            } else {
                println!("Falcon processes (/proc scan):");
                for p in &snapshot.processes {
                    println!("  pid {:>7}  {}", p.pid, p.comm);
                }
            }
            println!();
            let icon = if snapshot.active_state.is_running() {
                "colored (running)"
            } else {
                "monochrome (not running)"
            };
            println!("Panel icon would render: {icon}");
        }
        Err(e) => {
            println!("Snapshot failed: {e:#}");
            println!();
            println!("Raw /proc scan fallback:");
            for p in process::scan(std::path::Path::new("/proc")) {
                println!("  pid {:>7}  {}", p.pid, p.comm);
            }
        }
    }

    Ok(())
}
