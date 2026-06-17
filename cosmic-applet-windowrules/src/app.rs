use cosmic::Element;
use cosmic::app::Task;
use cosmic::applet::menu_button;
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::surface::{self, action::destroy_popup};
use cosmic::widget::{Column, button, text};

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
    caps: ManagerCaps,
    sender: Option<WlSender>,
    menu_popup: Option<Id>,
}

#[derive(Debug, Clone)]
pub enum Message {
    WlEvt(WlEvent),
    LeftClick,
    OpenMenu,
    OpenSettings,
    PopupClosed(Id),
    OverviewResult(Result<(), String>),
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
        (
            Self {
                core,
                config: Config::load(),
                workspaces: Vec::new(),
                caps: ManagerCaps::empty(),
                sender: None,
                menu_popup: None,
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::run(wl_run).map(Message::WlEvt)
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
            Message::LeftClick => return open_workspace_overview(),
            Message::OpenMenu => return self.toggle_menu_popup(),
            Message::OpenSettings => {
                let close = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                return Task::batch([close, spawn_settings_window()]);
            }
            Message::PopupClosed(id) => {
                if self.menu_popup.as_ref() == Some(&id) {
                    self.menu_popup = None;
                }
            }
            Message::OverviewResult(Ok(())) => {}
            Message::OverviewResult(Err(e)) => {
                tracing::warn!(error = %e, "failed to open workspace overview");
            }
            Message::NoOp => {}
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
                toplevels: _,
            } => {
                self.caps = caps;
                self.workspaces = workspaces;
                tracing::debug!(
                    workspaces = self.workspaces.len(),
                    "applet: snapshot received"
                );
            }
            WlEvent::NewToplevel(snap) => {
                self.handle_new_toplevel(&snap);
            }
        }
        Task::none()
    }

    fn handle_new_toplevel(&mut self, snap: &ToplevelSnapshot) {
        // Reload from disk so rules edited in the settings window apply
        // without restarting the panel applet. cosmic-config reads are cheap.
        self.config = Config::load();
        tracing::debug!(
            app_id = %snap.app_id,
            title = %snap.title,
            identifier = %snap.identifier,
            rules = self.config.rules.len(),
            "applet: new toplevel"
        );

        let Some(rule) = self.find_matching_rule(snap) else {
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

    fn find_matching_rule(&self, snap: &ToplevelSnapshot) -> Option<&Rule> {
        self.config
            .rules
            .iter()
            .find(|r| r.matches(&snap.app_id, &snap.title))
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
        let body = Column::new()
            .padding(4)
            .spacing(0)
            .push(menu_button(text::body("Settings…")).on_press(Message::OpenSettings));
        Element::from(self.core.applet.popup_container(body))
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
        let res = call_workspaces_show().await.map_err(|e| e.to_string());
        Message::OverviewResult(res)
    })
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

fn open_menu_popup(new_id: Id) -> Task<Message> {
    let action = surface::action::app_popup::<AppModel>(
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
