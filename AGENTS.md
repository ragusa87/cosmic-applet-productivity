# AGENTS.md

Notes for AI coding agents (and humans new to the codebase). The README is the
user-facing doc; this file is the *contributor*-facing one.

## What this is

A COSMIC desktop panel applet, written in Rust on libcosmic / iced. It shows
the Gmail unread count in the panel and polls every N seconds (default 60).
Two modes ship in **one binary**, picked by `argv`:

| Mode | Entry | Surface type | Trigger |
|---|---|---|---|
| Panel applet | `cosmic::applet::run::<AppModel>(())` | transparent sub-surface inside the panel | default — no flag |
| Settings window | `cosmic::app::run::<SettingsApp>(Settings, ())` | regular xdg_toplevel | `--show-settings` |

The applet's right-click menu → **Credentials…** spawns `current_exe()` with
`--show-settings`, which is how the user reaches the OAuth setup. Both modes
share `APP_ID = "com.github.ragusa87.CosmicAppletGmail"` so they read/write the same
cosmic-config namespace and the same Secret Service entry.

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
│                  right-click menu popup, polling subscription,
│                  SIGUSR2 listener, token refresh + fetch loop
├── settings.rs    standalone settings app — toplevel window, OAuth flow,
│                  Cancel/Authorize buttons, writes config + tokens, exits
├── ui.rs          shared widgets — menu popup view, credentials form view
│                  (generic over Message via `CredentialsHandlers<M>`),
│                  CredentialsForm + Status types
├── config.rs      cosmic-config schema: email, client_id, poll_interval_secs
├── secrets.rs     keyring wrapper — stores a JSON blob keyed by email under
│                  service "cosmic-applet-gmail:tokens" (sync API wrapped in
│                  spawn_blocking)
├── auth.rs        OAuth 2.0 PKCE + loopback redirect via the `oauth2` crate;
│                  exports `start_oauth_flow` + `refresh`
└── gmail.rs       single GET on users/me/labels/INBOX → messagesUnread
                   (+ unit tests on the JSON parsing path)

data/
├── com.github.ragusa87.CosmicAppletGmail.desktop   panel applet .desktop entry
└── icons/com.github.ragusa87.CosmicAppletGmail.svg Gmail-red envelope (SimpleIcons)
                                            also `include_bytes!`'d into the
                                            binary for the panel button
```

## Storage split

| Item | Where | Reason |
|---|---|---|
| `email`, `client_id`, `poll_interval_secs` | cosmic-config (RON in `~/.config/com.github.ragusa87.CosmicAppletGmail/v1/`) | non-secret, watched live |
| `client_secret`, `refresh_token`, `access_token`, `expires_at_unix` | Secret Service via `keyring` v3, one JSON blob keyed by `email` under service `cosmic-applet-gmail:tokens` | secrets |

Cross-binary propagation: the settings binary writes both. The applet's
`watch_config::<Config>` subscription delivers `Message::UpdateConfig` when
either field changes; the applet then reloads tokens from the keyring and
issues an immediate `Tick`. No IPC.

## SIGUSR2 → force refresh

The applet listens for SIGUSR2 (subscription in `src/app.rs::sigusr2_stream`,
built on `tokio::signal::unix`). On receipt → reloads tokens → fetches.

The settings mode installs `SIG_IGN` for SIGUSR2 at startup so `pkill -USR2
cosmic-applet-gmail` (which would match both modes' processes by name) doesn't
terminate an open settings window. See `src/settings.rs::run`.

Manual trigger: `pkill -USR2 cosmic-applet-gmail`. Watch `RUST_LOG=info` for
"SIGUSR2 received…" to confirm.

## OAuth flow

BYO client_id — the user creates their own Google Cloud OAuth desktop client
and pastes `client_id` + `client_secret` into the settings window. Reason:
shipping a shared client_id would cap us at 100 unverified users. README has
the 5-step Cloud Console walkthrough.

Flow:
1. Bind `127.0.0.1:0` (kernel-picked port).
2. Build the auth URL with PKCE challenge, `access_type=offline`,
   `prompt=consent` (so Google returns a refresh_token), scope
   `gmail.metadata`, plus a random state.
3. `xdg-open` the URL → user consents in their default browser.
4. Compositor redirects to `http://127.0.0.1:PORT/?code=...&state=...`.
   `wait_for_redirect` in `src/auth.rs` parses the request line, returns a
   "you can close this tab" HTML page, validates state, exchanges the code.
5. `refresh()` re-uses the same client to swap a refresh_token for a fresh
   access_token; called automatically on every poll when the cached access
   token is within 30 s of expiry.

Counting endpoint: `users/me/labels/INBOX` → `messagesUnread` integer. One
HTTP call per poll, no message listing.

## Build / run / test commands

```sh
just check          # cargo clippy --all-features -- -W clippy::pedantic
just build-release  # cargo build --release
just install-user   # ~/.local/{bin,share/applications,share/icons/...}
cargo test          # 2 tests in gmail.rs, JSON parsing
```

There is **no automated UI test** — a real COSMIC session is required. After
changes to `view()`, panel layout, or popup logic, install + `pkill
cosmic-applet-gmail` and the panel respawns it. Then:

- Right-click → menu shows "Credentials…"
- Left-click → opens mail.google.com
- `pkill -USR2 cosmic-applet-gmail` → immediate refresh
- `cosmic-applet-gmail --show-settings` from a terminal → settings window
  (useful for UI iteration without rebuilding the panel)

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
  `popup_view` two-layer alignment note.
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
  `text::title4("Gmail credentials")` heading inside the window instead.
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
  `tokio::task::spawn_blocking` (for the sync keyring API).
