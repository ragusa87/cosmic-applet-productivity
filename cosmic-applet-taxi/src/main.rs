mod app;
mod atomic;
mod config;
mod export;
mod lock;
mod sessions;
mod settings;
mod state;
mod taxi;
mod ui;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,cosmic_applet_taxi=info")
            }),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--show-settings") {
        settings::run()
    } else if args.iter().any(|a| a == "--show-export") {
        export::run()
    } else if args.iter().any(|a| a == "--debug") && args.iter().any(|a| a == "--lock") {
        run_lock_debug();
        Ok(())
    } else {
        cosmic::applet::run::<app::AppModel>(())
    }
}

/// `--debug --lock`: run the real lock/suspend detection stack standalone and
/// print each edge with a timestamp. A diagnostic tool and the verification
/// harness for the detection code. Runs until Ctrl-C.
fn run_lock_debug() {
    use futures_util::StreamExt;

    let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
    rt.block_on(async {
        let mut stream = std::pin::pin!(lock::stream());
        eprintln!("watching for lock/unlock/suspend — Ctrl-C to quit");
        while let Some(ev) = stream.next().await {
            let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
            match ev {
                lock::LockEvent::Locked => println!("{now}  LOCKED"),
                lock::LockEvent::Unlocked => println!("{now}  UNLOCKED"),
            }
        }
    });
}
