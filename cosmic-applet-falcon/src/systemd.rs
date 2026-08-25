use std::sync::LazyLock;
use std::time::Duration;

use futures_util::SinkExt;
use tokio::sync::Notify;
use zbus::Connection;
use zbus::proxy;
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;

use crate::process::{self, FalconProc};

pub static REFRESH_NOTIFY: LazyLock<Notify> = LazyLock::new(Notify::new);

pub const UNIT: &str = "falcon-sensor.service";

const POLL_INTERVAL: Duration = Duration::from_secs(5);
// Transitional states (activating/deactivating) resolve within seconds; poll
// faster so the panel icon doesn't lag behind a start/stop the user just did.
const POLL_TRANSITIONAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Unknown,
}

impl ActiveState {
    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "reloading" => Self::Reloading,
            "inactive" => Self::Inactive,
            "failed" => Self::Failed,
            "activating" => Self::Activating,
            "deactivating" => Self::Deactivating,
            _ => Self::Unknown,
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Active | Self::Reloading)
    }

    pub fn is_transitional(self) -> bool {
        matches!(
            self,
            Self::Activating | Self::Deactivating | Self::Reloading
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Reloading => "reloading",
            Self::Inactive => "inactive",
            Self::Failed => "failed",
            Self::Activating => "activating",
            Self::Deactivating => "deactivating",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub active_state: ActiveState,
    pub sub_state: String,
    pub load_state: String,
    pub unit_file_state: Option<String>,
    pub processes: Vec<FalconProc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitAction {
    Start,
    Stop,
    Restart,
}

impl UnitAction {
    pub fn progress_label(self) -> &'static str {
        match self {
            Self::Start => "Starting\u{2026}",
            Self::Stop => "Stopping\u{2026}",
            Self::Restart => "Restarting\u{2026}",
        }
    }
}

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn load_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    fn get_unit_file_state(&self, name: &str) -> zbus::Result<String>;
    // The interactive-auth flag lets polkit pop its graphical authentication
    // dialog instead of failing with AccessDenied for unprivileged callers.
    #[zbus(allow_interactive_auth)]
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    #[zbus(allow_interactive_auth)]
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    #[zbus(allow_interactive_auth)]
    fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn sub_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn load_state(&self) -> zbus::Result<String>;
}

pub async fn fetch_snapshot(conn: &Connection) -> anyhow::Result<Snapshot> {
    let manager = SystemdManagerProxy::new(conn).await?;
    let path = manager.load_unit(UNIT).await?;
    // systemd only emits PropertiesChanged for units after Manager.Subscribe();
    // without it zbus's property cache would go permanently stale — disable it.
    let unit = SystemdUnitProxy::builder(conn)
        .path(path)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let active_state = ActiveState::parse(&unit.active_state().await?);
    let sub_state = unit.sub_state().await?;
    let load_state = unit.load_state().await?;
    let unit_file_state = manager.get_unit_file_state(UNIT).await.ok();
    let processes = process::scan(std::path::Path::new("/proc"));
    Ok(Snapshot {
        active_state,
        sub_state,
        load_state,
        unit_file_state,
        processes,
    })
}

pub async fn run_action(action: UnitAction) -> Result<(), String> {
    let result = async {
        let conn = Connection::system().await?;
        let manager = SystemdManagerProxy::new(&conn).await?;
        match action {
            UnitAction::Start => manager.start_unit(UNIT, "replace").await?,
            UnitAction::Stop => manager.stop_unit(UNIT, "replace").await?,
            UnitAction::Restart => manager.restart_unit(UNIT, "replace").await?,
        };
        Ok::<(), zbus::Error>(())
    }
    .await;
    result.map_err(|e| friendly_error(&e))
}

fn friendly_error(e: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, detail, _) = e {
        if name.as_str().ends_with(".AccessDenied")
            || name.as_str().ends_with(".InteractiveAuthorizationRequired")
        {
            return "Not authorized (authentication dialog dismissed?)".to_owned();
        }
        if let Some(detail) = detail {
            return detail.clone();
        }
    }
    e.to_string()
}

pub fn stream() -> impl cosmic::iced::futures::Stream<Item = Result<Snapshot, String>> {
    cosmic::iced::stream::channel(
        8,
        |mut sender: cosmic::iced::futures::channel::mpsc::Sender<Result<Snapshot, String>>| async move {
            let mut conn: Option<Connection> = None;
            let mut last: Option<Result<Snapshot, String>> = None;
            loop {
                if conn.is_none() {
                    match Connection::system().await {
                        Ok(c) => conn = Some(c),
                        Err(e) => tracing::warn!(error = %e, "failed to open system bus"),
                    }
                }
                let snap = match &conn {
                    Some(c) => fetch_snapshot(c).await.map_err(|e| e.to_string()),
                    None => Err("cannot connect to the system bus".to_owned()),
                };
                if snap.is_err() {
                    // Could be a dead bus connection; rebuild it on the next tick.
                    conn = None;
                }
                if last.as_ref() != Some(&snap) {
                    if sender.send(snap.clone()).await.is_err() {
                        return;
                    }
                    last = Some(snap.clone());
                }
                let interval = match &snap {
                    Ok(s) if s.active_state.is_transitional() => POLL_TRANSITIONAL,
                    _ => POLL_INTERVAL,
                };
                tokio::select! {
                    () = tokio::time::sleep(interval) => {}
                    () = REFRESH_NOTIFY.notified() => {
                        tracing::debug!("manual refresh requested");
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_maps_every_systemd_state() {
        assert_eq!(ActiveState::parse("active"), ActiveState::Active);
        assert_eq!(ActiveState::parse("reloading"), ActiveState::Reloading);
        assert_eq!(ActiveState::parse("inactive"), ActiveState::Inactive);
        assert_eq!(ActiveState::parse("failed"), ActiveState::Failed);
        assert_eq!(ActiveState::parse("activating"), ActiveState::Activating);
        assert_eq!(
            ActiveState::parse("deactivating"),
            ActiveState::Deactivating
        );
        assert_eq!(ActiveState::parse("refreshing"), ActiveState::Unknown);
        assert_eq!(ActiveState::parse(""), ActiveState::Unknown);
    }

    #[test]
    fn running_and_transitional_split() {
        assert!(ActiveState::Active.is_running());
        assert!(ActiveState::Reloading.is_running());
        assert!(!ActiveState::Inactive.is_running());
        assert!(!ActiveState::Failed.is_running());
        assert!(!ActiveState::Activating.is_running());

        assert!(ActiveState::Activating.is_transitional());
        assert!(ActiveState::Deactivating.is_transitional());
        assert!(ActiveState::Reloading.is_transitional());
        assert!(!ActiveState::Active.is_transitional());
        assert!(!ActiveState::Inactive.is_transitional());
    }
}
