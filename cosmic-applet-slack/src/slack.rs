use std::sync::{LazyLock, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use tokio::sync::Notify;
use zbus::Connection;
use zbus::proxy;

pub static REFRESH_NOTIFY: LazyLock<Notify> = LazyLock::new(Notify::new);

const RESCAN_GONE_INTERVAL: Duration = Duration::from_secs(2);
const RESCAN_OK_INTERVAL: Duration = Duration::from_secs(5);
const SLACK_PROCESS: &str = "slack";
const SNI_PATH: &str = "/StatusNotifierItem";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unread {
    #[default]
    None,
    Indicator,
    Count(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackEvent {
    Unread(Unread),
    Gone,
}

pub(crate) type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

pub(crate) struct DebugCandidate {
    pub name: String,
    pub pid: u32,
    pub comm: String,
    pub tooltip: Result<ToolTip, String>,
    pub parsed: Option<Unread>,
}

pub(crate) struct DebugReport {
    pub total_names: usize,
    pub connection_names: usize,
    pub slack_candidates: Vec<DebugCandidate>,
    pub chosen: Option<String>,
}

#[proxy(
    interface = "org.freedesktop.DBus",
    default_service = "org.freedesktop.DBus",
    default_path = "/org/freedesktop/DBus"
)]
trait DBus {
    fn list_names(&self) -> zbus::Result<Vec<String>>;
    #[zbus(name = "GetConnectionUnixProcessID")]
    fn get_connection_unix_process_id(&self, bus_name: &str) -> zbus::Result<u32>;
    #[zbus(signal)]
    fn name_owner_changed(
        &self,
        name: String,
        old_owner: String,
        new_owner: String,
    ) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.kde.StatusNotifierItem",
    default_path = "/StatusNotifierItem"
)]
trait StatusNotifierItem {
    #[zbus(property)]
    fn tool_tip(&self) -> zbus::Result<ToolTip>;
    #[zbus(signal)]
    fn new_tool_tip(&self) -> zbus::Result<()>;
}

pub fn stream() -> impl cosmic::iced::futures::Stream<Item = SlackEvent> {
    cosmic::iced::stream::channel(8, |sender| async move {
        run(sender).await;
    })
}

#[allow(clippy::too_many_lines)]
async fn run(mut sender: cosmic::iced::futures::channel::mpsc::Sender<SlackEvent>) {
    let conn = match Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to open session bus");
            return;
        }
    };
    let dbus = match DBusProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to build org.freedesktop.DBus proxy");
            return;
        }
    };

    let mut last_unread: Option<Unread> = None;
    let mut last_gone = false;

    loop {
        let Some(service) = find_slack_service(&conn, &dbus).await else {
            if !last_gone {
                if sender.send(SlackEvent::Gone).await.is_err() {
                    return;
                }
                last_gone = true;
                last_unread = None;
            }
            tokio::select! {
                () = tokio::time::sleep(RESCAN_GONE_INTERVAL) => {}
                () = REFRESH_NOTIFY.notified() => {
                    tracing::info!("manual refresh while Slack not running");
                }
            }
            continue;
        };

        last_gone = false;
        tracing::info!(service = %service, "found Slack StatusNotifierItem");

        let proxy = match build_sni_proxy(&conn, &service).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "failed to build SNI proxy");
                tokio::time::sleep(RESCAN_OK_INTERVAL).await;
                continue;
            }
        };

        let unread = read_tooltip(&proxy).await;
        if last_unread != Some(unread) {
            if sender.send(SlackEvent::Unread(unread)).await.is_err() {
                return;
            }
            last_unread = Some(unread);
        }

        let mut new_tool_tip = match proxy.receive_new_tool_tip().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to subscribe to NewToolTip");
                tokio::time::sleep(RESCAN_OK_INTERVAL).await;
                continue;
            }
        };
        let mut name_changes = match dbus.receive_name_owner_changed().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to subscribe to NameOwnerChanged");
                tokio::time::sleep(RESCAN_OK_INTERVAL).await;
                continue;
            }
        };

        let restart = loop {
            tokio::select! {
                _ = new_tool_tip.next() => {
                    let unread = read_tooltip(&proxy).await;
                    if last_unread != Some(unread) {
                        if sender.send(SlackEvent::Unread(unread)).await.is_err() {
                            return;
                        }
                        last_unread = Some(unread);
                    }
                }
                msg = name_changes.next() => {
                    match msg {
                        Some(signal) => {
                            if let Ok(args) = signal.args()
                                && args.name == service
                                && args.new_owner.is_empty()
                            {
                                tracing::info!("Slack bus name disappeared");
                                break true;
                            }
                        }
                        None => break true,
                    }
                }
                () = tokio::time::sleep(RESCAN_OK_INTERVAL) => {
                    match proxy.tool_tip().await {
                        Ok(t) => {
                            let unread = parse_unread(&t);
                            if last_unread != Some(unread) {
                                if sender.send(SlackEvent::Unread(unread)).await.is_err() {
                                    return;
                                }
                                last_unread = Some(unread);
                            }
                        }
                        Err(e) => {
                            tracing::info!(error = %e, "periodic ToolTip read failed; restarting");
                            break true;
                        }
                    }
                }
                () = REFRESH_NOTIFY.notified() => {
                    tracing::info!("manual refresh requested");
                    let unread = read_tooltip(&proxy).await;
                    if last_unread != Some(unread) {
                        if sender.send(SlackEvent::Unread(unread)).await.is_err() {
                            return;
                        }
                        last_unread = Some(unread);
                    }
                }
            }
        };

        if restart && sender.send(SlackEvent::Gone).await.is_err() {
            return;
        }
        if restart {
            last_gone = true;
            last_unread = None;
        }
    }
}

