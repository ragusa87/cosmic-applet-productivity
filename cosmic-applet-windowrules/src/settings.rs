use std::collections::HashMap;

use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::{self, Alignment, Length, Size, Subscription};
use cosmic::widget::{
    Column, Row, button, container, dropdown, scrollable, text, text_input, toggler,
};
use uuid::Uuid;

use crate::config::{APP_ID, Config};
use crate::models::{Rule, WorkspaceTarget};
use crate::wayland::{
    ToplevelRef, ToplevelSnapshot, WlCommand, WlEvent, WlSender, WorkspaceRef, WorkspaceSnapshot,
    run as wl_run,
};

pub fn run() -> iced::Result {
    let settings = cosmic::app::Settings::default().size(Size::new(680.0, 600.0));
    cosmic::app::run::<SettingsApp>(settings, ())
}

#[derive(Default)]
pub struct SettingsApp {
    core: cosmic::Core,
    config: Config,
    workspaces: Vec<WorkspaceSnapshot>,
    toplevels: Vec<ToplevelSnapshot>,
    // Both label vectors start with a synthetic "pick…" entry at index 0 so
    // the dropdown always has something visible to render when nothing real
    // is selected. The real items live at index 1..=N.
    workspace_labels: Vec<String>,
    toplevel_labels: Vec<String>,
    form: Form,
    status: Option<Status>,
    sender: Option<WlSender>,
    try_results: HashMap<Uuid, TryResultEntry>,
    next_try_token: u64,
}

#[derive(Debug, Clone)]
struct TryResultEntry {
    token: u64,
    outcome: TryOutcome,
}

#[derive(Debug, Clone)]
enum TryOutcome {
    Moved { count: usize, target: String },
    NoMatch,
    NoSender,
}

const TRY_RESULT_TTL_SECS: u64 = 3;

#[derive(Debug, Clone)]
enum Status {
    Info(String),
    Error(String),
}

