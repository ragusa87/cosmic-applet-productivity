use std::collections::BTreeMap;

use cosmic::iced::futures::StreamExt;

use crate::cap::{CapOptions, CapPlanner};
use crate::cap_exceptions::{ExceptionMatcher, cap_exempt};
use crate::config::Config;
use crate::decide::{matching_rules, output_available, select_rule};
use crate::models::Rule;
use crate::wayland::{ToplevelSnapshot, WlEvent, WorkspaceSnapshot, run as wl_run};

/// CLI debug mode: stream every wayland subscription event to stdout AND replay
/// the exact decisions the running applet would make (a dry run — nothing is
/// actually moved). Two things are explained:
///
///  * Per new window, the *rule* decision — the reproduction tool for
///    "screenshot with flameshot → redirected to an empty workspace": the log
///    shows the transient window's `app_id`/`title`, which rule (if any)
///    captures it, and whether that rule would *switch* the active workspace.
///
///  * Per snapshot, the *capping* decision — the reproduction tool for the
///    experimental "cap N windows per workspace" mode: the log shows each
///    workspace's occupancy, which windows are exempt, and the exact batch of
///    moves the [`CapPlanner`] would emit to converge toward the capped layout.
///    Because nothing is dispatched, the compositor state never changes, so the
///    planner replans the same move until its in-flight TTL lapses — that's
///    expected in a dry run; it shows *what* it would do, not the full cascade.
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
        let config = Config::load();
        print_rules(&config);
        print_cap_config(&config);

        // Latest workspace snapshot, kept so the dry-run rule selection honours
        // the same "is the target monitor connected?" logic as the applet.
        let mut workspaces: Vec<WorkspaceSnapshot> = Vec::new();

        // Dry-run capping state, mirroring `AppModel`: the matcher decides which
        // windows are exempt, the planner computes the convergence moves. We
        // drive it exactly like the applet (note_new on NewToplevel, step on
        // Snapshot) but never dispatch the moves.
        let cap_matcher = ExceptionMatcher::new(&config.cap_exceptions);
        let mut cap_planner = CapPlanner::default();

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
                    workspaces: ws,
                    toplevels,
                } => {
                    println!(
                        "[snapshot] {} workspace(s), {} toplevel(s)",
                        ws.len(),
                        toplevels.len()
                    );
                    for w in &ws {
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
                    // Refresh before evaluating any subsequent NewToplevel.
                    workspaces = ws;
                    explain_cap(
                        &config,
                        &cap_matcher,
                        &mut cap_planner,
                        &workspaces,
                        &toplevels,
                    );
                }
                WlEvent::NewToplevel(t) => {
                    println!(
                        "[new toplevel] app_id={:?} title={:?} id={:?}",
                        t.app_id, t.title, t.identifier
                    );
                    explain_decision(&config, &workspaces, &t);
                    // Queue it for placement, same as the applet, so the next
                    // snapshot's cap step accounts for it.
                    cap_planner.note_new(&t.identifier);
                }
            }
        }
    });
}

fn print_rules(config: &Config) {
    println!("loaded {} rule(s) from config:", config.rules.len());
    for r in &config.rules {
        let status = if r.enabled { "enabled" } else { "disabled" };
        println!(
            "  [{status}] label={:?} app_id={:?} title_contains={:?} \
             → target={} output={:?} switch_to_workspace={} skip_empty_title={}",
            r.label,
            r.app_id,
            r.title_contains,
            r.target.display(),
            r.target_output,
            r.switch_to_workspace,
            r.skip_empty_title,
        );
    }
}

fn print_cap_config(config: &Config) {
    if !config.cap_enabled {
        println!("cap windows per workspace: DISABLED");
        return;
    }
    println!(
        "cap windows per workspace: ENABLED max_windows={} only_place_new={}",
        config.cap_max_windows.max(1),
        config.cap_only_place_new,
    );
    println!("  {} exemption rule(s):", config.cap_exceptions.len());
    for e in &config.cap_exceptions {
        println!("    - app_id={:?} title={:?}", e.appid, e.title);
    }
}

