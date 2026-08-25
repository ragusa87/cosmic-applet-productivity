use cosmic::Element;
use cosmic::app::Task;
use cosmic::applet::menu_button;
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::surface::{self, action::LiveSettings, action::destroy_popup};
use cosmic::widget::{Column, button, text};

use crate::cap_exceptions::{ExceptionMatcher, cap_exempt};
use crate::config::{APP_ID, Config};
use crate::models::Rule;
use crate::wayland::{
    ManagerCaps, ToplevelSnapshot, WlCommand, WlEvent, WlSender, WorkspaceRef, WorkspaceSnapshot,
    run as wl_run,
};

const ICON_SVG: &[u8] =
    include_bytes!("../data/icons/com.github.ragusa87.CosmicAppletWindowRules.svg");

pub struct AppModel {
    core: cosmic::Core,
    config: Config,
    workspaces: Vec<WorkspaceSnapshot>,
    toplevels: Vec<ToplevelSnapshot>,
    caps: ManagerCaps,
    sender: Option<WlSender>,
    menu_popup: Option<Id>,
    cap: crate::cap::CapPlanner,
    /// Compiled from `config.cap_exceptions`; rebuilt on config updates.
    cap_exceptions: ExceptionMatcher,
}

#[derive(Debug, Clone)]
pub enum Message {
    WlEvt(WlEvent),
    LeftClick,
    OpenMenu,
    OpenSettings,
    ApplyAllRules,
    PopupClosed(Id),
    OverviewResult(Result<(), String>),
    UpdateConfig(Config),
    NoOp,
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let config = Config::load();
        let cap_exceptions = ExceptionMatcher::new(&config.cap_exceptions);
        (
            Self {
                core,
                config,
                workspaces: Vec::new(),
                toplevels: Vec::new(),
                caps: ManagerCaps::empty(),
                sender: None,
                menu_popup: None,
                cap: crate::cap::CapPlanner::default(),
                cap_exceptions,
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let wl = Subscription::run(wl_run).map(Message::WlEvt);
        let watch = self
            .core()
            .watch_config::<Config>(Self::APP_ID)
            .map(|u| Message::UpdateConfig(u.config));
        Subscription::batch([wl, watch])
    }

    fn view(&self) -> Element<'_, Self::Message> {
        use cosmic::applet::cosmic_panel_config::PanelAnchor;
        use cosmic::iced::widget::mouse_area;

        let is_horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let (icon_size, _) = self.core.applet.suggested_size(true);
        let (pad_major, pad_minor) = self.core.applet.suggested_padding(true);

        let icon = cosmic::widget::icon(
            cosmic::widget::icon::from_svg_bytes(ICON_SVG.to_vec()).symbolic(true),
        )
        .size(icon_size);

        let (h_pad, v_pad) = if is_horizontal {
            (pad_major, pad_minor)
        } else {
            (pad_minor, pad_major)
        };

        let btn = button::custom(icon)
            .padding([v_pad, h_pad])
            .on_press(Message::LeftClick)
            .class(cosmic::theme::Button::AppletIcon);

        let interactive = mouse_area(btn).on_right_press(Message::OpenMenu);
        self.core.applet.autosize_window(interactive).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        self.menu_view()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::WlEvt(ev) => return self.on_wl(ev),
            Message::LeftClick => {
                // Close the grabbed right-click menu first so we don't leave
                // an active input grab while the overview is showing.
                let close = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                return Task::batch([close, open_workspace_overview()]);
            }
            Message::OpenMenu => return self.toggle_menu_popup(),
            Message::OpenSettings => {
                let close = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                return Task::batch([close, spawn_settings_window()]);
            }
            Message::ApplyAllRules => return self.apply_all_rules(),
            Message::PopupClosed(id) => {
                if self.menu_popup.as_ref() == Some(&id) {
                    self.menu_popup = None;
                }
            }
            Message::OverviewResult(Ok(())) | Message::NoOp => {}
            Message::OverviewResult(Err(e)) => {
                tracing::warn!(error = %e, "failed to open workspace overview");
            }
            Message::UpdateConfig(config) => {
                let cap_changed = (
                    self.config.cap_enabled,
                    self.config.cap_max_windows,
                    self.config.cap_only_place_new,
                    &self.config.cap_exceptions,
                ) != (
                    config.cap_enabled,
                    config.cap_max_windows,
                    config.cap_only_place_new,
                    &config.cap_exceptions,
                );
                if self.config.cap_exceptions != config.cap_exceptions {
                    self.cap_exceptions = ExceptionMatcher::new(&config.cap_exceptions);
                }
                self.config = config;
                if cap_changed {
                    // Drop queue/in-flight state from the previous settings;
                    // when (still) enabled, immediately converge under the
                    // new ones (e.g. re-pack existing stacked windows).
                    self.cap.reset();
                    if self.config.cap_enabled {
                        self.run_cap_step();
                    }
                }
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl AppModel {
    fn on_wl(&mut self, ev: WlEvent) -> Task<Message> {
        match ev {
            WlEvent::Ready { caps, cmd_tx } => {
                self.caps = caps;
                self.sender = Some(cmd_tx);
                tracing::debug!(?caps, "applet: wayland ready");
            }
            WlEvent::Snapshot {
                caps,
                workspaces,
                toplevels,
            } => {
                self.caps = caps;
                self.workspaces = workspaces;
                self.toplevels = toplevels;
                tracing::debug!(
                    workspaces = self.workspaces.len(),
                    toplevels = self.toplevels.len(),
                    "applet: snapshot received"
                );
                if self.config.cap_enabled {
                    self.run_cap_step();
                }
            }
            WlEvent::NewToplevel(snap) => {
                // Upsert into self.toplevels so "Apply all rules" sees the
                // window even if clicked before the next Snapshot arrives.
                if let Some(existing) = self
                    .toplevels
                    .iter_mut()
                    .find(|t| t.identifier == snap.identifier)
                {
                    *existing = snap.clone();
                } else {
                    self.toplevels.push(snap.clone());
                }
                if self.config.cap_enabled {
                    // Experimental cap mode fully overrides rules: the window
                    // is queued for capacity-based placement instead. Exempt
                    // windows (dialogs, title-less popups) are left alone.
                    if cap_exempt(&self.cap_exceptions, &snap.app_id, &snap.title) {
                        tracing::info!(
                            app_id = %snap.app_id,
                            title = %snap.title,
                            "cap: window exempt; leaving in place"
                        );
                    } else {
                        self.cap.note_new(&snap.identifier);
                    }
                    self.run_cap_step();
                } else {
                    self.handle_new_toplevel(&snap);
                }
            }
        }
        Task::none()
    }

    fn handle_new_toplevel(&mut self, snap: &ToplevelSnapshot) {
        tracing::debug!(
            app_id = %snap.app_id,
            title = %snap.title,
            identifier = %snap.identifier,
            rules = self.config.rules.len(),
            "applet: new toplevel"
        );

        let Some(rule) = self.select_rule(snap) else {
            return;
        };
        tracing::info!(
            app_id = %snap.app_id,
            rule_label = %rule.label,
            target = %rule.target.display(),
            "applet: rule matched"
        );
        let Some(sender) = self.sender.as_ref() else {
            tracing::warn!("no wayland sender; cannot dispatch move");
            return;
        };

        // We do NOT bail when MOVE_TO_EXT_WORKSPACE is missing: cosmic-comp
        // 1.0.x omits it from its hardcoded capability list while still
        // implementing the request. The wayland thread logs once and
        // proceeds.

        let target = match &rule.target {
            crate::models::WorkspaceTarget::ByName(n) => WorkspaceRef::Name(n.clone()),
            crate::models::WorkspaceTarget::ByIndex(i) => WorkspaceRef::Index(*i),
        };
        let output = rule.target_output.clone();

        sender.send(WlCommand::MoveToplevelToWorkspace {
            toplevel: crate::wayland::ToplevelRef(snap.identifier.clone()),
            workspace: target.clone(),
            output: output.clone(),
        });
        if rule.switch_to_workspace {
            sender.send(WlCommand::ActivateWorkspace {
                workspace: target,
                output,
            });
        }
    }

    /// Advance the "cap windows per workspace" convergence loop by one batch
    /// of moves. Called on every snapshot (and on new-toplevel/enable) while
    /// the experimental option is on; each executed batch produces a fresh
    /// snapshot, which drives the next step.
    fn run_cap_step(&mut self) {
        let opts = crate::cap::CapOptions {
            max_windows: self.config.cap_max_windows.max(1),
            only_place_new: self.config.cap_only_place_new,
        };
        // Exempt windows are never planned: not counted toward any
        // workspace's cap, never evicted or compacted. (A queued window that
        // later turns exempt is dropped by the planner's gc.) They are still
        // passed along so their workspaces read as occupied, not as gaps.
        let (eligible, exempt): (Vec<ToplevelSnapshot>, Vec<ToplevelSnapshot>) = self
            .toplevels
            .iter()
            .cloned()
            .partition(|t| !cap_exempt(&self.cap_exceptions, &t.app_id, &t.title));
        let moves = self.cap.step(&eligible, &exempt, &self.workspaces, &opts);
        if moves.is_empty() {
            return;
        }
        let Some(sender) = self.sender.as_ref() else {
            tracing::warn!("cap: no wayland sender; cannot dispatch moves");
            return;
        };
        for mv in moves {
            tracing::info!(
                identifier = %mv.identifier,
                target_index = mv.target_index,
                output = ?mv.output,
                activate = mv.activate,
                "cap: moving window"
            );
            let workspace = WorkspaceRef::Index(mv.target_index);
            sender.send(WlCommand::MoveToplevelToWorkspace {
                toplevel: crate::wayland::ToplevelRef(mv.identifier),
                workspace: workspace.clone(),
                output: mv.output.clone(),
            });
            if mv.activate {
                sender.send(WlCommand::ActivateWorkspace {
                    workspace,
                    output: mv.output,
                });
            }
        }
    }

    /// Pick the rule to apply to `snap`. Thin wrapper over the shared,
    /// unit-tested [`crate::decide::select_rule`] so the applet and the
    /// `--debug` explainer make identical decisions.
    fn select_rule(&self, snap: &ToplevelSnapshot) -> Option<&Rule> {
        crate::decide::select_rule(
            &self.config.rules,
            &self.workspaces,
            &snap.app_id,
            &snap.title,
        )
    }

    fn toggle_menu_popup(&mut self) -> Task<Message> {
        if let Some(id) = self.menu_popup.take() {
            return dispatch_surface(destroy_popup(id));
        }
        let new_id = Id::unique();
        self.menu_popup = Some(new_id);
        open_menu_popup(new_id)
    }

    fn menu_view(&self) -> Element<'_, Message> {
        let has_enabled_rules = self.config.rules.iter().any(|r| r.enabled);
        let mut body = Column::new().padding(4).spacing(0);
        if has_enabled_rules {
            body = body
                .push(menu_button(text::body("Apply all rules")).on_press(Message::ApplyAllRules));
        }
        body = body.push(menu_button(text::body("Settings…")).on_press(Message::OpenSettings));
        Element::from(self.core.applet.popup_container(body))
    }

    fn apply_all_rules(&mut self) -> Task<Message> {
        let close = self
            .menu_popup
            .take()
            .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));

        let Some(sender) = self.sender.as_ref() else {
            tracing::warn!("apply all: wayland not ready");
            return close;
        };

        // Iterate windows (not rules): each window gets exactly one move — the
        // best applicable rule per `select_rule` — so stacked fallback rules
        // don't move the same window twice. `allow_switch` is effectively off
        // here (we never activate) to avoid yanking the user across workspaces.
        let mut total = 0usize;
        for snap in &self.toplevels {
            let Some(rule) = self.select_rule(snap) else {
                continue;
            };
            let target = match &rule.target {
                crate::models::WorkspaceTarget::ByName(n) => WorkspaceRef::Name(n.clone()),
                crate::models::WorkspaceTarget::ByIndex(i) => WorkspaceRef::Index(*i),
            };
            sender.send(WlCommand::MoveToplevelToWorkspace {
                toplevel: crate::wayland::ToplevelRef(snap.identifier.clone()),
                workspace: target,
                output: rule.target_output.clone(),
            });
            total += 1;
        }
        tracing::info!(
            total,
            rules_total = self.config.rules.len(),
            "applet: apply all rules"
        );
        close
    }
}

