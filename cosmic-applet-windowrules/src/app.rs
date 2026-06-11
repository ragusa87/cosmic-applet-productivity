use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::{Length, Limits, Subscription, window::Id};
use cosmic::surface::{self, action::destroy_popup};
use cosmic::widget::{Column, button, text};

use crate::config::Config;
use crate::models::Rule;
use crate::wayland::{
    ManagerCaps, ToplevelSnapshot, WlCommand, WlEvent, WlSender, WorkspaceRef, WorkspaceSnapshot,
    run as wl_run,
};

const APP_ID: &str = "com.github.ragusa87.CosmicAppletWindowRules";
const ICON_SVG: &[u8] =
    include_bytes!("../data/icons/com.github.ragusa87.CosmicAppletWindowRules.svg");

pub struct AppModel {
    core: cosmic::Core,
    config: Config,
    workspaces: Vec<WorkspaceSnapshot>,
    caps: ManagerCaps,
    sender: Option<WlSender>,
    last_action: Option<String>,
    info_popup: Option<Id>,
    menu_popup: Option<Id>,
}

#[derive(Debug, Clone)]
pub enum Message {
    WlEvt(WlEvent),
    LeftClick,
    OpenMenu,
    OpenSettings,
    PopupClosed(Id),
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
                last_action: None,
                info_popup: None,
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
        use cosmic::iced::widget::Row;
        use cosmic::iced::widget::mouse_area;

        let is_horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let (icon_size, _) = self.core.applet.suggested_size(true);
        let (pad_major, pad_minor) = self.core.applet.suggested_padding(true);
        let icon_px = f32::from(icon_size);
        let label_size = (icon_px * 0.55).round();

        let icon = cosmic::widget::icon(
            cosmic::widget::icon::from_svg_bytes(ICON_SVG.to_vec()).symbolic(true),
        )
        .size(icon_size);

        let enabled = self.config.rules.iter().filter(|r| r.enabled).count();
        let mut row = Row::new()
            .align_y(cosmic::iced::Alignment::Center)
            .spacing(6)
            .push(icon);
        if is_horizontal && enabled > 0 {
            row = row.push(
                cosmic::widget::text(format!("{enabled}"))
                    .size(label_size)
                    .height(Length::Fixed(icon_px))
                    .align_y(cosmic::iced::alignment::Vertical::Center),
            );
        }

        let content: Element<'_, Self::Message> = row.into();

        let (h_pad, v_pad) = if is_horizontal {
            (pad_major, pad_minor)
        } else {
            (pad_minor, pad_major)
        };

        let btn = button::custom(content)
            .padding([v_pad, h_pad])
            .on_press(Message::LeftClick)
            .class(cosmic::theme::Button::AppletIcon);

        let interactive = mouse_area(btn).on_right_press(Message::OpenMenu);
        self.core.applet.autosize_window(interactive).into()
    }

    fn view_window(&self, id: Id) -> Element<'_, Self::Message> {
        if self.menu_popup == Some(id) {
            self.menu_view()
        } else {
            self.info_view()
        }
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::WlEvt(ev) => return self.on_wl(ev),
            Message::LeftClick => return self.toggle_info_popup(),
            Message::OpenMenu => return self.toggle_menu_popup(),
            Message::OpenSettings => {
                let close = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                return Task::batch([close, spawn_settings_window()]);
            }
            Message::PopupClosed(id) => {
                if self.info_popup.as_ref() == Some(&id) {
                    self.info_popup = None;
                }
                if self.menu_popup.as_ref() == Some(&id) {
                    self.menu_popup = None;
                }
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
                self.handle_new_toplevel(snap);
            }
        }
        Task::none()
    }

    fn handle_new_toplevel(&mut self, snap: ToplevelSnapshot) {
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

        let Some(rule) = self.find_matching_rule(&snap) else {
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

        sender.send(WlCommand::PinWorkspace {
            workspace: target.clone(),
            output: output.clone(),
        });
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
        self.last_action = Some(format!("{} → {}", snap.app_id, rule.target.display()));
    }

    fn find_matching_rule(&self, snap: &ToplevelSnapshot) -> Option<&Rule> {
        self.config
            .rules
            .iter()
            .find(|r| r.matches(&snap.app_id, &snap.title))
    }

    fn toggle_info_popup(&mut self) -> Task<Message> {
        // If the right-click menu is up, close it first so we don't stack popups.
        let close_menu = self
            .menu_popup
            .take()
            .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
        // Toggle the info popup itself.
        if let Some(id) = self.info_popup.take() {
            return Task::batch([close_menu, dispatch_surface(destroy_popup(id))]);
        }
        let new_id = Id::unique();
        self.info_popup = Some(new_id);
        Task::batch([close_menu, open_info_popup(new_id)])
    }

    fn toggle_menu_popup(&mut self) -> Task<Message> {
        let close_info = self
            .info_popup
            .take()
            .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
        if let Some(id) = self.menu_popup.take() {
            return Task::batch([close_info, dispatch_surface(destroy_popup(id))]);
        }
        let new_id = Id::unique();
        self.menu_popup = Some(new_id);
        Task::batch([close_info, open_menu_popup(new_id)])
    }

    fn info_view(&self) -> Element<'_, Message> {
        let total = self.config.rules.len();
        let enabled = self.config.rules.iter().filter(|r| r.enabled).count();
        let cap_line = if self.caps.contains(ManagerCaps::MOVE_TO_EXT_WORKSPACE) {
            "move_to_ext_workspace: advertised"
        } else if self.caps.is_empty() {
            "probing compositor…"
        } else {
            "move_to_ext_workspace: attempting (not advertised)"
        };
        let last = match &self.last_action {
            Some(s) => s.clone(),
            None => "No rule has fired yet.".into(),
        };
        let body = Column::new()
            .padding(12)
            .spacing(6)
            .push(text::title4("Window Rules"))
            .push(text::body(format!("{enabled} enabled / {total} total")))
            .push(text::caption(cap_line))
            .push(text::caption(last));
        Element::from(self.core.applet.popup_container(body))
    }

    fn menu_view(&self) -> Element<'_, Message> {
        // popup_container is wrapped in `autosize`, which shrinks to content;
        // setting `width(Fill)` on the column is therefore meaningless. The
        // dependable way to make the button match the popup width is to give
        // its enclosing column an explicit fixed width that is consistent
        // with the popup's size_limits (min_width=200, max_width=280).
        const MENU_WIDTH: f32 = 220.0;
        let body = Column::new()
            .padding(4)
            .spacing(4)
            .width(Length::Fixed(MENU_WIDTH))
            .push(
                button::text("Settings…")
                    .width(Length::Fill)
                    .on_press(Message::OpenSettings),
            );
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

fn open_info_popup(new_id: Id) -> Task<Message> {
    let action = surface::action::app_popup::<AppModel>(
        move |state: &mut AppModel| {
            let parent = state.core.main_window_id().unwrap_or(Id::NONE);
            let mut settings = state
                .core
                .applet
                .get_popup_settings(parent, new_id, None, None, None);
            settings.grab = true;
            settings.positioner.size_limits = Limits::NONE
                .max_width(420.0)
                .min_width(280.0)
                .min_height(100.0)
                .max_height(280.0);
            settings
        },
        Some(Box::new(|state: &AppModel| {
            Element::from(state.info_view()).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
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
                .min_width(200.0)
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
