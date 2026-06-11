mod app;
mod config;
mod debug;
mod models;
mod settings;
mod wayland;

fn main() -> cosmic::iced::Result {
    let args: Vec<String> = std::env::args().collect();
    let is_debug = args.iter().any(|a| a == "--debug");

    let default_filter = if is_debug {
        "info,cosmic_applet_windowrules=debug"
    } else {
        "warn,cosmic_applet_windowrules=info"
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .init();

    if is_debug {
        debug::run();
        return Ok(());
    }

    if args.iter().any(|a| a == "--show-settings") {
        settings::run()
    } else {
        cosmic::applet::run::<app::AppModel>(())
    }
}
