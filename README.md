# Seer

Seer is a tmux dashboard for seeing which coding agents are working, idle, or waiting for structured input across tmux sessions and explicitly configured SSH hosts.

> **Status:** pre-release development. Source installation is available now. TPM installation becomes usable after the first GitHub Release contains matching binaries; this README does not imply that a release already exists.

## What it shows

- Green — working
- Yellow — idle
- Blue — needs structured permission or question input
- Gray — agent detected without working hooks, or a remote host is offline

`prefix + S` opens the top-right **Seer** dashboard. It groups agent panes by host, session, and window, and Enter jumps to the selected pane. Remote selections use a reusable local `seer:<host>` SSH bridge window.

Seer intentionally does not infer blue from conversational prose and does not send input, stop, or restart agents.

## Requirements

- tmux 3.5 or newer
- Rust 1.89 or newer for source builds
- Linux x86_64 or Apple Silicon macOS for the initial binary builds
- Claude Code, Codex, or Pi for native state hooks
- SSH aliases with non-interactive authentication for cross-host monitoring

## Source installation

Until a release exists, install from source:

```sh
git clone https://github.com/carlosarraes/tmux-seer.git
cd tmux-seer
cargo build --release
mkdir -p ~/.local/bin
install -m 0755 target/release/tmux-seer ~/.local/bin/tmux-seer
tmux-seer bootstrap
tmux-seer setup
```

Ensure `~/.local/bin` is on the PATH inherited by tmux and your agents. Reload tmux after installation.

With [`just`](https://github.com/casey/just) installed, the build and copy steps are also available as:

```sh
just build
tmux-seer bootstrap
tmux-seer setup
```

## Binary installation

After the first release, download and verify the matching binary with:

```sh
curl -fsSL https://github.com/carlosarraes/tmux-seer/releases/latest/download/install.sh | sh
```

Set `TMUX_SEER_VERSION=v0.1.0` to pin a release or `TMUX_SEER_INSTALL_PATH` to change the destination from `~/.local/bin/tmux-seer`.

## TPM installation

After the first release is published, add the plugin after status/theme plugins and before TPM's final `run` line:

```tmux
set -g @plugin 'carlosarraes/tmux-seer'

# Optional SSH aliases. Empty by default.
set -g @seer_hosts 'mac zapsign'

run '~/.tmux/plugins/tpm/tpm'
```

Press `prefix + I`. The root `tmux-seer.tmux` entrypoint downloads the matching checksummed release binary and bootstraps Seer without replacing the existing status theme.

Then run:

```sh
tmux-seer setup
tmux-seer doctor
```

The setup picker detects Claude, Codex, and Pi on each host, previews selected changes, preserves unrelated hook configuration, and creates backups for changed files. Codex requires reviewing the new definitions through `/hooks` before they run.

## Configuration

| tmux option | Default | Purpose |
|---|---:|---|
| `@seer_key` | `S` | Prefix binding for the dashboard |
| `@seer_hosts` | empty | Space-separated SSH aliases |
| `@seer_popup_width` | `76` | Popup columns |
| `@seer_popup_height` | `70%` | Popup height |
| `@seer_remote_interval_ms` | `2000` | Remote refresh interval |
| `@seer_notify_ms` | `4000` | tmux message duration |
| `@seer_binary` | auto | Explicit binary path override |

Status-square colors default to `#9ece6a`, `#e0af68`, `#7aa2f7`, and `#565f89` through the corresponding `@seer_color_*` options.

## Dashboard keys

| Key | Action |
|---|---|
| `↑` / `↓`, `j` / `k` | Move |
| `Tab` | Fold or expand a group |
| `/` | Filter |
| `Enter` | Jump to an agent pane |
| `q`, `Esc` | Close |

## Remote hosts

Hosts are never scanned automatically. Seer uses only aliases in `@seer_hosts`, respects your SSH configuration and host-key policy, and uses batch mode with a short connection timeout.

Each remote host needs `~/.local/bin/tmux-seer`. Before the first release, build and install it from source on that host. Once releases exist, setup and the standalone installer use the matching release artifact.

## Uninstalling agent hooks

```sh
tmux-seer setup --uninstall
```

The picker removes only Seer-owned JSON hook handlers and the Seer-managed Pi extension. Remove the TPM plugin entry separately with TPM's normal uninstall command.

## Development

```sh
just check
```

Maintainers can run `just release 0.1.0` from a clean tree. It checks the project, updates the Cargo package version, creates a release commit and annotated tag, then pushes both. GitHub Actions builds the supported archives, writes checksums, and publishes them with the standalone installer.

No license has been selected yet. No license is implied by the public repository.
