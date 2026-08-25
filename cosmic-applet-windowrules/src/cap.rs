//! Pure planning logic for the experimental "Cap windows per workspace"
//! mode: at most `max_windows` windows per workspace, no empty-workspace
//! gaps.
//!
//! The planner is a snapshot-driven convergence state machine: `step` is
//! called on every wayland snapshot and returns one small batch of moves
//! (usually a single move). The moves dirty the compositor state, which
//! produces a fresh snapshot, which drives the next step. Planning against
//! fresh state each step keeps us from racing COSMIC's dynamic workspace
//! pruning (a mid-list prune shifts every higher index) and serializes rapid
//! window opens: the next trailing empty workspace only exists once the
//! previous one is occupied.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::wayland::{TlWorkspace, ToplevelSnapshot, WorkspaceSnapshot};

/// User-facing knobs, derived from `Config` by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapOptions {
    /// Maximum windows per workspace (clamped to >= 1).
    pub max_windows: u32,
    /// Never reposition existing windows to enforce the cap; only place new
    /// windows and compact empty-workspace gaps (moving whole groups).
    pub only_place_new: bool,
}

impl Default for CapOptions {
    fn default() -> Self {
        Self {
            max_windows: 1,
            only_place_new: false,
        }
    }
}

/// One planned compositor action, translated by the caller into
/// `WlCommand::MoveToplevelToWorkspace` (+ `ActivateWorkspace` if `activate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMove {
    pub identifier: String,
    pub output: Option<String>,
    pub target_index: u32,
    /// Only set when placing a newly-opened window, never during re-pack.
    pub activate: bool,
}

/// Snapshots a batch stays "in flight" before we assume the compositor
/// silently dropped it (~3s at the wayland thread's 200ms dispatch cycle).
const IN_FLIGHT_TTL: u32 = 15;

#[derive(Debug)]
struct InFlight {
    /// (identifier, target_index) pairs of the emitted batch.
    moves: Vec<(String, u32)>,
    output: Option<String>,
    ttl: u32,
}

#[derive(Debug, Default)]
pub struct CapPlanner {
    /// FIFO of brand-new windows awaiting placement + activation.
    pending_new: VecDeque<String>,
    /// Stable arrival order, used as re-pack tiebreak when several windows
    /// share a workspace (e.g. right after enabling the option).
    first_seen: HashMap<String, u64>,
    next_rank: u64,
    /// Batch issued but not yet observed in a snapshot.
    in_flight: Option<InFlight>,
}

impl CapPlanner {
    /// Record a brand-new toplevel (from `WlEvent::NewToplevel`).
    pub fn note_new(&mut self, identifier: &str) {
        if !self.pending_new.iter().any(|i| i == identifier) {
            self.pending_new.push_back(identifier.to_owned());
        }
    }

    /// Forget all transient state (on option toggle or settings change).
    pub fn reset(&mut self) {
        self.pending_new.clear();
        self.first_seen.clear();
        self.next_rank = 0;
        self.in_flight = None;
    }

    /// Compute the next batch of moves toward the capped layout, or an empty
    /// vec if converged / waiting on the compositor.
    ///
    /// `exempt` holds the windows excluded from planning (never counted,
    /// queued or moved); their workspaces still count as occupied so gap
    /// compaction doesn't collapse groups onto them.
    pub fn step(
        &mut self,
        toplevels: &[ToplevelSnapshot],
        exempt: &[ToplevelSnapshot],
        workspaces: &[WorkspaceSnapshot],
        opts: &CapOptions,
    ) -> Vec<PlannedMove> {
        let max = opts.max_windows.max(1);
        self.gc(toplevels);

        if self.in_flight_pending(toplevels) {
            return Vec::new();
        }

        // Windows with exactly one known workspace participate; empty = the
        // assignment isn't known yet (a follow-up snapshot retries), >1 =
        // sticky, which has no meaningful position to pack.
        let placed: Vec<(&ToplevelSnapshot, &TlWorkspace)> = toplevels
            .iter()
            .filter_map(|t| match t.workspaces.as_slice() {
                [ws] => Some((t, ws)),
                _ => None,
            })
            .collect();

        // Workspaces holding an exempt window are invisible to the cap but
        // not vacant: never an "empty gap" to compact into, never a fresh
        // workspace to place a new window on.
        let anchors: Vec<TlWorkspace> = exempt
            .iter()
            .filter_map(|t| match t.workspaces.as_slice() {
                [ws] => Some(ws.clone()),
                _ => None,
            })
            .collect();

        if let Some(mv) = self.plan_placement(&placed, &anchors, workspaces, max) {
            return vec![mv];
        }
        // Compact/evict only once no new window is waiting: a pending
        // placement will shift the layout anyway.
        if !self.pending_new.is_empty() {
            return Vec::new();
        }
        // The active workspace on each output is anchored too: the user is
        // looking at it — collapsing it, or a window on it, away would
        // strand them on an empty screen.
        let active_by_output: HashMap<Option<String>, u32> = workspaces
            .iter()
            .filter(|w| w.is_active)
            .map(|w| (w.output_name.clone(), w.index))
            .collect();
        let cascade = self.plan_gap_compaction(&placed, &anchors, &active_by_output);
        if !cascade.is_empty() || opts.only_place_new {
            return cascade;
        }
        self.plan_evictions(&placed, workspaces, max)
    }

