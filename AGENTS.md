# AGENTS.md

Notes for AI coding agents (and humans new to the codebase). The README is the
user-facing doc; this file is the *contributor*-facing one.

## What this is

A COSMIC desktop panel applet, written in Rust on libcosmic / iced. It shows
the **next Google Calendar event** with a live countdown and fires a desktop
notification a few minutes before it starts. Two modes ship in **one binary**,
picked by `argv`:

| Mode | Entry | Surface type | Trigger |
|---|---|---|---|
| Panel applet | `cosmic::applet::run::<AppModel>(())` | transparent sub-surface inside the panel | default — no flag |
| Settings window | `cosmic::app::run::<SettingsApp>(Settings, ())` | regular xdg_toplevel | `--show-settings` |
| CLI debug dump | `debug::run()` (tokio current-thread, no iced) | stdout only | `--debug` |

The applet's right-click menu → **Credentials…** spawns `current_exe()` with
`--show-settings`, which is how the user reaches the OAuth setup. Both modes
share `APP_ID = "io.github.cosmic_google_agenda_panel"` so they read/write the
same cosmic-config namespace and the same Secret Service entry.

## Why two modes, not two binaries

A `cosmic::applet::run` process is constrained: every surface it creates
(including `surface::action::app_window`) is rendered as a transparent
sub-surface embedded in the panel. Real toplevels with WM chrome require
`cosmic::app::run`. The two entry points are incompatible in the same
process, but a single binary can dispatch to either based on `argv` — saves
maintaining two installs and two `.desktop` files. See `src/main.rs`.

## File layout

```
src/
├── main.rs        argv check → applet::run or app::run (settings)
├── app.rs         panel applet — Application impl, panel button view,
│                  right-click menu popup, two timer subscriptions
│                  (display 30s, fetch 5min), SIGUSR2 listener, token
│                  refresh + fetch loop, notification dispatch
├── settings.rs    standalone settings app — toplevel window, OAuth flow,
│                  Cancel/Authorize buttons, writes config + tokens, exits
├── debug.rs       --debug CLI mode — prints config, loads tokens, refreshes
│                  if needed, calls calendar::debug_fetch, dumps every event
│                  with KEEP/SKIP verdict. No GUI. Spins its own tokio
│                  current-thread runtime since libcosmic isn't loaded.
├── ui.rs          shared widgets — menu popup view, credentials form view
│                  (generic over Message via `CredentialsHandlers<M>`),
│                  CredentialsForm + Status types
├── config.rs      cosmic-config schema: email, client_id,
│                  fetch_interval_secs, display_tick_secs,
│                  notification_lead_secs, show_title,
│                  show_progress
├── secrets.rs     keyring wrapper — stores a JSON blob keyed by email under
│                  service "cosmic-google-agenda-panel:tokens" (sync API
│                  wrapped in spawn_blocking)
├── auth.rs        OAuth 2.0 PKCE + loopback redirect via the `oauth2`
│                  crate; exports `start_oauth_flow` + `refresh`.
│                  Scope: calendar.events.readonly
└── calendar.rs    GET on /calendar/v3/calendars/primary/events
                   → Vec<Event> (id, summary, start, end, meet_url).
                   Filters cancelled / all-day / transparent / declined.
                   Also exposes `debug_fetch` → Vec<DebugItem> (every raw
                   event + KEEP/SKIP verdict + reason) for the --debug mode.
                   (+ unit tests on the JSON parsing path)

data/
├── io.github.cosmic_google_agenda_panel.desktop   panel applet .desktop entry
└── icons/io.github.cosmic_google_agenda_panel.svg Google Calendar blue grid
                                                   also `include_bytes!`'d
                                                   into the binary for the
                                                   panel button
```

## Storage split

| Item | Where | Reason |
|---|---|---|
| `email`, `client_id`, `fetch_interval_secs`, `display_tick_secs`, `notification_lead_secs`, `show_title`, `show_progress` | cosmic-config (RON in `~/.config/io.github.cosmic_google_agenda_panel/v1/`) | non-secret, watched live |
| `client_secret`, `refresh_token`, `access_token`, `expires_at_unix` | Secret Service via `keyring` v3, one JSON blob keyed by `email` under service `cosmic-google-agenda-panel:tokens` | secrets |

Cross-binary propagation: the settings binary writes both. The applet's
`watch_config::<Config>` subscription delivers `Message::UpdateConfig` when
either field changes; the applet then reloads tokens from the keyring and
issues an immediate `Refetch`. No IPC.