fn dispatch_surface(a: surface::Action) -> Task<Message> {
    cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(a)))
}

fn spawn_settings_window() -> Task<Message> {
    cosmic::task::future(async move {
        match std::env::current_exe() {
            Ok(path) => {
                if let Err(e) = tokio::process::Command::new(path)
                    .arg("--show-settings")
                    .spawn()
                {
                    tracing::warn!(error = %e, "failed to spawn settings window");
                }
            }
            Err(e) => tracing::warn!(error = %e, "current_exe() failed"),
        }
        Message::NoOp
    })
}

fn open_workspace_overview() -> Task<Message> {
    cosmic::task::future(async move {
        let res = crate::dbus::show_workspace_overview()
            .await
            .map_err(|e| e.to_string());
        Message::OverviewResult(res)
    })
}

fn open_menu_popup(new_id: Id) -> Task<Message> {
    let action = surface::action::app_popup::<AppModel>(
        |_| LiveSettings::default(),
        move |state: &mut AppModel| {
            let parent = state.core.main_window_id().unwrap_or(Id::NONE);
            let mut settings = state
                .core
                .applet
                .get_popup_settings(parent, new_id, None, None, None);
            settings.grab = true;
            settings.positioner.size_limits = Limits::NONE
                .max_width(280.0)
                .min_width(180.0)
                .min_height(40.0)
                .max_height(160.0);
            settings
        },
        Some(Box::new(|state: &AppModel| {
            Element::from(state.menu_view()).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
}
