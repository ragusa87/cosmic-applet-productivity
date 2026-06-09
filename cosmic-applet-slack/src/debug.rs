use anyhow::{Context, Result};

use crate::slack::{self, Unread};

pub fn run() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(run_async())
}

async fn run_async() -> Result<()> {
    println!("=== cosmic-applet-slack --debug ===");
    println!();
    println!("Scanning session bus for Slack StatusNotifierItem...");
    println!();

    let report = slack::debug_scan().await.context("debug_scan")?;

    println!("Bus names enumerated: {} total ({} are connection names like :1.X)",
        report.total_names, report.connection_names);
    println!("Slack-owned connections found: {}", report.slack_candidates.len());
    println!();

    if report.slack_candidates.is_empty() {
        println!("No connection on the session bus belongs to a process named 'slack'.");
        println!("The applet would render a hidden badge until Slack starts.");
        return Ok(());
    }

    for (idx, cand) in report.slack_candidates.iter().enumerate() {
        println!("[{}] {}", idx + 1, cand.name);
        println!("    pid:        {}", cand.pid);
        println!("    /proc comm: {}", cand.comm);
        match &cand.tooltip {
            Ok(t) => {
                println!("    SNI path:   /StatusNotifierItem (reachable)");
                println!("    ToolTip raw:");
                println!("      icon_name:   {}", quote(&t.0));
                println!("      icon_count:  {}", t.1.len());
                println!("      title:       {}", quote(&t.2));
                println!("      description: {}", quote(&t.3));
                if let Some(parsed) = cand.parsed {
                    print_parse(parsed, &t.2, &t.3);
                }
            }
            Err(e) => {
                println!("    /StatusNotifierItem unreachable: {e}");
                println!("    VERDICT:    SKIP (sibling slack connection, no SNI here)");
            }
        }
        println!();
    }

    if let Some(name) = &report.chosen {
        println!("Chosen service: {name}");
        if let Some(cand) = report.slack_candidates.iter().find(|c| &c.name == name)
            && let Some(parsed) = cand.parsed
        {
            println!("Badge that would render: {}", render_badge(parsed));
        }
    } else {
        println!("No candidate exposed /StatusNotifierItem with a readable ToolTip.");
        println!("Badge would stay hidden until one becomes available.");
    }

    Ok(())
}

fn print_parse(unread: Unread, title: &str, description: &str) {
    let haystack = format!("{title} {description}");
    println!("    Parse logic on \"{}\":", haystack.trim());
    let lower = haystack.to_lowercase();
    let digit = haystack
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if !digit.is_empty() && digit.parse::<u32>().is_ok_and(|n| n > 0) {
        println!("      step 1: regex \\d+ matched '{digit}' (> 0) -> Count");
    } else {
        println!("      step 1: regex \\d+ found no positive integer");
        if lower.contains("no unread") || lower.contains("no notification") {
            println!("      step 2: text contains 'no unread' / 'no notification' -> None");
        } else if lower.contains("unread") || lower.contains("notification") {
            println!("      step 2: text contains 'unread' / 'notification' -> Indicator");
        } else {
            println!("      step 2: no keywords matched -> None");
        }
    }
    println!("    VERDICT:    {}", verdict(unread));
}

fn verdict(u: Unread) -> String {
    match u {
        Unread::None => "Unread::None (no badge)".to_owned(),
        Unread::Indicator => "Unread::Indicator (dot badge \u{2022})".to_owned(),
        Unread::Count(n) => format!("Unread::Count({n}) (badge shows {n})"),
    }
}

fn render_badge(u: Unread) -> &'static str {
    match u {
        Unread::None => "(hidden)",
        Unread::Indicator => "\u{2022}",
        Unread::Count(_) => "<integer>",
    }
}

fn quote(s: &str) -> String {
    if s.is_empty() {
        "(empty)".to_owned()
    } else {
        format!("\"{s}\"")
    }
}