## Two timers: display vs. fetch

`AppModel` caches the event list in `self.events` and runs two independent
timer subscriptions, batched in `subscription()`:

- **display tick** (default 30s) → `Message::Tick`. Pure local recompute:
  drops events whose end is in the past from the cache, picks `self.next`,
  recomputes the relative-time string for `view()`, and fires
  `maybe_notify` (one-shot per event id, tracked in `self.notified`).
- **fetch tick** (default 5min) → `Message::Refetch`. Refreshes the access
  token if needed, then calls `calendar::upcoming_events` and replaces
  `self.events`. Chains an immediate `Tick` so the display updates.

Network blips therefore only delay the next *refetch* — the countdown
continues smoothly from cached events. `notified` is pruned on every Tick
to drop ids no longer in the upcoming window, so recurring meetings notify
again the next day.

## SIGUSR2 → force refresh

The applet listens for SIGUSR2 (subscription in `src/app.rs::sigusr2_stream`,
built on `tokio::signal::unix`). On receipt → `Message::Refetch`.

The settings mode installs `SIG_IGN` for SIGUSR2 at startup so
`pkill -USR2 cosmic-google-agenda-panel` (which would match both modes'
processes by name) doesn't terminate an open settings window. See
`src/settings.rs::run`.

Manual trigger: `pkill -USR2 cosmic-google-agenda-panel`. Watch
`RUST_LOG=info` for "SIGUSR2 received…" to confirm.

## OAuth flow

BYO client_id — the user creates their own Google Cloud OAuth desktop client
and pastes `client_id` + `client_secret` into the settings window. Reason:
shipping a shared client_id would cap us at 100 unverified users. README has
the 5-step Cloud Console walkthrough.

Flow:
1. Bind `127.0.0.1:0` (kernel-picked port).
2. Build the auth URL with PKCE challenge, `access_type=offline`,
   `prompt=consent` (so Google returns a refresh_token), scope
   `calendar.events.readonly`, plus a random state.
3. `xdg-open` the URL → user consents in their default browser.
4. Compositor redirects to `http://127.0.0.1:PORT/?code=...&state=...`.
   `wait_for_redirect` in `src/auth.rs` parses the request line, returns a
   "you can close this tab" HTML page, validates state, exchanges the code.
5. `refresh()` re-uses the same client to swap a refresh_token for a fresh
   access_token; called automatically on every fetch when the cached access
   token is within 30 s of expiry.

API endpoint: `users/me/calendars/primary/events?timeMin=...&timeMax=...
&singleEvents=true&orderBy=startTime`. One HTTP call per fetch interval.

## Event filtering rules (in `src/calendar.rs::classify`)

Applied to the raw API response, in order:

1. Drop `status == "cancelled"`.
2. Drop `transparency == "transparent"` ("Free"-marked).
3. Drop self-declined: an attendee with `self == true` and
   `responseStatus == "declined"`.
4. Drop all-day (`start.date` present, `start.dateTime` missing).

`classify` returns `Result<DateTime<Utc>, SkipReason>`. The applet uses
`map_event` (`classify(...).ok() → build_event`) to drop skipped events
silently; the `--debug` CLI uses `to_debug_item` to print every event with
its verdict so you can see *why* something was filtered.

Meet-link extraction prefers `conferenceData.entryPoints[]` with
`entryPointType == "video"` and `uri` starting `https://meet.google.com/`,
and falls back to the top-level legacy `hangoutLink`.

## Notifications

`maybe_notify` (in `src/app.rs`) is a one-shot per event id: when the next
event's start is within `notification_lead_secs` of now, it inserts the id
into `self.notified` and spawns a `tokio::task::spawn_blocking` that calls
`notify_rust::Notification::show()`. Setting `notification_lead_secs = 0`
disables all notifications.

## Build / run / test commands

```sh
just check          # cargo clippy --all-features -- -W clippy::pedantic
just build-release  # cargo build --release
just install-user   # ~/.local/{bin,share/applications,share/icons/...}
cargo test          # JSON parsing tests in calendar.rs + helper tests in app.rs
```

There is **no automated UI test** — a real COSMIC session is required. After
changes to `view()`, panel layout, or popup logic, install + `pkill
cosmic-google-agenda-panel` and the panel respawns it. Then:

