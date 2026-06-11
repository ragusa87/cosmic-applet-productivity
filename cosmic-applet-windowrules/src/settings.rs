use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::{self, Alignment, Length, Size, Subscription};
use cosmic::widget::{
    Column, Row, button, container, dropdown, scrollable, text, text_input, toggler,
};
use uuid::Uuid;

use crate::config::{APP_ID, Config};
use crate::models::{Rule, WorkspaceTarget};
use crate::wayland::{ToplevelSnapshot, WlEvent, WorkspaceSnapshot, run as wl_run};

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
}

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

#[derive(Debug, Clone)]
struct Form {
    app_id: String,
    title_contains: String,
    workspace_idx: Option<usize>, // index into `workspaces` (real, not label-shifted)
    picked_toplevel_idx: Option<usize>, // index into `toplevels`
    switch_to_workspace: bool,
    skip_empty_title: bool,
    editing: Option<Uuid>,
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
            editing: None,
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
    SaveRule,
    EditRule(Uuid),
    CancelEdit,
    DeleteRule(Uuid),
    ToggleEnabled(Uuid),
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

        let mut rules_col = Column::new().spacing(6);
        if self.config.rules.is_empty() {
            rules_col = rules_col.push(text::caption("No rules yet — use the form below."));
        }
        for r in &self.config.rules {
            rules_col = rules_col.push(rule_row(r, &self.workspaces));
        }

        let app_id = text_input("e.g. org.mozilla.firefox", &self.form.app_id)
            .label("App ID (required, exact)")
            .on_input(Msg::FormAppId);
        let title = text_input("optional substring", &self.form.title_contains)
            .label("Title contains (optional)")
            .on_input(Msg::FormTitle);

        // Convert real toplevel index → label-index by adding 1 (slot 0 = placeholder).
        let pick_label_idx = self
            .form
            .picked_toplevel_idx
            .map_or(Some(0), |i| Some(i + 1));
        let ws_label_idx = self.form.workspace_idx.map_or(Some(0), |i| Some(i + 1));

        let pick = dropdown(&self.toplevel_labels, pick_label_idx, Msg::FormPickToplevel);
        let ws_pick = dropdown(&self.workspace_labels, ws_label_idx, Msg::FormPickWorkspace);

        // Make each picker visually obvious by wrapping it in a labeled
        // container with padding so the closed dropdown doesn't disappear
        // into the surrounding layout.
        let pick_section = labeled_picker("Pick an open window (autofills App ID)", pick.into());
        let ws_section = labeled_picker("Target workspace", ws_pick.into());

        let switch_toggle = toggler(self.form.switch_to_workspace)
            .label("Switch to the chosen workspace".to_owned())
            .on_toggle(Msg::FormSwitchToWorkspace);

        let skip_empty_toggle = toggler(self.form.skip_empty_title)
            .label("Skip windows with an empty title".to_owned())
            .on_toggle(Msg::FormSkipEmptyTitle);

        let pin_tip = pin_workspace_tip();

        let is_editing = self.form.editing.is_some();
        let save_label = if is_editing {
            "Save changes"
        } else {
            "Add rule"
        };
        let mut actions = Row::new().spacing(8);
        if is_editing {
            actions = actions.push(button::standard("Cancel edit").on_press(Msg::CancelEdit));
        }
        actions = actions.push(button::suggested(save_label).on_press(Msg::SaveRule));

