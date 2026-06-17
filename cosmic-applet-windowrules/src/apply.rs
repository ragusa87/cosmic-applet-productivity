use crate::models::{Rule, WorkspaceTarget};
use crate::wayland::{ToplevelRef, ToplevelSnapshot, WlCommand, WlSender, WorkspaceRef};

/// Move every toplevel matched by `rule` to the rule's target workspace.
///
/// When `allow_switch` is true and the rule has `switch_to_workspace` set,
/// the target workspace is also activated after at least one move. Callers
/// applying many rules at once (e.g. the "Apply all rules" menu item) pass
/// `false` to avoid yanking the user across workspaces.
///
/// Returns the number of toplevels that were dispatched a move command.
pub fn apply_rule(
    rule: &Rule,
    toplevels: &[ToplevelSnapshot],
    sender: &WlSender,
    allow_switch: bool,
) -> usize {
    let target_ref = match &rule.target {
        WorkspaceTarget::ByName(n) => WorkspaceRef::Name(n.clone()),
        WorkspaceTarget::ByIndex(i) => WorkspaceRef::Index(*i),
    };
    let output = rule.target_output.clone();

    let mut count = 0usize;
    for snap in toplevels {
        if !rule.matches(&snap.app_id, &snap.title) {
            continue;
        }
        sender.send(WlCommand::MoveToplevelToWorkspace {
            toplevel: ToplevelRef(snap.identifier.clone()),
            workspace: target_ref.clone(),
            output: output.clone(),
        });
        count += 1;
    }

    if count > 0 && allow_switch && rule.switch_to_workspace {
        sender.send(WlCommand::ActivateWorkspace {
            workspace: target_ref,
            output,
        });
    }

    count
}