- Right-click → menu shows "Credentials…"
- Left-click → opens Meet link of the next event (or
  `calendar.google.com` fallback)
- `pkill -USR2 cosmic-google-agenda-panel` → immediate refetch
- `cosmic-google-agenda-panel --show-settings` from a terminal → settings
  window (useful for UI iteration without rebuilding the panel)

## Conventions

- **clippy pedantic is mandatory.** `just check` must stay clean. The one
  current `#[allow(clippy::too_many_lines)]` is on `App::update` — keep the
  message dispatch flat; don't split it just to shrink line count.
- **No `unwrap()` or `expect()`** in normal paths. Use `anyhow::Result` for
  fallible work, log with `tracing::warn!(error = %e, ...)` when an error is
  recovered from but worth noting.
- **No comments explaining *what* the code does** — only *why* when it's
  non-obvious (subtle invariant, Wayland quirk, libcosmic-API workaround).
  See e.g. the `LeftClick` guard comment, the `SIG_IGN` rationale, the
  all-day-event filter comment.
- **No docstrings on private items.** Public API of the modules (`pub fn`)
  gets a one-line summary at most.
- **Don't add `derive(Default)` to enums** unless `#[default]` makes sense
  semantically.

## libcosmic 1.0 gotchas (learned the hard way)

- `cosmic::Task<M>` from `cosmic::prelude::*` is `iced::Task<M>` — *not* the
  `iced::Task<Action<M>>` the trait wants. Import `cosmic::app::Task`
  explicitly. The prelude re-export is misleading.
- `cosmic::iced_winit::commands::popup` (referenced in the official template)
  doesn't exist; use `cosmic::surface::action::{app_popup, destroy_popup,
  app_window, destroy_window}` and dispatch them via
  `cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(a)))`.
  The `dispatch_surface` helper in `app.rs` encapsulates this.
- `Application::title(&self, id)` (with the `multi-window` feature) is on
  `ApplicationExt`, which has a *blanket* impl — you cannot override it.
  `core.set_title(id, ...)` exists but returns `Task::none()` (no-op). There
  is currently no public way to set per-window titles; settings shows a
  `text::title4("Google Calendar credentials")` heading inside the window
  instead.
- `keyring` v4 is the deprecated CLI/sample crate. Use `keyring` **v3**
  (`sync-secret-service` + `crypto-rust` features) for the library API.
- `Subscription::run_with_id` (in older templates) is gone; use
  `Subscription::run(fn_pointer)` where the fn pointer's address is the
  identity. For dynamic-stream subscriptions wrap a `cosmic::iced::stream::
  channel(buffer, async closure)` call inside a `fn() -> impl Stream`.
- `text(...).color(Color)` requires `Theme::Class: From<StyleFn>` which
  cosmic's text theme doesn't satisfy. Use `text(...).class(Color::WHITE)`
  instead — `cosmic::theme::Text: From<Color>` works.
- Panel popups with `grab: false` *still* get dismissed by COSMIC when focus
  changes (compositor-side decision, not our flag). The settings window had
  to be a real toplevel (`app_window` from `cosmic::app::run`, NOT from
  inside the applet) for this reason.
- `text` widgets center their glyph inside their line-height box by default.
  To put a glyph at a corner of a container you need `text.align_x(End)
  .align_y(End)` *and* the container's `align_x(Right).align_y(Bottom)` —
  one without the other looks centered. See `view()` in `app.rs`.
- Always use `self.core.applet.suggested_padding(true)` (returns a
  `(major, minor)` tuple) and rotate horizontal vs vertical based on
  `self.core.applet.anchor`. Wrap final widget in
  `self.core.applet.autosize_window(...)` so the panel sizes the surface
  correctly. See `view()`.

## Don't

- Don't write to `target/`, `Cargo.lock`, `data/icons/` from agents without
  asking; these are part of the working state the user iterates on.
- Don't commit. The user asks explicitly when commits are wanted.
- Don't add a second binary (`[[bin]]` entry). The `--show-settings` split
  exists *specifically* to avoid the maintenance cost of two binaries; if
  you find yourself wanting two, ask first.
- Don't change `APP_ID`. The applet and settings binary depend on sharing it
  for cosmic-config + Secret Service.
- Don't introduce a global async runtime — libcosmic / iced own the runtime.
  Async work goes through `cosmic::task::future` or
  `tokio::task::spawn_blocking` (for the sync keyring + notify-rust APIs).
