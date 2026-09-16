use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::window::Id;
use cosmic::iced::{self, Size};
use cosmic::surface::{self, action::destroy_layer_shell};
use cosmic_config::CosmicConfigEntry;
use cosmic_google_common::auth::{self, OAuthParams};
use cosmic_google_common::secrets::{self, Tokens};

use crate::config::{APP_ID, Config, KEYRING_SERVICE};
use crate::ui::{self, CredentialsForm, OverlayContent, SettingsHandlers, Status};

const SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";
const SUCCESS_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Authorization complete</title><link rel=\"icon\" href=\"data:,\"></head><body style=\"font-family:sans-serif;text-align:center;padding-top:4em\"><h1>You can close this tab</h1><p>The agenda applet has received your authorization.</p></body></html>";

pub fn run() -> iced::Result {
    // Both modes ship in the same binary, so `pkill -USR2 cosmic-applet-google-agenda`
    // would also reach this process. SIGUSR2's default action is to terminate;
    // ignore it here so an external "refresh the applet" signal doesn't kill
    // an open settings window.
    // SAFETY: signal(2) with SIG_IGN is async-signal-safe and has no preconditions.
    unsafe {
        libc::signal(libc::SIGUSR2, libc::SIG_IGN);
    }

    let settings = cosmic::app::Settings::default().size(Size::new(500.0, 460.0));
    cosmic::app::run::<SettingsApp>(settings, ())
}

#[derive(Default)]
pub struct SettingsApp {
    core: cosmic::Core,
    config: Config,
    form: CredentialsForm,
    status: Status,
    authorizing: bool,
    /// Copy rendered on the meeting-overlay preview while it is shown.
    overlay: Option<OverlayContent>,
    /// Layer-shell surface id of the overlay preview; `None` when not shown.
    /// Guards against opening more than one.
    overlay_surface: Option<Id>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    FormEmail(String),
    FormClientId(String),
    FormClientSecret(String),
    ToggleShowTitle(bool),
    ToggleShowTime(bool),
    ToggleShowProgress(bool),
    ToggleNotify(bool),
    ToggleShowMeetingOverlay(bool),
    ToggleDisableDuringWeekend(bool),
    SetLeadIdx(usize),
    TryNotify,
    TestOverlay,
    CloseOverlay,
    Authorize,
    AuthorizeDone(Result<(String, String, Tokens), String>),
    Cancel,
    SavedAndExit,
    LoadTokens(Option<Tokens>),
}

impl SettingsApp {
    /// Kick off the OAuth flow: mark the app busy and spawn the browser-based
    /// authorization, resolving to `Msg::AuthorizeDone`.
    fn start_authorization(&mut self) -> Task<Msg> {
        if self.authorizing || !self.form.is_complete() {
            return Task::none();
        }
        self.authorizing = true;
        self.status = Status::Authorizing;

        let email = self.form.email.clone();
        let client_id = self.form.client_id.clone();
        let client_secret = self.form.client_secret.clone();

        cosmic::task::future(async move {
            let params = OAuthParams {
                scope: SCOPE,
                success_html: SUCCESS_HTML,
            };
            let result = auth::start_oauth_flow(params, client_id.clone(), client_secret).await;
            let result = result
                .map(|tokens| (email, client_id, tokens))
                .map_err(|e| e.to_string());
            Msg::AuthorizeDone(result)
        })
    }
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
        let config = cosmic_config::Config::new(APP_ID, Config::VERSION)
            .map(|ctx| match Config::get_entry(&ctx) {
                Ok(c) => c,
                Err((_errors, c)) => c,
            })
            .unwrap_or_default();

        let form = CredentialsForm {
            email: config.email.clone(),
            client_id: config.client_id.clone(),
            ..CredentialsForm::default()
        };

        let task = if config.is_configured() {
            let email = config.email.clone();
            cosmic::task::future(async move {
                let tokens = secrets::load(KEYRING_SERVICE, &email).await.ok();
                Msg::LoadTokens(tokens)
            })
        } else {
            Task::none()
        };