    /// Drop state for windows that no longer exist; rank newcomers in
    /// snapshot order (covers windows that predate enabling the option).
    fn gc(&mut self, toplevels: &[ToplevelSnapshot]) {
        let alive = |id: &str| -> bool { toplevels.iter().any(|t| t.identifier == id) };
        self.pending_new.retain(|id| alive(id));
        self.first_seen.retain(|id, _| alive(id));
        for t in toplevels {
            if !self.first_seen.contains_key(&t.identifier) {
                self.first_seen.insert(t.identifier.clone(), self.next_rank);
                self.next_rank += 1;
            }
        }
    }

    /// True while a previously-emitted batch hasn't fully shown up in the
    /// snapshot yet (bounded by a TTL so a silently-rejected move can't
    /// wedge us).
    fn in_flight_pending(&mut self, toplevels: &[ToplevelSnapshot]) -> bool {
        let Some(fl) = self.in_flight.as_mut() else {
            return false;
        };
        let landed = fl.moves.iter().all(|(id, target)| {
            match toplevels.iter().find(|t| &t.identifier == id) {
                // Window is gone — nothing left to wait for.
                None => true,
                Some(t) => t.workspaces.iter().any(|w| {
                    w.index == *target && (fl.output.is_none() || w.output_name == fl.output)
                }),
            }
        });
        if landed {
            self.in_flight = None;
            return false;
        }
        fl.ttl -= 1;
        if fl.ttl == 0 {
            tracing::warn!(
                moves = ?fl.moves,
                "cap: batch never observed; giving up and re-planning"
            );
            self.in_flight = None;
            return false;
        }
        true
    }

    /// Place the oldest pending new window. If it already fits where it
    /// opened (count <= max) it's left alone; otherwise it moves to a fresh
    /// workspace — the lowest one that is truly free (no planned window, no
    /// exempt window) — rather than reusing an under-full occupied workspace
    /// or skipping past an empty mid-list workspace (pinned/kept ones sit
    /// below the trailing empty). Waits (returns None without popping) while
    /// no free workspace exists yet, i.e. the compositor hasn't grown the
    /// list.
    fn plan_placement(
        &mut self,
        placed: &[(&ToplevelSnapshot, &TlWorkspace)],
        anchors: &[TlWorkspace],
        workspaces: &[WorkspaceSnapshot],
        max: u32,
    ) -> Option<PlannedMove> {
        while let Some(id) = self.pending_new.front().cloned() {
            // FIFO: if the front window's assignment isn't known yet, wait
            // for a later snapshot rather than skipping ahead of it.
            let (_, current) = placed.iter().find(|(t, _)| t.identifier == id)?;

            let count_at = |index: u32| {
                placed
                    .iter()
                    .filter(|(_, w)| w.output_name == current.output_name && w.index == index)
                    .count() as u32
            };
            let anchored = |index: u32| {
                anchors
                    .iter()
                    .any(|w| w.output_name == current.output_name && w.index == index)
            };

            // Fits where it opened (count includes the window itself): no
            // move, no activation — it opened on the active workspace.
            if count_at(current.index) <= max {
                self.pending_new.pop_front();
                continue;
            }

            // Over the cap where it opened → move to the lowest free
            // workspace. If none is free the compositor hasn't grown the
            // list yet — wait for the next snapshot.
            let target = workspaces
                .iter()
                .filter(|w| w.output_name == current.output_name)
                .map(|w| w.index)
                .filter(|i| count_at(*i) == 0 && !anchored(*i))
                .min()?;
            self.pending_new.pop_front();
            self.in_flight = Some(InFlight {
                moves: vec![(id.clone(), target)],
                output: current.output_name.clone(),
                ttl: IN_FLIGHT_TTL,
            });
            return Some(PlannedMove {
                identifier: id,
                output: current.output_name.clone(),
                target_index: target,
                activate: true,
            });
        }
        None
    }

