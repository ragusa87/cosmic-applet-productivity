mod app;
mod config;
mod gmail;
mod settings;
mod ui;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,cosmic_applet_gmail=info")
            }),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let has_flag = |name: &str| args.iter().any(|a| a == name);

    if has_flag("--show-settings") {
        settings::run()
    } else {
        let flags = app::Flags {
            test_notify: has_flag("--test-notify"),
        };
        cosmic::applet::run::<app::AppModel>(flags)
    }
}
