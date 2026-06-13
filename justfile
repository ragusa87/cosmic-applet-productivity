rootdir := ''
prefix := '/usr'

base-dir := absolute_path(clean(rootdir / prefix))
user-base-dir := env('HOME') / '.local'
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')

# Listing recipes is a safer default than building the whole workspace.
default:
    @just --list

# === housekeeping ===

clean:
    cargo clean

# Workspace clippy with pedantic lints (matches the pre-commit hook).
check *args:
    cargo clippy --workspace --all-features {{args}} -- -W clippy::pedantic

# Format the whole workspace with rustfmt.
fix:
    cargo fmt --all

# Trigger an immediate refresh on every running workspace applet (SIGUSR2).
refresh:
    #!/usr/bin/env bash
    set -euo pipefail
    for d in */data; do
        compgen -G "$d/*.desktop" >/dev/null || continue
        pkill -USR2 -f "${d%/data}" || true
    done

# === everyday dev loop ===

# Fast-iterate one applet: release-fast build, user install, restart cosmic-panel.
dev crate:
    cargo build --profile=release-fast -p {{crate}}
    @just _install release-fast {{user-base-dir}} {{crate}}
    -pkill -x cosmic-panel

# Run an applet from source with backtraces (no panel icon — log/settings use only).
run crate *args:
    env RUST_BACKTRACE=full cargo run --release -p {{crate}} {{args}}

# === release / install ===

# Release build + user install into ~/.local (no arg = whole workspace).
release crate='':
    cargo build --release {{ if crate == '' { '' } else { '-p ' + crate } }}
    @just _install release {{user-base-dir}} {{crate}}
    -pkill -x cosmic-panel

# System-wide install into /usr (no rebuild — run `just release` first).
install-system:
    @just _install release {{base-dir}}

# Remove every workspace applet's binary, desktop entry, and icon from /usr.
uninstall-system:
    @just _uninstall {{base-dir}}

# Remove every workspace applet's binary, desktop entry, and icon from ~/.local.
uninstall-user:
    @just _uninstall {{user-base-dir}}

# === internals ===

# Install one applet (when `crate` is given) or every applet that ships a
# desktop file (when `crate` is empty). The appid is discovered from
# `<crate>/data/*.desktop`, so new applets need no justfile changes.
_install profile dest crate='':
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "{{crate}}" ]]; then
        crates=({{crate}})
    else
        crates=()
        for d in */data; do
            compgen -G "$d/*.desktop" >/dev/null && crates+=("${d%/data}")
        done
    fi
    for c in "${crates[@]}"; do
        appid=$(basename "$c"/data/*.desktop .desktop)
        install -Dm0755 "{{cargo-target-dir}}/{{profile}}/$c" "{{dest}}/bin/$c"
        install -Dm0644 "$c/data/$appid.desktop"           "{{dest}}/share/applications/$appid.desktop"
        install -Dm0644 "$c/data/icons/$appid.svg"         "{{dest}}/share/icons/hicolor/scalable/apps/$appid.svg"
    done

_uninstall dest:
    #!/usr/bin/env bash
    set -euo pipefail
    for d in */data; do
        compgen -G "$d/*.desktop" >/dev/null || continue
        c="${d%/data}"
        appid=$(basename "$c"/data/*.desktop .desktop)
        rm -f \
            "{{dest}}/bin/$c" \
            "{{dest}}/share/applications/$appid.desktop" \
            "{{dest}}/share/icons/hicolor/scalable/apps/$appid.svg"
    done
