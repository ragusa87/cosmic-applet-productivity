mod app;
mod config;
mod debug;
mod slack;
mod ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let has_flag = |name: &str| args.iter().any(|a| a == name);
    let debug_mode = has_flag("--debug");

    let default_filter = if debug_mode {
        "warn,cosmic_applet_slack=debug"
    } else {
        "warn,cosmic_applet_slack=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .init();

    if debug_mode {
        debug::run()?;
        return Ok(());
    }

    cosmic::applet::run::<app::AppModel>(())?;
    Ok(())
}