async fn find_slack_service(conn: &Connection, dbus: &DBusProxy<'_>) -> Option<String> {
    let names = match dbus.list_names().await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "ListNames failed");
            return None;
        }
    };

    let mut candidates = Vec::new();
    for name in names {
        if !name.starts_with(':') {
            continue;
        }
        let Ok(pid) = dbus.get_connection_unix_process_id(&name).await else {
            continue;
        };
        if process_name(pid).await.as_deref() == Some(SLACK_PROCESS) {
            candidates.push(name);
        }
    }

    for name in candidates {
        let Ok(proxy) = build_sni_proxy(conn, &name).await else {
            continue;
        };
        match tokio::time::timeout(Duration::from_millis(500), proxy.tool_tip()).await {
            Ok(Ok(_)) => return Some(name),
            Ok(Err(_)) => {}
            Err(_) => {
                tracing::debug!(service = %name, "ToolTip probe timed out");
            }
        }
    }
    None
}

async fn build_sni_proxy(
    conn: &Connection,
    bus_name: &str,
) -> zbus::Result<StatusNotifierItemProxy<'static>> {
    StatusNotifierItemProxy::builder(conn)
        .destination(bus_name.to_owned())?
        .path(SNI_PATH)?
        .build()
        .await
}

async fn process_name(pid: u32) -> Option<String> {
    let path = format!("/proc/{pid}/comm");
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|s| s.trim().to_owned())
}

async fn read_tooltip(proxy: &StatusNotifierItemProxy<'_>) -> Unread {
    match proxy.tool_tip().await {
        Ok(t) => {
            let unread = parse_unread(&t);
            tracing::debug!(
                title = %t.2,
                description = %t.3,
                parsed = ?unread,
                "ToolTip fetched"
            );
            unread
        }
        Err(e) => {
            tracing::debug!(error = %e, "ToolTip read failed");
            Unread::None
        }
    }
}

pub(crate) async fn debug_scan() -> anyhow::Result<DebugReport> {
    let conn = Connection::session().await?;
    let dbus = DBusProxy::new(&conn).await?;
    let names = dbus.list_names().await?;
    let total_names = names.len();

    let connection_names_vec: Vec<&String> = names.iter().filter(|n| n.starts_with(':')).collect();
    let connection_names = connection_names_vec.len();

    let pid_lookups = connection_names_vec.iter().map(|name| {
        let dbus = &dbus;
        async move {
            let pid = dbus
                .get_connection_unix_process_id(name.as_str())
                .await
                .ok()?;
            Some(((*name).clone(), pid))
        }
    });
    let resolved: Vec<Option<(String, u32)>> = futures_util::future::join_all(pid_lookups).await;

    let mut slack_pids: Vec<(String, u32, String)> = Vec::new();
    for entry in resolved.into_iter().flatten() {
        let (name, pid) = entry;
        let comm = process_name(pid).await.unwrap_or_default();
        if comm == SLACK_PROCESS {
            slack_pids.push((name, pid, comm));
        }
    }

    let mut slack_candidates: Vec<DebugCandidate> = Vec::new();
    let mut chosen: Option<String> = None;
    for (name, pid, comm) in slack_pids {
        let (tooltip, parsed) = match build_sni_proxy(&conn, &name).await {
            Ok(proxy) => {
                match tokio::time::timeout(Duration::from_millis(500), proxy.tool_tip()).await {
                    Ok(Ok(t)) => {
                        let parsed = parse_unread(&t);
                        if chosen.is_none() {
                            chosen = Some(name.clone());
                        }
                        (Ok(t), Some(parsed))
                    }
                    Ok(Err(e)) => (Err(e.to_string()), None),
                    Err(_) => (Err("timed out reading ToolTip property".to_owned()), None),
                }
            }
            Err(e) => (Err(e.to_string()), None),
        };
        slack_candidates.push(DebugCandidate {
            name,
            pid,
            comm,
            tooltip,
            parsed,
        });
    }

    Ok(DebugReport {
        total_names,
        connection_names,
        slack_candidates,
        chosen,
    })
}

fn parse_unread(tooltip: &ToolTip) -> Unread {
    static NUM_RE: OnceLock<Regex> = OnceLock::new();
    let re = NUM_RE.get_or_init(|| Regex::new(r"\d+").expect("valid regex"));
    let haystack = format!("{} {}", tooltip.2, tooltip.3);

    if let Some(m) = re.find(&haystack)
        && let Ok(n) = m.as_str().parse::<u32>()
        && n > 0
    {
        return Unread::Count(n);
    }

    let lower = haystack.to_lowercase();
    if lower.contains("no unread") || lower.contains("no notification") {
        return Unread::None;
    }
    if lower.contains("unread") || lower.contains("notification") {
        return Unread::Indicator;
    }
    Unread::None
}
