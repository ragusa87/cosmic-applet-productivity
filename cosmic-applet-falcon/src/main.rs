mod app;
mod debug;
mod process;
mod systemd;
mod ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let debug_mode = args.iter().any(|a| a == "--debug");

    let default_filter = if debug_mode {
        "warn,cosmic_applet_falcon=debug"
    } else {
        "warn,cosmic_applet_falcon=info"
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
