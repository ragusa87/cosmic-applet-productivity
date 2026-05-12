mod app;
mod auth;
mod calendar;
mod config;
mod debug;
mod secrets;
mod settings;
mod ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,cosmic_google_agenda_panel=info")
            }),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--debug") {
        debug::run()?;
        return Ok(());
    }
    if args.iter().any(|a| a == "--show-settings") {
        settings::run()?;
        return Ok(());
    }
    cosmic::applet::run::<app::AppModel>(())?;
    Ok(())
}
