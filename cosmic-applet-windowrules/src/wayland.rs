use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use bitflags::bitflags;
use cosmic::iced::futures::{SinkExt, channel::mpsc as iced_mpsc};
use cosmic_client_toolkit::{
    GlobalData,
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState, ToplevelUserData},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    workspace::{WorkspaceHandler, WorkspaceState},
};
use cosmic_protocols::{
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    workspace::v2::client::zcosmic_workspace_handle_v2,
};
use smithay_client_toolkit::{
    output::{OutputHandler, OutputState},
    reexports::{calloop, calloop_wayland_source::WaylandSource},
    registry::{ProvidesRegistryState, RegistryState},
};
use wayland_client::{
    Connection, QueueHandle, WEnum, globals::registry_queue_init, protocol::wl_output,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1;

/// Snapshot of a workspace exposed to the UI/applet side. Cheap to clone.
#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub name: String,
    pub index: u32,
    pub output_name: Option<String>,
    pub is_pinned: bool,
}

/// Snapshot of a window exposed to the UI/applet side.
#[derive(Debug, Clone)]
pub struct ToplevelSnapshot {
    pub identifier: String,
    pub app_id: String,
    pub title: String,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ManagerCaps: u32 {
        const CLOSE                 = 1 << 0;
        const ACTIVATE              = 1 << 1;
        const MAXIMIZE              = 1 << 2;
        const MINIMIZE              = 1 << 3;
        const FULLSCREEN            = 1 << 4;
        const MOVE_TO_WORKSPACE     = 1 << 5;
        const STICKY                = 1 << 6;
        const MOVE_TO_EXT_WORKSPACE = 1 << 7;
    }
}

/// Identifier the iced side uses to reference a workspace when sending
/// commands. Holding raw Wayland proxy handles in `Message`s is awkward, so we
/// resolve them inside the wayland thread.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkspaceRef {
    Name(String),
    Index(u32),
}

/// Same idea for toplevels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToplevelRef(pub String); // identifier

#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)] // each variant operates on a workspace; the shared suffix is part of the meaning
pub enum WlCommand {
    MoveToplevelToWorkspace {
        toplevel: ToplevelRef,
        workspace: WorkspaceRef,
        /// Output name (e.g. `"DP-4"`) the target workspace must live on.
        /// `None` means "any output" (first match wins, legacy behaviour).
        output: Option<String>,
    },
    /// Switch the active workspace to the target one (no toplevel involved).
    ActivateWorkspace {
        workspace: WorkspaceRef,
        output: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum WlEvent {
    /// Sent once the wayland thread has bound the globals.
    Ready { caps: ManagerCaps, cmd_tx: WlSender },
    /// Initial state plus any subsequent change.
    Snapshot {
        caps: ManagerCaps,
        workspaces: Vec<WorkspaceSnapshot>,
        toplevels: Vec<ToplevelSnapshot>,
    },
    /// A brand new toplevel has appeared. The applet should evaluate rules.
    NewToplevel(ToplevelSnapshot),
}

/// Cheap, cloneable handle that lets the applet send commands into the
/// wayland thread. Internally an `Arc<Mutex<calloop::channel::Sender>>`
/// because `calloop::channel::Sender` is not `Sync`.
#[derive(Clone)]
pub struct WlSender(Arc<Mutex<calloop::channel::Sender<WlCommand>>>);

impl WlSender {
    pub fn send(&self, cmd: WlCommand) {
        if let Ok(s) = self.0.lock()
            && let Err(e) = s.send(cmd)
        {
            tracing::warn!(error = %e, "wayland command channel closed");
        }
    }
}

impl std::fmt::Debug for WlSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WlSender")
    }
}