/// Log the capping decision for the current snapshot: per-workspace occupancy of
/// eligible windows, which windows are exempt, and the exact batch of moves the
/// planner would emit. Mirrors `AppModel::run_cap_step` without dispatching.
fn explain_cap(
    config: &Config,
    matcher: &ExceptionMatcher,
    planner: &mut CapPlanner,
    workspaces: &[WorkspaceSnapshot],
    toplevels: &[ToplevelSnapshot],
) {
    if !config.cap_enabled {
        return;
    }
    let opts = CapOptions {
        max_windows: config.cap_max_windows.max(1),
        only_place_new: config.cap_only_place_new,
    };

    // Same partition the applet makes: exempt windows are never counted toward a
    // workspace's cap, never moved.
    let (eligible, exempt): (Vec<ToplevelSnapshot>, Vec<ToplevelSnapshot>) = toplevels
        .iter()
        .cloned()
        .partition(|t| !cap_exempt(matcher, &t.app_id, &t.title));

    // Occupancy of eligible windows per placed workspace (output, index),
    // ordered for a stable read. Windows on 0 or >1 workspaces don't have a
    // single position to pack, so they don't count toward a workspace's load.
    let mut occ: BTreeMap<(Option<String>, u32), usize> = BTreeMap::new();
    for t in &eligible {
        if let [ws] = t.workspaces.as_slice() {
            *occ.entry((ws.output_name.clone(), ws.index)).or_default() += 1;
        }
    }
    println!(
        "    [cap] {} eligible, {} exempt window(s); occupancy (cap={}):",
        eligible.len(),
        exempt.len(),
        opts.max_windows,
    );
    for ((output, index), n) in &occ {
        let over = if *n > opts.max_windows as usize {
            "  <-- OVER CAP"
        } else {
            ""
        };
        println!("        output={output:?} workspace#{index}: {n} window(s){over}");
    }
    for t in &exempt {
        println!("        exempt: app_id={:?} title={:?}", t.app_id, t.title);
    }

    let moves = planner.step(&eligible, &exempt, workspaces, &opts);
    if moves.is_empty() {
        println!(
            "    [cap] → no move this snapshot (converged, or waiting on an in-flight batch)."
        );
    } else {
        for mv in &moves {
            println!(
                "    [cap] → WOULD MOVE id={:?} to workspace#{} output={:?}{}",
                mv.identifier,
                mv.target_index,
                mv.output,
                if mv.activate { " (+ switch to it)" } else { "" },
            );
        }
    }
}

/// Log the rule decision for a freshly created window: every rule that matches,
/// the one that would win, and what it would do to the window. Mirrors
/// `AppModel::handle_new_toplevel` without dispatching anything.
fn explain_decision(config: &Config, workspaces: &[WorkspaceSnapshot], t: &ToplevelSnapshot) {
    let competitors = matching_rules(&config.rules, &t.app_id, &t.title);
    if competitors.is_empty() {
        // Explain *why* nothing matched — the two silent filters bite here.
        let empty_title_skipped = t.title.is_empty()
            && config
                .rules
                .iter()
                .any(|r| r.enabled && r.app_id == t.app_id && r.skip_empty_title);
        if empty_title_skipped {
            println!(
                "    → no rule applies: a rule targets app_id={:?} but the window \
                 has an empty title and skip_empty_title is on (treated as a \
                 transient popup). Disable skip_empty_title on that rule to \
                 capture it.",
                t.app_id
            );
        } else {
            println!("    → no rule applies: window left where the compositor placed it.");
        }
        return;
    }

    println!("    → {} rule(s) match this window:", competitors.len());
    for r in &competitors {
        let ok = output_available(r, workspaces);
        let avail = if ok {
            "target monitor connected"
        } else {
            "target monitor NOT connected"
        };
        println!("        - {:?} → {} ({avail})", r.label, r.target.display());
    }

    let Some(selected) = select_rule(&config.rules, workspaces, &t.app_id, &t.title) else {
        return;
    };
    describe_action(selected);
}

fn describe_action(rule: &Rule) {
    println!(
        "    → WOULD MOVE to workspace {}{}",
        rule.target.display(),
        rule.target_output
            .as_deref()
            .map(|o| format!(" on output {o:?}"))
            .unwrap_or_default(),
    );
    if rule.switch_to_workspace {
        println!(
            "    → WOULD SWITCH the active workspace to {} — this is what \
             redirects you. Turn off \"switch to workspace\" on rule {:?} to \
             move the window silently.",
            rule.target.display(),
            rule.label,
        );
    }
}
