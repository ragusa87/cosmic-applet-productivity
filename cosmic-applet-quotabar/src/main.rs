mod anthropic;
mod app;
mod models;
mod openai;
mod ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let debug_mode = args.iter().any(|a| a == "--debug");

    let default_filter = if debug_mode {
        "warn,cosmic_applet_quotabar=debug"
    } else {
        "warn,cosmic_applet_quotabar=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .init();

    if debug_mode {
        return debug_dump();
    }

    cosmic::applet::run::<app::AppModel>(())?;
    Ok(())
}

fn debug_dump() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let client = anthropic::http_client()?;
        let (anth, oai) = tokio::join!(
            anthropic::fetch_snapshot(&client),
            openai::fetch_snapshot(&client),
        );
        match anth {
            Ok(s) => println!("Anthropic: {s:#?}"),
            Err(e) => println!("Anthropic ERR: {e:#}"),
        }
        match oai {
            Ok(s) => println!("OpenAI: {s:#?}"),
            Err(e) => println!("OpenAI ERR: {e:#}"),
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