        let status: Element<'_, Msg> = match &self.status {
            Some(Status::Info(s)) => text::caption(s.clone()).into(),
            Some(Status::Error(s)) => text::caption(s.clone())
                .class(cosmic::theme::Text::Custom(error_text_style))
                .into(),
            None => text::caption("").into(),
        };

        let form_heading = if is_editing {
            text::heading("Edit rule")
        } else {
            text::heading("Add a rule")
        };

        let col = Column::new()
            .padding(16)
            .spacing(12)
            .push(header)
            .push(sub)
            .push(text::heading("Rules"))
            .push(rules_col)
            .push(form_heading)
            .push(pick_section)
            .push(app_id)
            .push(title)
            .push(ws_section)
            .push(switch_toggle)
            .push(skip_empty_toggle)
            .push(actions)
            .push(status)
            .push(pin_tip);

        container(scrollable(col).height(Length::Fill))
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
            Msg::SaveRule => self.save_rule(),
            Msg::EditRule(id) => self.start_edit(id),
            Msg::CancelEdit => {
                self.form = Form::default();
                self.status = Some(Status::info("Edit cancelled."));
            }
            Msg::DeleteRule(id) => self.delete_rule(id),
            Msg::ToggleEnabled(id) => self.toggle_enabled(id),
            Msg::OpenWorkspaceOverview => {
                return cosmic::task::future(async move {
                    let res = call_workspaces_show().await.map_err(|e| e.to_string());
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

async fn call_workspaces_show() -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(
        Some("com.system76.CosmicWorkspaces"),
        "/com/system76/CosmicWorkspaces",
        Some("com.system76.CosmicWorkspaces"),
        "Show",
        &(),
    )
    .await?;
    Ok(())
}

impl SettingsApp {
    fn on_wl(&mut self, ev: WlEvent) {
        match ev {
            WlEvent::Ready { .. } => {}
            WlEvent::Snapshot {
                workspaces,
                toplevels,
                ..
            } => {
                self.workspaces = workspaces;
                self.toplevels = toplevels;
                self.refresh_labels();
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
        for (idx, w) in self.workspaces.iter().enumerate() {
            let name = display_ws_name(&w.name, idx);
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
        candidate.title_contains = title_contains.clone();
        candidate.target_output = target_output.clone();
        candidate.switch_to_workspace = self.form.switch_to_workspace;
        candidate.skip_empty_title = self.form.skip_empty_title;

        // Reject a rule that would compete with an existing one for the same
        // toplevels: same app_id + same (or both absent) title_contains.
        // When editing, the rule being edited is exempted from the check.
        if let Some(dup) = self
            .config
            .rules
            .iter()
            .find(|r| self.form.editing != Some(r.id) && r.matches_same_windows(&candidate))
        {
            self.status = Some(Status::error(format!(
                "A rule for {} (same title filter) already targets workspace {} — \
                 edit or delete it instead.",
                dup.app_id,
                dup.target.display()
            )));
            return;
        }

        if let Some(id) = self.form.editing {
            // Edit existing — preserve id, label, enabled, mode.
            if let Some(r) = self.config.rules.iter_mut().find(|r| r.id == id) {
                r.app_id = app_id;
                r.label = r.app_id.clone();
                r.title_contains = title_contains;
                r.target = target;
                r.target_output = target_output;
                r.switch_to_workspace = self.form.switch_to_workspace;
                r.skip_empty_title = self.form.skip_empty_title;
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
        let was_editing = self.form.editing.is_some();
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
            editing: Some(id),
        };
        self.status = Some(Status::info(format!("Editing rule for {}", rule.app_id)));
    }

    fn delete_rule(&mut self, id: Uuid) {
        self.config.rules.retain(|r| r.id != id);
        if self.form.editing == Some(id) {
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
        if let Err(e) = self.config.save() {
            self.status = Some(Status::error(format!("Save failed: {e}")));
        }
    }
}

/// Style function used by `cosmic::theme::Text::Custom` to render error
/// statuses in the destructive (red) accent. Must be a free `fn` since
/// `Text::Custom` takes a function pointer (no captures).
fn error_text_style(theme: &cosmic::Theme) -> cosmic::iced::widget::text::Style {
    cosmic::iced::widget::text::Style {
        color: Some(theme.cosmic().destructive_text_color().into()),
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

/// Tip card explaining why the target workspace should be pinned. Drops the
/// step-by-step "press Super then click the pin" description in favour of a
/// button that opens the Workspaces overview directly.
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

fn rule_row<'a>(r: &'a Rule, workspaces: &'a [WorkspaceSnapshot]) -> Element<'a, Msg> {
    let target_str = render_target(r, workspaces);
    let switch_suffix = if r.switch_to_workspace {
        "  + switch"
    } else {
        ""
    };
    let summary = match &r.title_contains {
        Some(t) => format!(
            "{}  (title ⊇ \"{}\")  →  workspace {target_str}{switch_suffix}",
            r.app_id, t,
        ),
        None => format!("{}  →  workspace {target_str}{switch_suffix}", r.app_id),
    };
    let edit = button::standard("Edit").on_press(Msg::EditRule(r.id));
    let toggle = button::standard(if r.enabled { "Disable" } else { "Enable" })
        .on_press(Msg::ToggleEnabled(r.id));
    let del = button::destructive("Delete").on_press(Msg::DeleteRule(r.id));

    Row::new()
        .align_y(Alignment::Center)
        .spacing(8)
        .push(text::body(summary).width(Length::Fill))
        .push(edit)
        .push(toggle)
        .push(del)
        .into()
}

/// Render a rule's workspace target with its owning output. Prefers the
/// `target_output` saved with the rule (authoritative on multi-monitor
/// setups where two workspaces can share a name). Falls back to looking up
/// the first matching workspace from the live snapshot — used for rules
/// persisted before `target_output` existed.
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