        (
            Self {
                core,
                config,
                form,
                ..Self::default()
            },
            task,
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Msg> {
        // The compositor closing the overlay surface (e.g. Esc) should tear down
        // the preview, not the settings window. Other surfaces fall through to
        // the default handling.
        (self.overlay_surface == Some(id)).then_some(Msg::CloseOverlay)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let handlers = SettingsHandlers {
            on_email: Msg::FormEmail,
            on_client_id: Msg::FormClientId,
            on_client_secret: Msg::FormClientSecret,
            on_toggle_show_title: Msg::ToggleShowTitle,
            on_toggle_show_time: Msg::ToggleShowTime,
            on_toggle_show_progress: Msg::ToggleShowProgress,
            on_toggle_notify: Msg::ToggleNotify,
            on_toggle_show_meeting_overlay: Msg::ToggleShowMeetingOverlay,
            on_toggle_disable_during_weekend: Msg::ToggleDisableDuringWeekend,
            on_lead_change: Msg::SetLeadIdx,
            on_try_notify: Msg::TryNotify,
            on_test_overlay: Msg::TestOverlay,
            authorize: Msg::Authorize,
            cancel: Msg::Cancel,
        };
        ui::settings_view(
            &self.form,
            &self.config,
            &self.status,
            self.authorizing,
            &handlers,
        )
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Msg::FormEmail(s) => self.form.email = s,
            Msg::FormClientId(s) => self.form.client_id = s,
            Msg::FormClientSecret(s) => self.form.client_secret = s,

            Msg::ToggleShowTitle(on) => {
                self.config.show_title = on;
                persist_config(&self.config);
            }

            Msg::ToggleShowTime(on) => {
                self.config.show_time = on;
                persist_config(&self.config);
            }

            Msg::ToggleShowProgress(on) => {
                self.config.show_progress = on;
                persist_config(&self.config);
            }

            Msg::ToggleNotify(on) => {
                self.config.notify = on;
                persist_config(&self.config);
            }

            Msg::ToggleShowMeetingOverlay(on) => {
                self.config.show_meeting_overlay = on;
                persist_config(&self.config);
            }

            Msg::ToggleDisableDuringWeekend(on) => {
                self.config.disable_during_weekend = on;
                persist_config(&self.config);
            }

            Msg::SetLeadIdx(idx) => {
                if let Some(&secs) = ui::LEAD_PRESETS_SECS.get(idx) {
                    self.config.notification_lead_secs = secs;
                    persist_config(&self.config);
                }
            }

            Msg::TryNotify => fire_preview_notification(&self.config),

            Msg::TestOverlay => {
                // Open the same full-screen layer-shell overlay the applet shows
                // for a real meeting, with placeholder copy. One at a time.
                if self.overlay_surface.is_none() {
                    let id = Id::unique();
                    self.overlay = Some(OverlayContent::test());
                    self.overlay_surface = Some(id);
                    return open_meeting_overlay(id);
                }
            }

            Msg::CloseOverlay => {
                self.overlay = None;
                if let Some(id) = self.overlay_surface.take() {
                    return dispatch_surface(destroy_layer_shell(id));
                }
            }

            Msg::LoadTokens(Some(tokens)) => {
                if self.form.client_secret.is_empty() {
                    self.form.client_secret = tokens.client_secret;
                }
            }
            Msg::LoadTokens(None) => {}

            Msg::Authorize => return self.start_authorization(),

            Msg::AuthorizeDone(Ok((email, client_id, tokens))) => {
                self.authorizing = false;
                self.status = Status::Saved;

                let mut new_cfg = self.config.clone();
                new_cfg.email.clone_from(&email);
                new_cfg.client_id = client_id;
                persist_config(&new_cfg);
                self.config = new_cfg;

                return cosmic::task::future(async move {
                    if let Err(e) = secrets::save(KEYRING_SERVICE, &email, &tokens).await {
                        tracing::warn!(error = %e, "failed to persist tokens");
                    }
                    Msg::SavedAndExit
                });
            }

            Msg::AuthorizeDone(Err(e)) => {
                self.authorizing = false;
                self.status = Status::Error(e);
            }

            Msg::Cancel | Msg::SavedAndExit => {
                return cosmic::iced::exit();
            }
        }
        Task::none()
    }
}

/// Fire a dummy notification in the same format as a real meeting reminder (see
/// `decide_notify` in app.rs): summary is the lead time, body is
/// "<title> — <HH:MM>".
fn fire_preview_notification(config: &Config) {
    let lead_min = config.notification_lead_secs / 60;
    let start =
        chrono::Local::now() + chrono::Duration::seconds(i64::from(config.notification_lead_secs));
    cosmic_google_common::notify::show(
        &format!("Meeting in {lead_min} min"),
        &format!("Team standup \u{2014} {}", start.format("%H:%M")),
        APP_ID,
    );
}

/// Open the meeting-overlay preview as a layer-shell surface, reusing the same
/// shared settings and view as the applet so the preview is pixel-identical.
/// Both overlay buttons map to `CloseOverlay` here — this is a look preview, so
/// "Snooze" simply dismisses it rather than scheduling a re-show.
fn open_meeting_overlay(id: Id) -> Task<Msg> {
    let action = surface::action::app_layer_shell::<SettingsApp>(
        |_state: &SettingsApp| ui::overlay_live_settings(),
        move |_state: &mut SettingsApp| ui::overlay_layer_settings(id),
        Some(Box::new(|state: &SettingsApp| {
            ui::meeting_overlay_view(state.overlay.as_ref(), Msg::CloseOverlay, Msg::CloseOverlay)
                .map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
}

fn dispatch_surface(a: surface::Action) -> Task<Msg> {
    cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(a)))
}

fn persist_config(config: &Config) {
    match cosmic_config::Config::new(APP_ID, Config::VERSION) {
        Ok(ctx) => {
            if let Err(why) = config.write_entry(&ctx) {
                tracing::warn!(?why, "failed writing config entry");
            }
        }
        Err(why) => tracing::warn!(?why, "failed opening cosmic-config"),
    }
}
