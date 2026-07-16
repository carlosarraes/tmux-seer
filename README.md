# Seer

Seer is a tmux dashboard for seeing which coding agents are working, idle, or waiting for structured input across tmux sessions and explicitly configured SSH hosts.

> **Status:** early development. Binary releases and TPM installation are available.

## What it shows

- Green — working
- Yellow — idle
- Blue — needs structured permission or question input
- Gray — agent detected without usable lifecycle state, or a remote host is offline

`prefix + S` opens the top-right **Seer** dashboard. `prefix + s` replaces tmux's default session tree with a borderless full-screen Seer view. Both group agent panes by host, session, and window; windows and panes without a running agent are omitted. Enter jumps to local targets and turns the current Seer view into an interactive SSH/tmux view for remote targets.

Seer intentionally does not infer blue from conversational prose and does not send input, stop, or restart agents.

Codex hooks remain authoritative once they emit an event. Before the first event of a newly resumed Codex session, Seer uses the live Codex process tree to distinguish idle from active tool execution instead of leaving the pane untracked.

## Requirements

- tmux 3.5 or newer
- Rust 1.89 or newer for source builds
- Linux x86_64 or Apple Silicon macOS for the initial binary builds
- Claude Code, Codex, or Pi for native state hooks
- SSH aliases with non-interactive authentication for cross-host monitoring

## Source installation

To install from source:

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

Download and verify the latest binary release with:

```sh
curl -fsSL https://github.com/carlosarraes/tmux-seer/releases/latest/download/install.sh | sh
```

Set `TMUX_SEER_VERSION=v0.1.0` to pin a release or `TMUX_SEER_INSTALL_PATH` to change the destination from `~/.local/bin/tmux-seer`.

## TPM installation

Add the plugin after status/theme plugins and before TPM's final `run` line:

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

The setup picker detects Claude, Codex, and Pi on each host. After you choose integrations, it leaves the picker and prints an exact unified diff in terminal scrollback. JSON is normalized first so formatting-only differences do not obscure the hook changes. Nothing is written until you explicitly answer `y` to `Apply these changes? [y/N]`; cancellation is the default. Setup preserves unrelated hook configuration and creates backups for changed files. Codex requires reviewing the new definitions through `/hooks` before they run.

## Configuration

| tmux option | Default | Purpose |
|---|---:|---|
| `@seer_key` | `S` | Prefix binding for the quick dashboard |
| `@seer_fullscreen_key` | `s` | Prefix binding for the full-screen dashboard |
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
| `h` / `l` | Jump to the previous or next online host, wrapping at the ends |
| `Tab` | Fold or expand the selected group; from an agent, fold its session |
| `/` | Filter |
| `Enter` | Connect to a remote host or jump to the selected session, window, or agent pane |
| `r` | Rename the selected local or remote session inline |
| `q`, `Esc` | Close |

## Remote hosts

Hosts are never scanned automatically. Seer uses only aliases in `@seer_hosts`, respects your SSH configuration and host-key policy, and uses batch mode with a short connection timeout.

Each remote host needs `~/.local/bin/tmux-seer`. Setup and the standalone installer use the matching release artifact.

Selecting any remote host, session, window, or agent reuses the Seer popup for an interactive remote tmux attachment. Press your tmux prefix followed by `d` to detach and return; the remote tmux session and its agents continue running.

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