impl Status {
    fn info(s: impl Into<String>) -> Self {
        Self::Info(s.into())
    }
    fn error(s: impl Into<String>) -> Self {
        Self::Error(s.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormMode {
    Idle,
    Creating,
    Editing(Uuid),
}

impl FormMode {
    fn is_open(&self) -> bool {
        !matches!(self, FormMode::Idle)
    }
    fn editing_id(&self) -> Option<Uuid> {
        match self {
            FormMode::Editing(id) => Some(*id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Form {
    app_id: String,
    title_contains: String,
    workspace_idx: Option<usize>, // index into `workspaces` (real, not label-shifted)
    picked_toplevel_idx: Option<usize>, // index into `toplevels`
    switch_to_workspace: bool,
    skip_empty_title: bool,
    mode: FormMode,
}

impl Default for Form {
    fn default() -> Self {
        // skip_empty_title mirrors `Rule`'s default — see models.rs.
        Self {
            app_id: String::new(),
            title_contains: String::new(),
            workspace_idx: None,
            picked_toplevel_idx: None,
            switch_to_workspace: false,
            skip_empty_title: true,
            mode: FormMode::Idle,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    WlEvt(WlEvent),
    FormAppId(String),
    FormTitle(String),
    /// Receives the dropdown's label-index (0 = the synthetic placeholder).
    FormPickToplevel(usize),
    /// Receives the dropdown's label-index (0 = the synthetic placeholder).
    FormPickWorkspace(usize),
    FormSwitchToWorkspace(bool),
    FormSkipEmptyTitle(bool),
    StartCreate,
    SaveRule,
    EditRule(Uuid),
    CancelEdit,
    DeleteRule(Uuid),
    ToggleEnabled(Uuid),
    TryNow(Uuid),
    ClearTryResult {
        id: Uuid,
        token: u64,
    },
    OpenWorkspaceOverview,
    OverviewResult(Result<(), String>),
}

impl cosmic::Application for SettingsApp {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Msg;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut app = Self {
            core,
            config: Config::load(),
            ..Self::default()
        };
        app.refresh_labels();
        (app, Task::none())
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::run(wl_run).map(Msg::WlEvt)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let header = text::title3("COSMIC Window Rules");
        let sub = text::caption(
            "When a window matching App ID (and optionally a title substring) appears, \
             send it to the chosen workspace once.",
        );

        let form_open = self.form.mode.is_open();
        let editing_id = self.form.mode.editing_id();

        // Rules card: header (heading + Add rule button) + list / empty-state.
        let mut add_btn = button::suggested("Add rule");
        if !form_open {
            add_btn = add_btn.on_press(Msg::StartCreate);
        }
        let rules_header = Row::new()
            .align_y(Alignment::Center)
            .spacing(8)
            .push(text::heading("Rules").width(Length::Fill))
            .push(add_btn);

        let rules_body: Element<'_, Msg> = if self.config.rules.is_empty() {
            text::caption("No rules yet. Click \"Add rule\" to get started.").into()
        } else {
            let mut col = Column::new().spacing(6);
            for r in &self.config.rules {
                col = col.push(rule_row(
                    r,
                    &self.workspaces,
                    form_open,
                    editing_id,
                    self.try_results.get(&r.id).map(|e| &e.outcome),
                ));
            }
            col.into()
        };

        let rules_card = container(Column::new().spacing(8).push(rules_header).push(rules_body))
            .padding(12)
            .width(Length::Fill)
            .class(cosmic::theme::Container::Card);

        let mut root = Column::new()
            .padding(16)
            .spacing(12)
            .push(header)
            .push(sub)
            .push(rules_card)
            .push(pin_workspace_tip());

        if form_open {
            root = root.push(self.form_card());
        }

        container(scrollable(root).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Msg::WlEvt(ev) => self.on_wl(ev),
            Msg::FormAppId(s) => self.form.app_id = s,
            Msg::FormTitle(s) => self.form.title_contains = s,
            Msg::FormPickToplevel(label_idx) => {
                // label_idx 0 = synthetic placeholder
                if label_idx == 0 {
                    self.form.picked_toplevel_idx = None;
                } else {
                    let real = label_idx - 1;
                    self.form.picked_toplevel_idx = Some(real);
                    if let Some(t) = self.toplevels.get(real) {
                        self.form.app_id = t.app_id.clone();
                    }
                }
            }
            Msg::FormPickWorkspace(label_idx) => {
                if label_idx == 0 {
                    self.form.workspace_idx = None;
                } else {
                    self.form.workspace_idx = Some(label_idx - 1);
                }
            }
            Msg::FormSwitchToWorkspace(v) => self.form.switch_to_workspace = v,
            Msg::FormSkipEmptyTitle(v) => self.form.skip_empty_title = v,
            Msg::StartCreate => {
                self.form = Form::default();
                self.form.mode = FormMode::Creating;
                self.status = None;
            }
            Msg::SaveRule => self.save_rule(),
            Msg::EditRule(id) => self.start_edit(id),
            Msg::CancelEdit => {
                let was_editing = matches!(self.form.mode, FormMode::Editing(_));
                self.form = Form::default();
                self.status = Some(Status::info(if was_editing {
                    "Edit cancelled."
                } else {
                    "Cancelled."
                }));
            }
            Msg::DeleteRule(id) => self.delete_rule(id),
            Msg::ToggleEnabled(id) => self.toggle_enabled(id),
            Msg::TryNow(id) => return self.try_now(id),
            Msg::ClearTryResult { id, token } => {
                if self.try_results.get(&id).is_some_and(|e| e.token == token) {
                    self.try_results.remove(&id);
                }
            }
            Msg::OpenWorkspaceOverview => {
                return cosmic::task::future(async move {
                    let res = crate::dbus::show_workspace_overview()
                        .await
                        .map_err(|e| e.to_string());
                    Msg::OverviewResult(res)
                });
            }
            Msg::OverviewResult(Ok(())) => {
                self.status = Some(Status::info("Workspace overview opened."));
            }
            Msg::OverviewResult(Err(e)) => {
                self.status = Some(Status::error(format!("Failed to open overview: {e}")));
            }
        }
        Task::none()
    }
}

impl SettingsApp {
    fn on_wl(&mut self, ev: WlEvent) {
        match ev {
            WlEvent::Snapshot {
                workspaces,
                toplevels,
                ..
            } => {
                self.workspaces = workspaces;
                self.toplevels = toplevels;
                self.refresh_labels();
            }
            WlEvent::Ready { cmd_tx, .. } => {
                self.sender = Some(cmd_tx);
            }
            WlEvent::NewToplevel(_) => {}
        }
    }

    fn refresh_labels(&mut self) {
        let pick_count = self.toplevels.len();
        let mut tl = Vec::with_capacity(self.toplevels.len() + 1);
        tl.push(if pick_count == 0 {
            "— no open windows visible —".to_owned()
        } else {
            format!("— pick one of {pick_count} open window(s) —")
        });
        for t in &self.toplevels {
            tl.push(format!("{} — {}", t.app_id, truncate(&t.title, 48)));
        }
        self.toplevel_labels = tl;

        let ws_count = self.workspaces.len();
        let mut ws = Vec::with_capacity(self.workspaces.len() + 1);
        ws.push(if ws_count == 0 {
            "— no workspaces yet —".to_owned()
        } else {
            format!("— pick one of {ws_count} workspace(s) —")
        });
        for w in &self.workspaces {
            let name = display_ws_name(&w.name, w.index as usize);
            let mut label = match &w.output_name {
                Some(out) => format!("{name}  ({out})"),
                None => name,
            };
            if w.is_pinned {
                label.push_str("  (pinned)");
            }
            ws.push(label);
        }
        self.workspace_labels = ws;
    }

    fn save_rule(&mut self) {
        let app_id = self.form.app_id.trim().to_owned();
        if app_id.is_empty() {
            self.status = Some(Status::error("App ID is required."));
            return;
        }
        let Some(ws_idx) = self.form.workspace_idx else {
            self.status = Some(Status::error("Choose a target workspace."));
            return;
        };
        let Some(ws) = self.workspaces.get(ws_idx) else {
            self.status = Some(Status::error("Selected workspace no longer exists."));
            return;
        };
        let target = if ws.name.is_empty() {
            WorkspaceTarget::ByIndex(ws.index)
        } else {
            WorkspaceTarget::ByName(ws.name.clone())
        };
        let target_output = ws.output_name.clone();
        let title = self.form.title_contains.trim();
        let title_contains = if title.is_empty() {
            None
        } else {
            Some(title.to_owned())
        };

        // Candidate rule used purely for the uniqueness check. Its id is
        // discarded if we end up editing in place.
        let mut candidate = Rule::new(&app_id, target.clone());
        candidate.title_contains.clone_from(&title_contains);
        candidate.target_output.clone_from(&target_output);
        candidate.switch_to_workspace = self.form.switch_to_workspace;
        candidate.skip_empty_title = self.form.skip_empty_title;

        // Reject a rule that would compete with an existing one for the same
        // toplevels: same app_id + same (or both absent) title_contains.
        // When editing, the rule being edited is exempted from the check.
        let editing_id = self.form.mode.editing_id();
        if let Some(dup) = self
            .config
            .rules
            .iter()
            .find(|r| editing_id != Some(r.id) && r.matches_same_windows(&candidate))
        {
            self.status = Some(Status::error(format!(
                "A rule for {} (same title filter) already targets workspace {} — \
                 edit or delete it instead.",
                dup.app_id,
                dup.target.display()
            )));
            return;
        }

        if let Some(id) = editing_id {
            // Edit existing — preserve id, label, enabled, mode.
            if let Some(r) = self.config.rules.iter_mut().find(|r| r.id == id) {
                r.app_id = app_id;
                r.label = r.app_id.clone();
                r.title_contains = title_contains;
                r.target = target;
                r.target_output = target_output;
                r.switch_to_workspace = self.form.switch_to_workspace;
                r.skip_empty_title = self.form.skip_empty_title;
                self.try_results.remove(&id);
            } else {
                self.status = Some(Status::error("Rule no longer exists."));
                return;
            }
        } else {
            self.config.rules.push(candidate);
        }

        if let Err(e) = self.config.save() {
            self.status = Some(Status::error(format!("Save failed: {e}")));
            return;
        }
        let was_editing = editing_id.is_some();
        self.form = Form::default();
        self.status = Some(Status::info(if was_editing {
            "Rule updated."
        } else {
            "Rule added."
        }));
    }

    fn start_edit(&mut self, id: Uuid) {
        let Some(rule) = self.config.rules.iter().find(|r| r.id == id).cloned() else {
            self.status = Some(Status::error("Rule not found."));
            return;
        };
        // Find the workspace index whose snapshot matches both the rule's
        // target key AND its saved output (when present). Without the output
        // disambiguator, a rule saved against "1 on DP-4" would load with the
        // dropdown pointing at "1 on eDP-1" on a multi-monitor session.
        let workspace_idx = self.workspaces.iter().position(|w| {
            let key_ok = match &rule.target {
                WorkspaceTarget::ByName(n) => w.name == *n,
                WorkspaceTarget::ByIndex(i) => w.index == *i,
            };
            let output_ok = rule
                .target_output
                .as_deref()
                .is_none_or(|want| w.output_name.as_deref() == Some(want));
            key_ok && output_ok
        });
        self.form = Form {
            app_id: rule.app_id.clone(),
            title_contains: rule.title_contains.unwrap_or_default(),
            workspace_idx,
            picked_toplevel_idx: None,
            switch_to_workspace: rule.switch_to_workspace,
            skip_empty_title: rule.skip_empty_title,
            mode: FormMode::Editing(id),
        };
        self.status = Some(Status::info(format!("Editing rule for {}", rule.app_id)));
    }

    fn delete_rule(&mut self, id: Uuid) {
        self.config.rules.retain(|r| r.id != id);
        self.try_results.remove(&id);
        if self.form.mode.editing_id() == Some(id) {
            self.form = Form::default();
        }
        if let Err(e) = self.config.save() {
            self.status = Some(Status::error(format!("Save failed: {e}")));
        }
    }

    fn toggle_enabled(&mut self, id: Uuid) {
        if let Some(r) = self.config.rules.iter_mut().find(|r| r.id == id) {
            r.enabled = !r.enabled;
        }
        // The "try now" outcome is tied to the rule's previous enabled state
        // and target; clear it so the row doesn't show a stale caption while
        // the rule sits in its new state.
        self.try_results.remove(&id);
        if let Err(e) = self.config.save() {
            self.status = Some(Status::error(format!("Save failed: {e}")));
        }
    }

    fn try_now(&mut self, id: Uuid) -> Task<Msg> {
        let Some(rule) = self.config.rules.iter().find(|r| r.id == id).cloned() else {
            return Task::none();
        };
        let outcome = if let Some(sender) = self.sender.as_ref() {
            let target_ref = match &rule.target {
                WorkspaceTarget::ByName(n) => WorkspaceRef::Name(n.clone()),
                WorkspaceTarget::ByIndex(i) => WorkspaceRef::Index(*i),
            };
            let output = rule.target_output.clone();

            let mut count = 0usize;
            for snap in &self.toplevels {
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

            if count > 0 && rule.switch_to_workspace {
                sender.send(WlCommand::ActivateWorkspace {
                    workspace: target_ref,
                    output,
                });
            }

            if count == 0 {
                TryOutcome::NoMatch
            } else {
                TryOutcome::Moved {
                    count,
                    target: render_target(&rule, &self.workspaces),
                }
            }
        } else {
            TryOutcome::NoSender
        };

        // Bump the token so a still-pending clear-timer from a previous
        // click on this row can't wipe the fresh outcome.
        self.next_try_token = self.next_try_token.wrapping_add(1);
        let token = self.next_try_token;
        self.try_results
            .insert(id, TryResultEntry { token, outcome });

        cosmic::task::future(async move {
            tokio::time::sleep(std::time::Duration::from_secs(TRY_RESULT_TTL_SECS)).await;
            Msg::ClearTryResult { id, token }
        })
    }

    fn form_card(&self) -> Element<'_, Msg> {
        let is_editing = matches!(self.form.mode, FormMode::Editing(_));
        let heading_text = if is_editing { "Edit rule" } else { "Add rule" };
        let save_label = if is_editing {
            "Save changes"
        } else {
            "Add rule"
        };

        let header_row = Row::new()
            .align_y(Alignment::Center)
            .spacing(8)
            .push(text::heading(heading_text).width(Length::Fill))
            .push(button::standard("Cancel").on_press(Msg::CancelEdit));

        let app_id = text_input("e.g. org.mozilla.firefox", &self.form.app_id)
            .label("App ID (required, exact)")
            .on_input(Msg::FormAppId);
        let title = text_input("optional substring", &self.form.title_contains)
            .label("Title contains (optional)")
            .on_input(Msg::FormTitle);

        let pick_label_idx = self
            .form
            .picked_toplevel_idx
            .map_or(Some(0), |i| Some(i + 1));
        let ws_label_idx = self.form.workspace_idx.map_or(Some(0), |i| Some(i + 1));

        let pick = dropdown(&self.toplevel_labels, pick_label_idx, Msg::FormPickToplevel);
        let ws_pick = dropdown(&self.workspace_labels, ws_label_idx, Msg::FormPickWorkspace);

        let pick_section = labeled_picker("Pick an open window (autofills App ID)", pick.into());
        let ws_section = labeled_picker("Target workspace", ws_pick.into());

        let switch_toggle = toggler(self.form.switch_to_workspace)
            .label("Switch to the chosen workspace".to_owned())
            .on_toggle(Msg::FormSwitchToWorkspace);

        let skip_empty_toggle = toggler(self.form.skip_empty_title)
            .label("Skip windows with an empty title".to_owned())
            .on_toggle(Msg::FormSkipEmptyTitle);

        let status: Element<'_, Msg> = match &self.status {
            Some(Status::Info(s)) => text::caption(s.clone()).into(),
            Some(Status::Error(s)) => text::caption(s.clone())
                .class(cosmic::theme::Text::Custom(error_text_style))
                .into(),
            None => text::caption("").into(),
        };

        let footer = Row::new()
            .spacing(8)
            .push(button::standard("Cancel").on_press(Msg::CancelEdit))
            .push(button::suggested(save_label).on_press(Msg::SaveRule));

        let body = Column::new()
            .spacing(10)
            .push(header_row)
            .push(pick_section)
            .push(app_id)
            .push(title)
            .push(ws_section)
            .push(switch_toggle)
            .push(skip_empty_toggle)
            .push(status)
            .push(footer);

        container(body)
            .padding(12)
            .width(Length::Fill)
            .class(cosmic::theme::Container::Card)
            .into()
    }
}

// `Text::Custom` takes a function pointer (no captures), so this must be a free `fn`.
fn error_text_style(theme: &cosmic::Theme) -> cosmic::iced::widget::text::Style {
    let palette = theme.cosmic();
    cosmic::iced::widget::text::Style {
        color: Some(palette.destructive_text_color().into()),
        selected_fill: palette.accent_color().into(),
    }
}

fn labeled_picker<'a>(label: &'a str, picker: Element<'a, Msg>) -> Element<'a, Msg> {
    let inner = Column::new()
        .spacing(4)
        .push(text::caption(label))
        .push(picker);
    container(inner)
        .padding(8)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into()
}

fn pin_workspace_tip<'a>() -> Element<'a, Msg> {
    let heading = text::body("💡 Pin the target workspace");
    let body = text::caption(
        "COSMIC prunes unused workspaces dynamically. If your target workspace \
         gets pruned before the matching window appears, the rule will \
         silently do nothing. In the overview, hover the workspace thumbnail \
         and click the pin icon — pinned workspaces survive both dynamic \
         pruning and reboots.",
    );
    let open = button::standard("Open Workspaces overview").on_press(Msg::OpenWorkspaceOverview);
    let inner = Column::new().spacing(6).push(heading).push(body).push(open);
    container(inner)
        .padding(10)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into()
}

fn rule_row<'a>(
    r: &'a Rule,
    workspaces: &'a [WorkspaceSnapshot],
    form_open: bool,
    editing_id: Option<Uuid>,
    try_outcome: Option<&TryOutcome>,
) -> Element<'a, Msg> {
    let target_str = render_target(r, workspaces);
    let switch_suffix = if r.switch_to_workspace {
        "  + switch"
    } else {
        ""
    };
    let primary = match &r.title_contains {
        Some(t) => format!("{}  (title ⊇ \"{}\")", r.app_id, t),
        None => r.app_id.clone(),
    };
    let secondary = format!("→  workspace {target_str}{switch_suffix}");
    let summary = Column::new()
        .spacing(2)
        .push(text::body(primary))
        .push(text::caption(secondary))
        .width(Length::Fill);

    let enabled_toggle = toggler(r.enabled).on_toggle(move |_| Msg::ToggleEnabled(r.id));

    let is_being_edited = editing_id == Some(r.id);
    let mut edit_btn = button::standard("Edit");
    if !form_open || is_being_edited {
        edit_btn = edit_btn.on_press(Msg::EditRule(r.id));
    }
    let del_btn = button::destructive("Delete").on_press(Msg::DeleteRule(r.id));

    // Try-now: only actionable when the rule is enabled AND no form is open.
    let mut try_link = button::link("try now");
    if r.enabled && !form_open {
        try_link = try_link.on_press(Msg::TryNow(r.id));
    }

    let buttons_row = Row::new().spacing(8).push(edit_btn).push(del_btn);
    let mut actions = Column::new()
        .spacing(2)
        .align_x(Alignment::End)
        .push(buttons_row)
        .push(try_link);
    if let Some(outcome) = try_outcome {
        actions = actions.push(try_outcome_caption(outcome));
    }

    let row = Row::new()
        .align_y(Alignment::Center)
        .spacing(10)
        .push(enabled_toggle)
        .push(summary)
        .push(actions);

    let class = if is_being_edited {
        cosmic::theme::Container::Primary
    } else {
        cosmic::theme::Container::Transparent
    };
    container(row)
        .padding(8)
        .width(Length::Fill)
        .class(class)
        .into()
}

fn try_outcome_caption<'a>(outcome: &TryOutcome) -> Element<'a, Msg> {
    match outcome {
        TryOutcome::Moved { count, target } => {
            let noun = if *count == 1 { "window" } else { "windows" };
            text::caption(format!("Moved {count} {noun} to workspace {target}.")).into()
        }
        TryOutcome::NoMatch => text::caption("No matching windows.").into(),
        TryOutcome::NoSender => text::caption("Wayland not ready — try again in a moment.")
            .class(cosmic::theme::Text::Custom(error_text_style))
            .into(),
    }
}

// Prefer the rule's saved `target_output` (authoritative on multi-monitor
// setups where two workspaces can share a name); fall back to the live snapshot
// for rules persisted before `target_output` existed.
fn render_target(rule: &Rule, workspaces: &[WorkspaceSnapshot]) -> String {
    let output = rule.target_output.clone().or_else(|| {
        workspaces
            .iter()
            .find(|w| match &rule.target {
                WorkspaceTarget::ByName(n) => &w.name == n,
                WorkspaceTarget::ByIndex(i) => w.index == *i,
            })
            .and_then(|w| w.output_name.clone())
    });
    match output {
        Some(out) => format!("{} ({out})", rule.target.display()),
        None => rule.target.display(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn display_ws_name(name: &str, idx: usize) -> String {
    if name.is_empty() {
        format!("Workspace {}", idx + 1)
    } else {
        format!("Workspace {name}")
    }
}