/// Spawn the wayland thread and return an iced subscription stream of events.
pub fn run() -> impl cosmic::iced::futures::Stream<Item = WlEvent> {
    cosmic::iced::stream::channel(32, |mut out: iced_mpsc::Sender<WlEvent>| async move {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<WlEvent>();
        let (cmd_tx, cmd_rx) = calloop::channel::channel::<WlCommand>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();

        if let Err(e) = std::thread::Builder::new()
            .name("windowrules-wayland".into())
            .spawn(move || {
                if let Err(e) = wayland_main(event_tx, cmd_rx, &stop_thread) {
                    tracing::error!(error = %e, "wayland thread exited with error");
                }
            })
        {
            tracing::error!(error = %e, "failed to spawn wayland thread");
            return;
        }

        let sender = WlSender(Arc::new(Mutex::new(cmd_tx)));

        // First message after the thread reports it's ready.
        let mut sent_ready = false;

        while let Some(ev) = event_rx.recv().await {
            // Capture caps from the first Snapshot to synthesize a Ready event
            // before forwarding subsequent traffic. This way the applet stores
            // the WlSender once.
            if !sent_ready && let WlEvent::Snapshot { caps, .. } = &ev {
                sent_ready = true;
                let _ = out
                    .send(WlEvent::Ready {
                        caps: *caps,
                        cmd_tx: sender.clone(),
                    })
                    .await;
            }
            if out.send(ev).await.is_err() {
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
    })
}

// =====================================================================
// Wayland thread internals
// =====================================================================

fn wayland_main(
    event_tx: tokio::sync::mpsc::UnboundedSender<WlEvent>,
    cmd_rx: calloop::channel::Channel<WlCommand>,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let conn =
        Connection::connect_to_env().map_err(|e| anyhow::anyhow!("connect to wayland: {e}"))?;
    let (globals, event_queue) =
        registry_queue_init::<AppData>(&conn).map_err(|e| anyhow::anyhow!("registry init: {e}"))?;
    let qh = event_queue.handle();
    let registry_state = RegistryState::new(&globals);

    let toplevel_info_state = ToplevelInfoState::try_new(&registry_state, &qh)
        .ok_or_else(|| anyhow::anyhow!("ext_foreign_toplevel_list not advertised"))?;
    let toplevel_manager_state = ToplevelManagerState::try_new(&registry_state, &qh)
        .ok_or_else(|| anyhow::anyhow!("zcosmic_toplevel_manager_v1 not advertised"))?;
    let workspace_state = WorkspaceState::new(&registry_state, &qh);
    let output_state = OutputState::new(&globals, &qh);

    let mut app = AppData {
        registry_state,
        toplevel_info_state,
        toplevel_manager_state,
        workspace_state,
        output_state,
        caps: ManagerCaps::empty(),
        outputs_by_proxy: HashMap::new(),
        snapshots_dirty: true,
        sent_initial: false,
        event_tx,
    };

    let mut event_loop: calloop::EventLoop<AppData> =
        calloop::EventLoop::try_new().map_err(|e| anyhow::anyhow!("calloop init: {e}"))?;

    let loop_handle = event_loop.handle();

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .map_err(|e| anyhow::anyhow!("insert wayland source: {e}"))?;

    loop_handle
        .insert_source(cmd_rx, move |ev, (), data: &mut AppData| {
            if let calloop::channel::Event::Msg(cmd) = ev {
                data.handle_command(cmd);
            }
        })
        .map_err(|e| anyhow::anyhow!("insert command source: {e}"))?;

    loop {
        event_loop
            .dispatch(std::time::Duration::from_millis(200), &mut app)
            .map_err(|e| anyhow::anyhow!("dispatch: {e}"))?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Emit a snapshot whenever any handler marked state dirty.
        if app.snapshots_dirty {
            app.snapshots_dirty = false;
            app.emit_snapshot();
        }
    }
    Ok(())
}

struct AppData {
    registry_state: RegistryState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: ToplevelManagerState,
    workspace_state: WorkspaceState,
    output_state: OutputState,
    caps: ManagerCaps,
    outputs_by_proxy: HashMap<wl_output::WlOutput, String>,
    snapshots_dirty: bool,
    sent_initial: bool,
    event_tx: tokio::sync::mpsc::UnboundedSender<WlEvent>,
}

impl AppData {
    fn emit_snapshot(&mut self) {
        let workspaces = collect_workspaces(&self.workspace_state, &self.outputs_by_proxy);
        let toplevels = collect_toplevels(&self.toplevel_info_state);
        let _ = self.event_tx.send(WlEvent::Snapshot {
            caps: self.caps,
            workspaces,
            toplevels,
        });
        self.sent_initial = true;
    }

    fn handle_command(&mut self, cmd: WlCommand) {
        tracing::debug!(?cmd, "wl: command");
        match cmd {
            WlCommand::MoveToplevelToWorkspace {
                toplevel,
                workspace,
                output,
            } => self.move_toplevel(&toplevel, &workspace, output.as_deref()),
            WlCommand::ActivateWorkspace { workspace, output } => {
                self.activate_workspace(&workspace, output.as_deref());
            }
        }
    }

    fn activate_workspace(&self, w_ref: &WorkspaceRef, output_filter: Option<&str>) {
        let Some(workspace) = find_workspace(
            &self.workspace_state,
            &self.outputs_by_proxy,
            w_ref,
            output_filter,
        ) else {
            tracing::info!(reference = ?w_ref, output_filter, "activate: workspace not found");
            return;
        };
        // `activate` lives on the upstream ext_workspace_handle_v1.
        // Capability bit 0 of ext_workspace_handle_v1::WorkspaceCapabilities is `activate`.
        let activate_bit =
            wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1::WorkspaceCapabilities::from_bits_truncate(1);
        if !workspace.capabilities.contains(activate_bit) {
            tracing::info!("workspace does not advertise activate capability");
            return;
        }
        workspace.handle.activate();
        // Same commit() dance as pin — without it the compositor sees the
        // request but treats it as un-committed and never applies it.
        if let Ok(mgr) = self.workspace_state.workspace_manager().get() {
            mgr.commit();
        }
    }

    fn move_toplevel(
        &self,
        t_ref: &ToplevelRef,
        w_ref: &WorkspaceRef,
        output_filter: Option<&str>,
    ) {
        if !self.caps.contains(ManagerCaps::MOVE_TO_EXT_WORKSPACE) {
            // cosmic-comp 1.0.x implements move_to_ext_workspace but its
            // hardcoded capability list (state.rs ~720) forgets to advertise
            // it. The request still works, so we proceed anyway.
            tracing::warn!(
                "move_to_ext_workspace not advertised — attempting anyway \
                 (cosmic-comp omits it from its capability list)"
            );
        }
        let Some(tinfo) = self
            .toplevel_info_state
            .toplevels()
            .find(|t| t.identifier == t_ref.0)
        else {
            tracing::info!(id = %t_ref.0, "toplevel not found at move time");
            return;
        };
        let Some(cosmic_toplevel) = tinfo.cosmic_toplevel.as_ref() else {
            tracing::warn!("toplevel has no cosmic handle");
            return;
        };
        let Some(workspace) = find_workspace(
            &self.workspace_state,
            &self.outputs_by_proxy,
            w_ref,
            output_filter,
        ) else {
            tracing::info!(
                reference = ?w_ref,
                output_filter,
                "target workspace not found"
            );
            return;
        };
        if tinfo.workspace.contains(&workspace.handle) {
            tracing::info!(
                id = %t_ref.0,
                workspace = %workspace.name,
                "toplevel already on target workspace; skipping move"
            );
            return;
        }
        let group = self
            .workspace_state
            .workspace_groups()
            .find(|g| g.workspaces.contains(&workspace.handle));
        let Some(group) = group else {
            tracing::warn!("workspace has no group");
            return;
        };
        let Some(output) = group.outputs.first() else {
            tracing::warn!("workspace group has no output");
            return;
        };
        self.toplevel_manager_state.manager.move_to_ext_workspace(
            cosmic_toplevel,
            &workspace.handle,
            output,
        );
    }
}

#[allow(clippy::mutable_key_type)] // wl_output proxy has interior atomics but is used as a stable identity key
fn collect_workspaces(
    state: &WorkspaceState,
    outputs_by_proxy: &HashMap<wl_output::WlOutput, String>,
) -> Vec<WorkspaceSnapshot> {
    let groups: Vec<_> = state.workspace_groups().collect();
    let mut out = Vec::new();
    for group in &groups {
        let mut ws: Vec<_> = state
            .workspaces()
            .filter(|w| group.workspaces.contains(&w.handle))
            .collect();
        ws.sort_by(|a, b| a.coordinates.cmp(&b.coordinates));
        let output_name = group
            .outputs
            .first()
            .and_then(|o| outputs_by_proxy.get(o).cloned());
        // `pinned` is bit 0 of the cosmic v2 state bitfield. We use
        // `from_bits_truncate(1)` rather than the named `State::Pinned`
        // constant because the bindings are auto-generated and the variant
        // name has shifted across versions.
        let pinned_bit = zcosmic_workspace_handle_v2::State::from_bits_truncate(1);
        for (idx, w) in ws.iter().enumerate() {
            out.push(WorkspaceSnapshot {
                name: w.name.clone(),
                index: u32::try_from(idx).unwrap_or(0),
                output_name: output_name.clone(),
                is_pinned: w.cosmic_state.contains(pinned_bit),
            });
        }
    }
    out
}

fn collect_toplevels(state: &ToplevelInfoState) -> Vec<ToplevelSnapshot> {
    state
        .toplevels()
        .map(|t| ToplevelSnapshot {
            identifier: t.identifier.clone(),
            app_id: t.app_id.clone(),
            title: t.title.clone(),
        })
        .collect()
}

#[allow(clippy::mutable_key_type)] // wl_output proxy keys: see comment on collect_workspaces
fn find_workspace<'a>(
    state: &'a WorkspaceState,
    outputs_by_proxy: &HashMap<wl_output::WlOutput, String>,
    w_ref: &WorkspaceRef,
    output_filter: Option<&str>,
) -> Option<&'a cosmic_client_toolkit::workspace::Workspace> {
    let groups: Vec<_> = state.workspace_groups().collect();
    for group in &groups {
        // If the rule pinned the target to a specific output, skip groups on
        // other outputs so we don't pick a same-named workspace from the wrong
        // monitor.
        if let Some(filter) = output_filter {
            let group_output = group
                .outputs
                .first()
                .and_then(|o| outputs_by_proxy.get(o).map(String::as_str));
            if group_output != Some(filter) {
                continue;
            }
        }
        let mut ws: Vec<_> = state
            .workspaces()
            .filter(|w| group.workspaces.contains(&w.handle))
            .collect();
        ws.sort_by(|a, b| a.coordinates.cmp(&b.coordinates));
        for (idx, w) in ws.iter().enumerate() {
            let hit = match w_ref {
                WorkspaceRef::Name(n) => &w.name == n,
                WorkspaceRef::Index(i) => *i as usize == idx,
            };
            if hit {
                return Some(*w);
            }
        }
    }
    None
}