    /// Enforce the cap (default mode only): a workspace holding more than
    /// `max` windows keeps its first `max` (by arrival rank, then
    /// identifier) and the excess is evicted, one window per step, to the
    /// lowest-index workspace with free capacity. The cap is an upper bound,
    /// not a fill target: under-full workspaces are never merged.
    fn plan_evictions(
        &mut self,
        placed: &[(&ToplevelSnapshot, &TlWorkspace)],
        workspaces: &[WorkspaceSnapshot],
        max: u32,
    ) -> Vec<PlannedMove> {
        for output in outputs_of(placed) {
            let count_at = |index: u32| {
                placed
                    .iter()
                    .filter(|(_, w)| &w.output_name == output && w.index == index)
                    .count() as u32
            };
            let mut indices: Vec<u32> = placed
                .iter()
                .filter(|(_, w)| &w.output_name == output)
                .map(|(_, w)| w.index)
                .collect();
            indices.sort_unstable();
            indices.dedup();
            for idx in indices {
                if count_at(idx) <= max {
                    continue;
                }
                let mut group: Vec<&ToplevelSnapshot> = placed
                    .iter()
                    .filter(|(_, w)| &w.output_name == output && w.index == idx)
                    .map(|(t, _)| *t)
                    .collect();
                group.sort_by(|a, b| {
                    let ra = self
                        .first_seen
                        .get(&a.identifier)
                        .copied()
                        .unwrap_or(u64::MAX);
                    let rb = self
                        .first_seen
                        .get(&b.identifier)
                        .copied()
                        .unwrap_or(u64::MAX);
                    (ra, &a.identifier).cmp(&(rb, &b.identifier))
                });
                let evictee = group[max as usize];
                let target = workspaces
                    .iter()
                    .filter(|w| &w.output_name == output)
                    .map(|w| w.index)
                    .filter(|i| count_at(*i) < max)
                    .min();
                let Some(target) = target else {
                    // No capacity anywhere yet; wait for the compositor to
                    // grow the trailing workspace.
                    continue;
                };
                self.in_flight = Some(InFlight {
                    moves: vec![(evictee.identifier.clone(), target)],
                    output: (*output).clone(),
                    ttl: IN_FLIGHT_TTL,
                });
                return vec![PlannedMove {
                    identifier: evictee.identifier.clone(),
                    output: (*output).clone(),
                    target_index: target,
                    activate: false,
                }];
            }
        }
        Vec::new()
    }

    /// Gap compaction (both modes): never split or merge groups, just close
    /// empty-workspace gaps by moving each whole group down to the lowest
    /// free index below it — stepwise this is the 4→3, 5→4, 6→5 cascade.
    /// "Free" excludes anchored workspaces (exempt-occupied or active), so
    /// groups never stack onto an exempt window's workspace and never hop
    /// over one into its slot. A group sitting on the active workspace is
    /// never moved at all: its windows are what the user is looking at.
    /// The group is emitted as one batch (same source → same lower target),
    /// which is safe against mid-list pruning: the target sits below the
    /// gap, so no prune can shift it while the batch executes.
    fn plan_gap_compaction(
        &mut self,
        placed: &[(&ToplevelSnapshot, &TlWorkspace)],
        anchors: &[TlWorkspace],
        active_by_output: &HashMap<Option<String>, u32>,
    ) -> Vec<PlannedMove> {
        for output in outputs_of(placed) {
            let active = active_by_output.get(output).copied();
            let mut group_indices: Vec<u32> = placed
                .iter()
                .filter(|(_, w)| &w.output_name == output)
                .map(|(_, w)| w.index)
                .collect();
            group_indices.sort_unstable();
            group_indices.dedup();
            let blocked: HashSet<u32> = group_indices
                .iter()
                .copied()
                .chain(
                    anchors
                        .iter()
                        .filter(|w| &w.output_name == output)
                        .map(|w| w.index),
                )
                .chain(active)
                .collect();
            for idx in group_indices {
                if Some(idx) == active {
                    continue;
                }
                let Some(target) = (0..idx).find(|i| !blocked.contains(i)) else {
                    continue;
                };
                // Gap below this group: move the whole group into it.
                let group: Vec<(String, u32)> = placed
                    .iter()
                    .filter(|(_, w)| &w.output_name == output && w.index == idx)
                    .map(|(t, _)| (t.identifier.clone(), target))
                    .collect();
                self.in_flight = Some(InFlight {
                    moves: group.clone(),
                    output: (*output).clone(),
                    ttl: IN_FLIGHT_TTL,
                });
                return group
                    .into_iter()
                    .map(|(identifier, target_index)| PlannedMove {
                        identifier,
                        output: (*output).clone(),
                        target_index,
                        activate: false,
                    })
                    .collect();
            }
        }
        Vec::new()
    }
}

/// Distinct output names present in `placed`, in sorted order (determinism).
fn outputs_of<'a>(placed: &'a [(&ToplevelSnapshot, &TlWorkspace)]) -> Vec<&'a Option<String>> {
    let mut outputs: Vec<&Option<String>> = placed.iter().map(|(_, w)| &w.output_name).collect();
    outputs.sort();
    outputs.dedup();
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl(id: &str, on: &[(&str, u32)]) -> ToplevelSnapshot {
        ToplevelSnapshot {
            identifier: id.into(),
            app_id: format!("app.{id}"),
            title: id.into(),
            workspaces: on
                .iter()
                .map(|(out, idx)| TlWorkspace {
                    output_name: Some((*out).into()),
                    index: *idx,
                })
                .collect(),
        }
    }

    fn ws(output: &str, index: u32) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            name: format!("{}", index + 1),
            index,
            output_name: Some(output.into()),
            is_pinned: false,
            is_active: false,
        }
    }

    fn ws_active(output: &str, index: u32) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            is_active: true,
            ..ws(output, index)
        }
    }

    fn opts1() -> CapOptions {
        CapOptions::default()
    }

    fn opts_max(max: u32) -> CapOptions {
        CapOptions {
            max_windows: max,
            only_place_new: false,
        }
    }

    fn opts_place_only(max: u32) -> CapOptions {
        CapOptions {
            max_windows: max,
            only_place_new: true,
        }
    }

    /// Minimal fake compositor: applies moves and maintains COSMIC's
    /// invariant of exactly one trailing empty workspace per output
    /// (non-trailing empties are pruned, occupied trailing grows the list).
    struct FakeComp {
        toplevels: Vec<ToplevelSnapshot>,
        workspaces: Vec<WorkspaceSnapshot>,
    }

    impl FakeComp {
        fn new(toplevels: Vec<ToplevelSnapshot>, workspaces: Vec<WorkspaceSnapshot>) -> Self {
            let mut c = Self {
                toplevels,
                workspaces,
            };
            c.settle();
            c
        }

        fn apply(&mut self, moves: &[PlannedMove]) {
            for mv in moves {
                let t = self
                    .toplevels
                    .iter_mut()
                    .find(|t| t.identifier == mv.identifier)
                    .expect("move targets a live toplevel");
                t.workspaces = vec![TlWorkspace {
                    output_name: mv.output.clone(),
                    index: mv.target_index,
                }];
            }
            self.settle();
        }

        fn close(&mut self, id: &str) {
            self.toplevels.retain(|t| t.identifier != id);
            self.settle();
        }

        /// Re-establish the dynamic-workspace invariant per output.
        fn settle(&mut self) {
            let mut outputs: Vec<Option<String>> = self
                .workspaces
                .iter()
                .map(|w| w.output_name.clone())
                .collect();
            outputs.sort();
            outputs.dedup();
            for output in outputs {
                // Occupied indices on this output, old ordering preserved.
                let mut old: Vec<u32> = self
                    .workspaces
                    .iter()
                    .filter(|w| w.output_name == output)
                    .map(|w| w.index)
                    .collect();
                old.sort_unstable();
                let occupied: Vec<u32> = old
                    .iter()
                    .copied()
                    .filter(|i| {
                        self.toplevels.iter().any(|t| {
                            t.workspaces
                                .iter()
                                .any(|w| w.output_name == output && w.index == *i)
                        })
                    })
                    .collect();
                // Prune empties, compact indices, add one trailing empty.
                let remap: HashMap<u32, u32> = occupied
                    .iter()
                    .enumerate()
                    .map(|(new, old)| (*old, u32::try_from(new).unwrap()))
                    .collect();
                for t in &mut self.toplevels {
                    for w in &mut t.workspaces {
                        if w.output_name == output {
                            w.index = remap[&w.index];
                        }
                    }
                }
                self.workspaces.retain(|w| w.output_name != output);
                for i in 0..=u32::try_from(occupied.len()).unwrap() {
                    self.workspaces.push(WorkspaceSnapshot {
                        name: format!("{}", i + 1),
                        index: i,
                        output_name: output.clone(),
                        is_pinned: false,
                        is_active: false,
                    });
                }
            }
        }
    }

    /// Drive the planner against the fake compositor until it converges;
    /// returns the sequence of applied moves.
    fn converge(
        planner: &mut CapPlanner,
        comp: &mut FakeComp,
        opts: &CapOptions,
    ) -> Vec<PlannedMove> {
        let mut moves = Vec::new();
        for _ in 0..50 {
            let batch = planner.step(&comp.toplevels, &[], &comp.workspaces, opts);
            if batch.is_empty() {
                // Empty can mean "waiting" (pending/in-flight) — only stop
                // once the planner is truly idle.
                if planner.pending_new.is_empty() && planner.in_flight.is_none() {
                    return moves;
                }
            } else {
                comp.apply(&batch);
                moves.extend(batch);
            }
        }
        panic!("planner did not converge in 50 steps; moves so far: {moves:?}");
    }

    fn layout(comp: &FakeComp) -> Vec<(String, u32)> {
        let mut l: Vec<(String, u32)> = comp
            .toplevels
            .iter()
            .map(|t| (t.identifier.clone(), t.workspaces[0].index))
            .collect();
        l.sort();
        l
    }

    #[test]
    fn new_window_on_occupied_ws_moves_to_trailing_and_activates() {
        // "a" sits on ws 0; "b" opens on ws 0 too.
        let mut comp = FakeComp::new(
            vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])],
            vec![ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        p.note_new("b");

        let batch = p.step(&comp.toplevels, &[], &comp.workspaces, &opts1());
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].identifier, "b");
        assert_eq!(batch[0].target_index, 1);
        assert!(batch[0].activate);
        comp.apply(&batch);
        assert!(converge(&mut p, &mut comp, &opts1()).is_empty());
        assert_eq!(layout(&comp), vec![("a".into(), 0), ("b".into(), 1)]);
    }

    #[test]
    fn new_window_already_alone_is_noop_no_activation() {
        let comp = FakeComp::new(
            vec![tl("a", &[("eDP-1", 0)])],
            vec![ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        p.note_new("a");
        assert!(
            p.step(&comp.toplevels, &[], &comp.workspaces, &opts1())
                .is_empty()
        );
        assert!(
            p.pending_new.is_empty(),
            "placement resolved without a move"
        );
    }

    #[test]
    fn rapid_opens_are_serialized() {
        // "a" occupied ws 0; "b" and "c" both open onto ws 0 in a burst.
        let mut comp = FakeComp::new(
            vec![
                tl("a", &[("eDP-1", 0)]),
                tl("b", &[("eDP-1", 0)]),
                tl("c", &[("eDP-1", 0)]),
            ],
            vec![ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        p.note_new("b");
        p.note_new("c");

        let b1 = p.step(&comp.toplevels, &[], &comp.workspaces, &opts1());
        assert_eq!((b1[0].identifier.as_str(), b1[0].target_index), ("b", 1));
        // Before the compositor reflects the move, nothing more is planned.
        assert!(
            p.step(&comp.toplevels, &[], &comp.workspaces, &opts1())
                .is_empty()
        );
        comp.apply(&b1);

        let b2 = p.step(&comp.toplevels, &[], &comp.workspaces, &opts1());
        assert_eq!((b2[0].identifier.as_str(), b2[0].target_index), ("c", 2));
        assert!(b2[0].activate);
        comp.apply(&b2);
        assert!(converge(&mut p, &mut comp, &opts1()).is_empty());
        assert_eq!(
            layout(&comp),
            vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 2)]
        );
    }

    #[test]
    fn placement_waits_for_trailing_workspace_creation() {
        // Trailing workspace 1 is occupied and the compositor hasn't grown
        // the list yet — the planner must wait, not stack onto ws 1.
        let toplevels = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("b", &[("eDP-1", 1)]),
            tl("c", &[("eDP-1", 0)]),
        ];
        let stale_workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("c");
        assert!(
            p.step(&toplevels, &[], &stale_workspaces, &opts1())
                .is_empty()
        );

        // Compositor catches up and creates trailing ws 2.
        let grown = vec![ws("eDP-1", 0), ws("eDP-1", 1), ws("eDP-1", 2)];
        let batch = p.step(&toplevels, &[], &grown, &opts1());
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("c", 2)
        );
    }

    #[test]
    fn repack_after_middle_close() {
        let mut comp = FakeComp::new(
            vec![
                tl("a", &[("eDP-1", 0)]),
                tl("b", &[("eDP-1", 1)]),
                tl("c", &[("eDP-1", 2)]),
            ],
            vec![
                ws("eDP-1", 0),
                ws("eDP-1", 1),
                ws("eDP-1", 2),
                ws("eDP-1", 3),
            ],
        );
        let mut p = CapPlanner::default();
        assert!(
            converge(&mut p, &mut comp, &opts1()).is_empty(),
            "already packed"
        );

        comp.close("b");
        // The fake compositor prunes the empty middle workspace itself (as
        // COSMIC does), so the layout is already compact again.
        let moves = converge(&mut p, &mut comp, &opts1());
        assert!(moves.iter().all(|m| !m.activate));
        assert_eq!(layout(&comp), vec![("a".into(), 0), ("c".into(), 1)]);
    }

    #[test]
    fn repack_closes_gap_when_compositor_keeps_empty_workspace() {
        // A pinned/kept empty workspace at index 1 — planner must move "c"
        // down rather than leave the gap.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("c", &[("eDP-1", 2)])];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        let batch = p.step(&toplevels, &[], &workspaces, &opts1());
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("c", 1)
        );
        assert!(!batch[0].activate);
    }

    #[test]
    fn converged_layout_is_fixed_point() {
        let comp = FakeComp::new(
            vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 1)])],
            vec![ws("eDP-1", 0), ws("eDP-1", 1), ws("eDP-1", 2)],
        );
        let mut p = CapPlanner::default();
        for _ in 0..5 {
            assert!(
                p.step(&comp.toplevels, &[], &comp.workspaces, &opts1())
                    .is_empty()
            );
        }
    }

    #[test]
    fn enable_with_stacked_windows_packs_stepwise() {
        // Three windows stacked on ws 0 (option just enabled).
        let mut comp = FakeComp::new(
            vec![
                tl("a", &[("eDP-1", 0)]),
                tl("b", &[("eDP-1", 0)]),
                tl("c", &[("eDP-1", 0)]),
            ],
            vec![ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        let moves = converge(&mut p, &mut comp, &opts1());
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| !m.activate));
        assert_eq!(
            layout(&comp),
            vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 2)]
        );
    }

    #[test]
    fn sticky_windows_excluded_and_dont_occupy_slots() {
        // "s" is sticky (on both workspaces); "a" and "b" get packed around it.
        let toplevels = vec![
            tl("s", &[("eDP-1", 0), ("eDP-1", 1)]),
            tl("a", &[("eDP-1", 0)]),
            tl("b", &[("eDP-1", 0)]),
        ];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        let batch = p.step(&toplevels, &[], &workspaces, &opts1());
        // "b" (second on ws 0) moves to ws 1 even though sticky "s" is there.
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("b", 1)
        );
    }

    #[test]
    fn unknown_workspace_assignment_defers() {
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("b");
        assert!(p.step(&toplevels, &[], &workspaces, &opts1()).is_empty());
        assert_eq!(p.pending_new.len(), 1, "still waiting, not dropped");
    }

    #[test]
    fn outputs_planned_independently() {
        let mut comp = FakeComp::new(
            vec![
                tl("a", &[("DP-4", 0)]),
                tl("b", &[("DP-4", 0)]),
                tl("x", &[("eDP-1", 0)]),
                tl("y", &[("eDP-1", 0)]),
            ],
            vec![ws("DP-4", 0), ws("DP-4", 1), ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        let moves = converge(&mut p, &mut comp, &opts1());
        assert_eq!(moves.len(), 2);
        for t in &comp.toplevels {
            let others = comp
                .toplevels
                .iter()
                .filter(|o| o.identifier != t.identifier && o.workspaces == t.workspaces);
            assert_eq!(others.count(), 0, "no two windows share a workspace");
        }
    }

    #[test]
    fn in_flight_suppresses_reemission_and_ttl_expires() {
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("b");
        assert!(!p.step(&toplevels, &[], &workspaces, &opts1()).is_empty());
        // The compositor never applies the move: silence until TTL runs out.
        for _ in 0..(IN_FLIGHT_TTL - 1) {
            assert!(p.step(&toplevels, &[], &workspaces, &opts1()).is_empty());
        }
        // TTL expired → the planner re-plans (b is no longer pending_new, so
        // this surfaces as a re-pack move without activation).
        let batch = p.step(&toplevels, &[], &workspaces, &opts1());
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("b", 1)
        );
        assert!(!batch[0].activate);
    }

    #[test]
    fn repack_moves_never_activate() {
        let mut comp = FakeComp::new(
            vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])],
            vec![ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        let moves = converge(&mut p, &mut comp, &opts1());
        assert!(moves.iter().all(|m| !m.activate));
    }

    #[test]
    fn ordering_tiebreak_is_rank_then_identifier() {
        // "z" was seen before "b"; both on ws 0 → "z" keeps the lower slot
        // despite sorting after "b" alphabetically.
        let first = vec![tl("z", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        assert!(p.step(&first, &[], &workspaces, &opts1()).is_empty());

        let both = vec![tl("z", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])];
        let batch = p.step(&both, &[], &workspaces, &opts1());
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("b", 1)
        );
    }

    #[test]
    fn closed_pending_window_is_garbage_collected() {
        let toplevels = vec![tl("a", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("ghost");
        assert!(p.step(&toplevels, &[], &workspaces, &opts1()).is_empty());
        assert!(
            p.pending_new.is_empty(),
            "vanished window dropped from queue"
        );
    }

    // ---- max_windows > 1 ----

    #[test]
    fn max2_new_window_fits_where_it_opened() {
        // One window on ws 0, cap is 2 → the new window stays put.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("b");
        assert!(
            p.step(&toplevels, &[], &workspaces, &opts_max(2))
                .is_empty()
        );
        assert!(p.pending_new.is_empty());
    }

    #[test]
    fn max2_overflow_always_spawns_new_workspace() {
        // ws 0 holds 3 (over cap), ws 1 holds 1 (has room) — but a new
        // window always spawns a fresh workspace, so target the trailing
        // empty ws 2, never the under-full ws 1.
        let toplevels = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("b", &[("eDP-1", 0)]),
            tl("d", &[("eDP-1", 1)]),
            tl("c", &[("eDP-1", 0)]),
        ];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1), ws("eDP-1", 2)];
        let mut p = CapPlanner::default();
        p.note_new("c");
        let batch = p.step(&toplevels, &[], &workspaces, &opts_max(2));
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("c", 2)
        );
        assert!(batch[0].activate);
    }

    #[test]
    fn max2_eviction_spreads_excess_windows() {
        // Four windows stacked on ws 0 with cap 2 → a,b stay on 0; the
        // excess (c, d) is evicted to ws 1.
        let mut comp = FakeComp::new(
            vec![
                tl("a", &[("eDP-1", 0)]),
                tl("b", &[("eDP-1", 0)]),
                tl("c", &[("eDP-1", 0)]),
                tl("d", &[("eDP-1", 0)]),
            ],
            vec![ws("eDP-1", 0), ws("eDP-1", 1)],
        );
        let mut p = CapPlanner::default();
        let moves = converge(&mut p, &mut comp, &opts_max(2));
        assert!(moves.iter().all(|m| !m.activate));
        assert_eq!(
            layout(&comp),
            vec![
                ("a".into(), 0),
                ("b".into(), 0),
                ("c".into(), 1),
                ("d".into(), 1)
            ]
        );
    }

    #[test]
    fn max2_underfull_workspaces_are_never_merged() {
        // The cap is an upper bound, not a fill target: two half-full
        // workspaces stay as they are.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 1)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1), ws("eDP-1", 2)];
        let mut p = CapPlanner::default();
        for _ in 0..3 {
            assert!(
                p.step(&toplevels, &[], &workspaces, &opts_max(2))
                    .is_empty()
            );
        }
    }

    #[test]
    fn default_mode_gap_cascade_preserves_groups() {
        // Default mode, cap 2: the pair on ws 2 shifts down to the gap at
        // ws 1 as one batch — grouping intact, no redistribution.
        let toplevels = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("d", &[("eDP-1", 2)]),
            tl("e", &[("eDP-1", 2)]),
        ];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        let batch = p.step(&toplevels, &[], &workspaces, &opts_max(2));
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(|m| m.target_index == 1 && !m.activate));
    }

    // ---- only_place_new ----

    #[test]
    fn place_only_respects_manual_stacking() {
        // Two windows manually stacked on ws 0 with cap 1: no re-pack.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        for _ in 0..3 {
            assert!(
                p.step(&toplevels, &[], &workspaces, &opts_place_only(1))
                    .is_empty()
            );
        }
    }

    #[test]
    fn place_only_still_places_new_windows() {
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("b");
        let batch = p.step(&toplevels, &[], &workspaces, &opts_place_only(1));
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("b", 1)
        );
        assert!(batch[0].activate);
    }

    #[test]
    fn place_only_gap_compaction_moves_whole_group() {
        // ws 1 is an empty gap; the group (d, e) on ws 2 moves down to ws 1
        // together — one batch, grouping preserved.
        let toplevels = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("d", &[("eDP-1", 2)]),
            tl("e", &[("eDP-1", 2)]),
        ];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        let batch = p.step(&toplevels, &[], &workspaces, &opts_place_only(1));
        assert_eq!(batch.len(), 2);
        let mut ids: Vec<&str> = batch.iter().map(|m| m.identifier.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["d", "e"]);
        assert!(batch.iter().all(|m| m.target_index == 1 && !m.activate));

        // Once the batch lands, the layout is a fixed point.
        let landed = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("d", &[("eDP-1", 1)]),
            tl("e", &[("eDP-1", 1)]),
        ];
        let after = vec![ws("eDP-1", 0), ws("eDP-1", 1), ws("eDP-1", 2)];
        assert!(p.step(&landed, &[], &after, &opts_place_only(1)).is_empty());
        assert!(p.step(&landed, &[], &after, &opts_place_only(1)).is_empty());
    }

    // ---- active-workspace & exempt-window anchoring ----

    #[test]
    fn new_window_on_active_workspace_is_not_pulled_down() {
        // The reported bug: user sits on empty ws 2 (active) with ws 0/1
        // also empty, opens a terminal there — it must stay put, not be
        // gap-compacted to ws 0 (without activation) while the user keeps
        // staring at an empty ws 2.
        let toplevels = vec![tl("term", &[("eDP-1", 2)])];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws("eDP-1", 1),
            ws_active("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        p.note_new("term");
        for _ in 0..3 {
            assert!(
                p.step(&toplevels, &[], &workspaces, &opts_place_only(2))
                    .is_empty()
            );
        }
        assert!(p.pending_new.is_empty(), "placement resolved in place");
    }

    #[test]
    fn group_on_active_workspace_is_never_moved() {
        // Same layout but the window predates the planner (no pending_new):
        // a group on the active workspace is exempt from compaction too.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 2)])];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws("eDP-1", 1),
            ws_active("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        for _ in 0..3 {
            assert!(p.step(&toplevels, &[], &workspaces, &opts1()).is_empty());
        }
    }

    #[test]
    fn empty_active_workspace_is_not_a_gap() {
        // User deliberately went to empty ws 1: the group on ws 2 must not
        // collapse down onto the workspace the user is looking at.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("c", &[("eDP-1", 2)])];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws_active("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        for _ in 0..3 {
            assert!(p.step(&toplevels, &[], &workspaces, &opts1()).is_empty());
        }
    }

    #[test]
    fn exempt_occupied_workspace_is_not_a_gap() {
        // ws 1 holds only an exempt window (e.g. zoom). It is invisible to
        // the cap but its workspace is occupied: "b" on ws 2 must neither
        // stack onto it nor hop over it into its slot.
        let exempt = vec![tl("zoom", &[("eDP-1", 1)])];
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 2)])];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        for _ in 0..3 {
            assert!(
                p.step(&toplevels, &exempt, &workspaces, &opts1())
                    .is_empty()
            );
        }
    }

    #[test]
    fn overflow_fills_empty_mid_list_workspace_not_trailing() {
        // The firefox report: user on ws 1 where firefox is the 3rd window
        // (over cap 2); ws 2 is empty but sits mid-list (pinned/kept), ws 3
        // is the trailing empty. Firefox must land on ws 2, not skip to 3.
        let toplevels = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("b", &[("eDP-1", 1)]),
            tl("c", &[("eDP-1", 1)]),
            tl("ff", &[("eDP-1", 1)]),
        ];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws_active("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        p.note_new("ff");
        let batch = p.step(&toplevels, &[], &workspaces, &opts_max(2));
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("ff", 2)
        );
        assert!(batch[0].activate);
    }

    #[test]
    fn placement_skips_exempt_occupied_workspace() {
        // Same layout, but the empty-looking ws 2 actually holds an exempt
        // window (e.g. zoom): the new window goes to ws 3 instead.
        let exempt = vec![tl("zoom", &[("eDP-1", 2)])];
        let toplevels = vec![
            tl("a", &[("eDP-1", 0)]),
            tl("b", &[("eDP-1", 1)]),
            tl("c", &[("eDP-1", 1)]),
            tl("ff", &[("eDP-1", 1)]),
        ];
        let workspaces = vec![
            ws("eDP-1", 0),
            ws_active("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        p.note_new("ff");
        let batch = p.step(&toplevels, &exempt, &workspaces, &opts_max(2));
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("ff", 3)
        );
    }

    #[test]
    fn gap_behind_the_user_still_compacts() {
        // Active anchor doesn't disable compaction elsewhere: user on ws 0,
        // empty gap at ws 1, group on ws 2 → moves down to 1, no activation.
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("c", &[("eDP-1", 2)])];
        let workspaces = vec![
            ws_active("eDP-1", 0),
            ws("eDP-1", 1),
            ws("eDP-1", 2),
            ws("eDP-1", 3),
        ];
        let mut p = CapPlanner::default();
        let batch = p.step(&toplevels, &[], &workspaces, &opts1());
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("c", 1)
        );
        assert!(!batch[0].activate);
    }

    #[test]
    fn max_zero_is_clamped_to_one() {
        let toplevels = vec![tl("a", &[("eDP-1", 0)]), tl("b", &[("eDP-1", 0)])];
        let workspaces = vec![ws("eDP-1", 0), ws("eDP-1", 1)];
        let mut p = CapPlanner::default();
        p.note_new("b");
        let batch = p.step(&toplevels, &[], &workspaces, &opts_max(0));
        assert_eq!(
            (batch[0].identifier.as_str(), batch[0].target_index),
            ("b", 1)
        );
    }
}