// =====================================================================
// Handler impls
// =====================================================================

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    smithay_client_toolkit::registry_handlers!(OutputState);
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if let Some(info) = self.output_state.info(&output)
            && let Some(name) = info.name
        {
            self.outputs_by_proxy.insert(output, name);
        }
        self.snapshots_dirty = true;
    }
    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(info) = self.output_state.info(&output)
            && let Some(name) = info.name
        {
            self.outputs_by_proxy.insert(output, name);
        }
        self.snapshots_dirty = true;
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.outputs_by_proxy.remove(&output);
        self.snapshots_dirty = true;
    }
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }
    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            tracing::debug!(
                identifier = %info.identifier,
                app_id = %info.app_id,
                title = %info.title,
                "wl: new_toplevel"
            );
            let snap = ToplevelSnapshot {
                identifier: info.identifier.clone(),
                app_id: info.app_id.clone(),
                title: info.title.clone(),
            };
            let _ = self.event_tx.send(WlEvent::NewToplevel(snap));
        }
        self.snapshots_dirty = true;
    }
    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            tracing::debug!(
                identifier = %info.identifier,
                app_id = %info.app_id,
                title = %info.title,
                workspaces = info.workspace.len(),
                "wl: update_toplevel"
            );
        }
        self.snapshots_dirty = true;
    }
    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            tracing::debug!(
                identifier = %info.identifier,
                app_id = %info.app_id,
                "wl: toplevel_closed"
            );
        }
        self.snapshots_dirty = true;
    }
    fn info_done(&mut self, _: &Connection, _: &QueueHandle<Self>) {
        tracing::debug!("wl: info_done");
        self.snapshots_dirty = true;
    }
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager_state
    }
    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        capabilities: Vec<
            WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>,
        >,
    ) {
        let mut flags = ManagerCaps::empty();
        for cap in capabilities {
            if let WEnum::Value(v) = cap {
                use zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1 as Cap;
                match v {
                    Cap::Close => flags |= ManagerCaps::CLOSE,
                    Cap::Activate => flags |= ManagerCaps::ACTIVATE,
                    Cap::Maximize => flags |= ManagerCaps::MAXIMIZE,
                    Cap::Minimize => flags |= ManagerCaps::MINIMIZE,
                    Cap::Fullscreen => flags |= ManagerCaps::FULLSCREEN,
                    Cap::MoveToWorkspace => flags |= ManagerCaps::MOVE_TO_WORKSPACE,
                    Cap::Sticky => flags |= ManagerCaps::STICKY,
                    Cap::MoveToExtWorkspace => flags |= ManagerCaps::MOVE_TO_EXT_WORKSPACE,
                    _ => {}
                }
            }
        }
        tracing::debug!(?flags, "wl: manager capabilities");
        self.caps = flags;
        self.snapshots_dirty = true;
    }
}

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }
    fn done(&mut self) {
        tracing::debug!(
            workspaces = self.workspace_state.workspaces().count(),
            groups = self.workspace_state.workspace_groups().count(),
            "wl: workspace_done"
        );
        self.snapshots_dirty = true;
    }
}

// Tell sctk we don't process zcosmic_toplevel_handle_v1 directly — the
// toolkit's ToplevelInfoState handles it. delegate_toplevel_info! wires that
// up. Same for the other delegate macros.
smithay_client_toolkit::delegate_output!(AppData);
smithay_client_toolkit::delegate_registry!(AppData);
cosmic_client_toolkit::delegate_toplevel_info!(AppData);
cosmic_client_toolkit::delegate_toplevel_manager!(AppData);
cosmic_client_toolkit::delegate_workspace!(AppData);

// Suppress unused warning — ToplevelUserData lives behind delegate macros.
#[allow(dead_code)]
fn _phantom(_: ToplevelUserData, _: GlobalData) {}
