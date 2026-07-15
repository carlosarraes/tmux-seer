use std::{
    env, fs,
    io::{self, BufRead, Stdout, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};
use serde_json::{json, Map, Value};
use similar::TextDiff;

use crate::{
    daemon::runtime_snapshot_path,
    snapshot::AggregateSnapshot,
    tmux::{now_ms, Tmux},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Integration {
    Claude,
    Codex,
    Pi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupItem {
    pub host: String,
    pub integration: Integration,
    pub available: bool,
    pub configured: bool,
    pub selected: bool,
}

impl SetupItem {
    pub fn detected(
        host: impl Into<String>,
        integration: Integration,
        available: bool,
        configured: bool,
    ) -> Self {
        Self {
            host: host.into(),
            integration,
            available,
            configured,
            selected: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetupModel {
    pub items: Vec<SetupItem>,
    pub cursor: usize,
    pub uninstall: bool,
}

impl SetupModel {
    pub fn new(mut items: Vec<SetupItem>, uninstall: bool) -> Self {
        for item in &mut items {
            item.selected = if uninstall {
                item.configured
            } else {
                item.available && !item.configured
            };
        }
        Self {
            items,
            cursor: 0,
            uninstall,
        }
    }

    pub fn toggle(&mut self) {
        if let Some(item) = self.items.get_mut(self.cursor) {
            if item.available || (self.uninstall && item.configured) {
                item.selected = !item.selected;
            }
        }
    }

    pub fn toggle_all(&mut self) {
        let select = self
            .items
            .iter()
            .any(|item| (item.available || item.configured) && !item.selected);
        for item in &mut self.items {
            if item.available || (self.uninstall && item.configured) {
                item.selected = select;
            }
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.items.len().saturating_sub(1));
    }
}

impl Integration {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Pi];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    pub const fn display(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Pi => "Pi",
        }
    }
}

impl FromStr for Integration {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "pi" => Ok(Self::Pi),
            _ => Err(format!("unsupported integration: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    pub changed: bool,
    pub backup: Option<PathBuf>,
}

pub fn merge_hook_json(
    mut root: Value,
    integration: Integration,
    uninstall: bool,
) -> Result<Value> {
    if integration == Integration::Pi {
        bail!("Pi uses an extension file, not hook JSON");
    }
    if !root.is_object() {
        bail!("hook configuration root must be a JSON object");
    }
    let hooks = root
        .as_object_mut()
        .expect("validated object")
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .context("the `hooks` property must be a JSON object")?;
    remove_owned_hooks(hooks, integration.slug())?;

    if !uninstall {
        for &(event, matcher) in integration_events(integration) {
            let groups = hooks
                .entry(event)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .with_context(|| format!("hooks.{event} must be an array"))?;
            let mut group = Map::new();
            if let Some(matcher) = matcher {
                group.insert("matcher".into(), Value::String(matcher.into()));
            }
            let mut command = format!("tmux-seer hook {} {event}", integration.slug());
            if let Some(matcher) = matcher {
                command.push_str(&format!(" --matcher {matcher}"));
            }
            group.insert(
                "hooks".into(),
                Value::Array(vec![json!({
                    "type": "command",
                    "command": command,
                    "timeout": 5,
                    "statusMessage": "Updating Seer"
                })]),
            );
            groups.push(Value::Object(group));
        }
    }
    Ok(root)
}

pub fn preview_integration_change(
    original_bytes: &[u8],
    integration: Integration,
    uninstall: bool,
    target: &str,
) -> Result<String> {
    let (before, after) = match integration {
        Integration::Claude | Integration::Codex => {
            let original: Value = serde_json::from_slice(original_bytes)
                .with_context(|| format!("{target} contains malformed JSON"))?;
            let merged = merge_hook_json(original.clone(), integration, uninstall)?;
            (pretty_json(&original)?, pretty_json(&merged)?)
        }
        Integration::Pi => {
            let original = std::str::from_utf8(original_bytes)
                .with_context(|| format!("{target} contains non-UTF-8 data"))?;
            (
                original.to_owned(),
                merge_pi_extension(original, uninstall, target)?,
            )
        }
    };

    if before == after {
        return Ok(format!("No changes for {target}\n"));
    }
    let before_label = match integration {
        Integration::Claude | Integration::Codex => format!("{target} (normalized)"),
        Integration::Pi => format!("{target} (current)"),
    };
    let after_label = format!("{target} (after)");
    Ok(TextDiff::from_lines(&before, &after)
        .unified_diff()
        .context_radius(1)
        .header(&before_label, &after_label)
        .to_string())
}

pub fn confirm_apply(input: impl BufRead, mut output: impl Write) -> Result<bool> {
    write!(output, "Apply these changes? [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    let mut input = input;
    input.read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn pretty_json(value: &Value) -> Result<String> {
    let mut rendered = serde_json::to_string_pretty(value)?;
    rendered.push('\n');
    Ok(rendered)
}

pub fn apply_json_integration(
    path: &Path,
    integration: Integration,
    uninstall: bool,
) -> Result<ApplyResult> {
    let existed = path.exists();
    let original_bytes = if existed {
        fs::read(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        b"{}".to_vec()
    };
    let original: Value = serde_json::from_slice(&original_bytes)
        .with_context(|| format!("{} contains malformed JSON", path.display()))?;
    let merged = merge_hook_json(original.clone(), integration, uninstall)?;
    if merged == original {
        return Ok(ApplyResult {
            changed: false,
            backup: None,
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup = existed.then(|| unique_backup(path)).transpose()?;
    let temporary = path.with_extension("seer.tmp");
    let mut output = serde_json::to_vec_pretty(&merged)?;
    output.push(b'\n');
    fs::write(&temporary, output)?;
    if existed {
        let permissions = fs::metadata(path)?.permissions();
        fs::set_permissions(&temporary, permissions)?;
    }
    fs::rename(&temporary, path)?;
    Ok(ApplyResult {
        changed: true,
        backup,
    })
}

pub fn pi_extension_source() -> &'static str {
    r#"// Managed by tmux-seer. Changes may be replaced by `tmux-seer setup`.
import { spawn } from "node:child_process";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

const questionTools = new Set(["question", "request_user_input", "ask_user"]);

function emit(ctx: ExtensionContext, event: string, matcher?: string): void {
  const args = ["hook", "pi", event];
  if (matcher) args.push("--matcher", matcher);
  const child = spawn("tmux-seer", args, { stdio: ["pipe", "ignore", "ignore"] });
  child.stdin.end(JSON.stringify({ session_id: ctx.sessionManager.getSessionId() }));
}

export default function seer(pi: ExtensionAPI): void {
  pi.on("session_start", (_event, ctx) => emit(ctx, "session_start"));
  pi.on("agent_start", (_event, ctx) => emit(ctx, "agent_start"));
  pi.on("agent_end", (_event, ctx) => emit(ctx, "agent_end"));
  pi.on("session_shutdown", (_event, ctx) => emit(ctx, "session_shutdown"));
  pi.on("tool_execution_start", (event, ctx) => {
    if (questionTools.has(event.toolName)) emit(ctx, "tool_execution_start", event.toolName);
  });
  pi.on("tool_execution_end", (event, ctx) => {
    if (questionTools.has(event.toolName)) emit(ctx, "tool_execution_end", event.toolName);
  });
}
"#
}

pub fn apply_pi_extension(path: &Path, uninstall: bool) -> Result<ApplyResult> {
    let existed = path.exists();
    let existing = if existed {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let updated = merge_pi_extension(&existing, uninstall, &path.display().to_string())?;
    if updated == existing {
        return Ok(ApplyResult {
            changed: false,
            backup: None,
        });
    }
    let backup = existed.then(|| unique_backup(path)).transpose()?;
    if uninstall {
        fs::remove_file(path)?;
        return Ok(ApplyResult {
            changed: true,
            backup,
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("seer.tmp");
    fs::write(&temporary, updated)?;
    fs::rename(temporary, path)?;
    Ok(ApplyResult {
        changed: true,
        backup,
    })
}

fn merge_pi_extension(original: &str, uninstall: bool, target: &str) -> Result<String> {
    if original.is_empty() {
        return Ok(if uninstall {
            String::new()
        } else {
            pi_extension_source().to_owned()
        });
    }
    if !original.starts_with("// Managed by tmux-seer") {
        let action = if uninstall { "remove" } else { "replace" };
        bail!("refusing to {action} non-Seer file at {target}");
    }
    Ok(if uninstall {
        String::new()
    } else {
        pi_extension_source().to_owned()
    })
}

pub fn run(uninstall: bool) -> Result<()> {
    let items = detect_all(&Tmux::new());
    let selected = run_picker(SetupModel::new(items, uninstall))?;
    if selected.is_empty() {
        println!("No integrations selected; nothing changed.");
        return Ok(());
    }
    println!("Review exact normalized changes:\n");
    for preview in preview_selected(&selected, uninstall)? {
        print!("{preview}");
        if !preview.ends_with("\n\n") {
            println!();
        }
    }
    if !confirm_apply(io::stdin().lock(), io::stdout().lock())? {
        println!("\nCancelled; nothing changed.");
        return Ok(());
    }
    println!();
    let messages = apply_selected(&selected, uninstall)?;
    for message in messages {
        println!("{message}");
    }
    if selected
        .iter()
        .any(|item| item.integration == Integration::Codex)
        && !uninstall
    {
        println!("Codex: open /hooks and trust the new Seer hook definitions.");
    }
    println!("Restart active agent sessions so their native hooks are loaded.");
    Ok(())
}

pub fn run_noninteractive(integrations: &[Integration], uninstall: bool) -> Result<()> {
    let items = integrations
        .iter()
        .copied()
        .map(|integration| SetupItem {
            host: "local".into(),
            integration,
            available: true,
            configured: false,
            selected: true,
        })
        .collect::<Vec<_>>();
    for message in apply_selected(&items, uninstall)? {
        println!("{message}");
    }
    Ok(())
}

pub fn doctor() -> Result<String> {
    let tmux = Tmux::new();
    let mut lines = vec!["Seer doctor".to_owned()];
    match tmux.output(["-V"]) {
        Ok(version) => lines.push(format!("[ok] {}", version.trim())),
        Err(error) => lines.push(format!("[error] tmux: {error}")),
    }
    for item in detect_local() {
        let availability = if item.available {
            "installed"
        } else {
            "missing"
        };
        let configuration = if item.configured {
            "configured"
        } else {
            "not configured"
        };
        let level = if item.available && item.configured {
            "ok"
        } else {
            "warn"
        };
        lines.push(format!(
            "[{level}] local {}: {availability}, {configuration}",
            item.integration.display()
        ));
    }
    match fs::read(runtime_snapshot_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<AggregateSnapshot>(&bytes).ok())
    {
        Some(snapshot) => {
            let (level, description) = snapshot_freshness(now_ms(), snapshot.generated_at_ms);
            lines.push(format!("[{level}] runtime snapshot: {description}"));
        }
        None => lines.push("[warn] runtime snapshot: missing; daemon may not be running".into()),
    }
    let hosts = tmux.show_global_option("@seer_hosts").unwrap_or_default();
    for host in hosts.split_whitespace() {
        match probe_remote(host) {
            Ok(items) => {
                let configured = items.iter().filter(|item| item.configured).count();
                let binary_installed = remote_binary_available(host);
                let binary = if binary_installed {
                    "Seer binary installed"
                } else {
                    "Seer binary missing"
                };
                let level = if binary_installed { "ok" } else { "warn" };
                lines.push(format!(
                    "[{level}] {host}: reachable, {configured} integrations configured, {binary}"
                ));
            }
            Err(error) => lines.push(format!("[error] {host}: {error}")),
        }
    }
    Ok(lines.join("\n"))
}

pub fn snapshot_freshness(now: u64, generated_at: u64) -> (&'static str, String) {
    let age_ms = now.saturating_sub(generated_at);
    if age_ms <= 6_000 {
        ("ok", format!("fresh ({}ms old)", age_ms))
    } else {
        ("warn", format!("stale ({}s old)", age_ms / 1_000))
    }
}

fn detect_all(tmux: &Tmux) -> Vec<SetupItem> {
    let mut items = detect_local();
    let hosts = tmux.show_global_option("@seer_hosts").unwrap_or_default();
    for host in hosts.split_whitespace() {
        match probe_remote(host) {
            Ok(remote) => items.extend(remote),
            Err(_) => items.extend(
                Integration::ALL
                    .into_iter()
                    .map(|integration| SetupItem::detected(host, integration, false, false)),
            ),
        }
    }
    items
}

fn detect_local() -> Vec<SetupItem> {
    let home = home_directory();
    Integration::ALL
        .into_iter()
        .map(|integration| {
            let available = command_exists(integration.slug());
            let configured = fs::read_to_string(integration_path(&home, integration))
                .ok()
                .is_some_and(|content| {
                    content.contains(&format!("tmux-seer hook {}", integration.slug()))
                        || (integration == Integration::Pi
                            && content.starts_with("// Managed by tmux-seer"))
                });
            SetupItem::detected("local", integration, available, configured)
        })
        .collect()
}

fn probe_remote(host: &str) -> Result<Vec<SetupItem>> {
    validate_host(host)?;
    let ssh = env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    let script = r#"exec "$SHELL" -lic '
for agent in claude codex pi; do
  available=0; configured=0
  command -v "$agent" >/dev/null 2>&1 && available=1
  case "$agent" in
    claude) file="$HOME/.claude/settings.json" ;;
    codex) file="$HOME/.codex/hooks.json" ;;
    pi) file="$HOME/.pi/agent/extensions/tmux-seer.ts" ;;
  esac
  test -f "$file" && grep -q "tmux-seer\|Managed by tmux-seer" "$file" && configured=1
  printf "%s|%s|%s\n" "$agent" "$available" "$configured"
done
'"#;
    let output = Command::new(ssh)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            host,
            script,
        ])
        .output()
        .with_context(|| format!("failed to run SSH for {host}"))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8(output.stdout)?;
    let mut items = Vec::new();
    for line in stdout.lines() {
        let fields: Vec<_> = line.split('|').collect();
        if fields.len() != 3 {
            continue;
        }
        let integration = Integration::from_str(fields[0]).map_err(anyhow::Error::msg)?;
        items.push(SetupItem::detected(
            host,
            integration,
            fields[1] == "1",
            fields[2] == "1",
        ));
    }
    Ok(items)
}

fn remote_binary_available(host: &str) -> bool {
    if validate_host(host).is_err() {
        return false;
    }
    let ssh = env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    Command::new(ssh)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            host,
            "test -x \"$HOME/.local/bin/tmux-seer\"",
        ])
        .status()
        .is_ok_and(|status| status.success())
}

fn preview_selected(items: &[SetupItem], uninstall: bool) -> Result<Vec<String>> {
    items
        .iter()
        .filter(|item| item.selected)
        .map(|item| {
            let target = preview_target(&item.host, item.integration);
            let original = read_integration(item)?;
            preview_integration_change(&original, item.integration, uninstall, &target)
        })
        .collect()
}

fn read_integration(item: &SetupItem) -> Result<Vec<u8>> {
    let target = preview_target(&item.host, item.integration);
    if item.host == "local" {
        let path = integration_path(&home_directory(), item.integration);
        return if path.exists() {
            fs::read(&path).with_context(|| format!("failed to read {}", path.display()))
        } else if item.integration == Integration::Pi {
            Ok(Vec::new())
        } else {
            Ok(b"{}".to_vec())
        };
    }

    validate_host(&item.host)?;
    let ssh = env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    let output = Command::new(ssh)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            &item.host,
            remote_preview_script(item.integration),
        ])
        .output()
        .with_context(|| format!("failed to read {target} over SSH"))?;
    if !output.status.success() {
        bail!(
            "failed to read {target}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

pub const fn remote_preview_script(integration: Integration) -> &'static str {
    match integration {
        Integration::Claude => {
            r#"file="$HOME/.claude/settings.json"; if test -f "$file"; then cat "$file"; else printf '{}'; fi"#
        }
        Integration::Codex => {
            r#"file="$HOME/.codex/hooks.json"; if test -f "$file"; then cat "$file"; else printf '{}'; fi"#
        }
        Integration::Pi => {
            r#"file="$HOME/.pi/agent/extensions/tmux-seer.ts"; if test -f "$file"; then cat "$file"; else :; fi"#
        }
    }
}

fn apply_selected(items: &[SetupItem], uninstall: bool) -> Result<Vec<String>> {
    let mut messages = Vec::new();
    let local: Vec<_> = items
        .iter()
        .filter(|item| item.selected && item.host == "local")
        .collect();
    let home = home_directory();
    for item in local {
        let path = integration_path(&home, item.integration);
        let result = match item.integration {
            Integration::Claude | Integration::Codex => {
                apply_json_integration(&path, item.integration, uninstall)?
            }
            Integration::Pi => apply_pi_extension(&path, uninstall)?,
        };
        messages.push(format!(
            "{} local {}: {}{}",
            if uninstall { "Removed" } else { "Configured" },
            item.integration.display(),
            path.display(),
            result
                .backup
                .map(|backup| format!(" (backup: {})", backup.display()))
                .unwrap_or_default()
        ));
    }

    let remote_hosts: std::collections::BTreeSet<_> = items
        .iter()
        .filter(|item| item.selected && item.host != "local")
        .map(|item| item.host.clone())
        .collect();
    for host in remote_hosts {
        let integrations = items
            .iter()
            .filter(|item| item.selected && item.host == host)
            .map(|item| item.integration.slug())
            .collect::<Vec<_>>()
            .join(",");
        apply_remote(&host, &integrations, uninstall)?;
        messages.push(format!(
            "{} {host}: {integrations}",
            if uninstall {
                "Removed from"
            } else {
                "Configured"
            }
        ));
    }
    Ok(messages)
}

fn apply_remote(host: &str, integrations: &str, uninstall: bool) -> Result<()> {
    validate_host(host)?;
    let ssh = env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    let command = remote_setup_script(integrations, uninstall);
    let status = Command::new(ssh)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            host,
            &command,
        ])
        .status()?;
    if !status.success() {
        bail!("remote setup failed on {host}");
    }
    Ok(())
}

pub fn remote_setup_script(integrations: &str, uninstall: bool) -> String {
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let uninstall_flag = if uninstall { " --uninstall" } else { "" };
    format!(
        r#"if [ ! -x "$HOME/.local/bin/tmux-seer" ]; then
  # The version-pinned installer fetches and verifies its payload from GitHub releases.
  curl -fsSL "https://github.com/carlosarraes/tmux-seer/releases/download/{version}/install.sh" | TMUX_SEER_VERSION={version} sh -s -- "$HOME/.local/bin/tmux-seer" || exit 127
fi
exec "$HOME/.local/bin/tmux-seer" setup --non-interactive --agents {integrations}{uninstall_flag}"#
    )
}

pub fn preview_target(host: &str, integration: Integration) -> String {
    let relative = match integration {
        Integration::Claude => "~/.claude/settings.json",
        Integration::Codex => "~/.codex/hooks.json",
        Integration::Pi => "~/.pi/agent/extensions/tmux-seer.ts",
    };
    format!("{host}:{relative}")
}

fn integration_path(home: &Path, integration: Integration) -> PathBuf {
    match integration {
        Integration::Claude => home.join(".claude/settings.json"),
        Integration::Codex => home.join(".codex/hooks.json"),
        Integration::Pi => home.join(".pi/agent/extensions/tmux-seer.ts"),
    }
}

fn home_directory() -> PathBuf {
    env::var_os("TMUX_SEER_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn command_exists(command: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|directory| {
            let candidate = directory.join(command);
            fs::metadata(candidate)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
    })
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        bail!("invalid SSH alias: {host}");
    }
    Ok(())
}

type SetupTerminal = Terminal<CrosstermBackend<Stdout>>;

fn run_picker(mut model: SetupModel) -> Result<Vec<SetupItem>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = picker_loop(&mut terminal, &mut model);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn picker_loop(terminal: &mut SetupTerminal, model: &mut SetupModel) -> Result<Vec<SetupItem>> {
    loop {
        terminal.draw(|frame| render_picker(frame, model))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => model.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => model.move_cursor(1),
            KeyCode::Char(' ') => model.toggle(),
            KeyCode::Char('a') => model.toggle_all(),
            KeyCode::Enter if model.items.iter().any(|item| item.selected) => {
                return Ok(model
                    .items
                    .iter()
                    .filter(|item| item.selected)
                    .cloned()
                    .collect());
            }
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Vec::new()),
            _ => {}
        }
    }
}

fn render_picker(frame: &mut ratatui::Frame<'_>, model: &SetupModel) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    let action = if model.uninstall {
        "Uninstall"
    } else {
        "Setup"
    };
    frame.render_widget(
        Paragraph::new(format!("Seer {action}\nChoose host × agent integrations"))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        header,
    );
    let items = model.items.iter().map(|item| {
        let checkbox = if item.selected { "[x]" } else { "[ ]" };
        let status = if !item.available {
            "unavailable"
        } else if item.configured {
            "configured"
        } else {
            "available"
        };
        let style = if item.available || item.configured {
            Style::default()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("{checkbox} {:<10} ", item.host), style),
            Span::styled(format!("{:<8} {status}", item.integration.display()), style),
        ]))
    });
    let mut state =
        ListState::default().with_selected((!model.items.is_empty()).then_some(model.cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL))
            .highlight_symbol("› ")
            .highlight_style(Style::default().bg(Color::Rgb(41, 46, 66))),
        body,
        &mut state,
    );
    frame.render_widget(
        Paragraph::new("↑↓/jk move · Space toggle · a all · Enter preview · q cancel"),
        footer,
    );
}

fn integration_events(integration: Integration) -> &'static [(&'static str, Option<&'static str>)] {
    match integration {
        Integration::Claude => &[
            ("SessionStart", None),
            ("UserPromptSubmit", None),
            ("PreToolUse", None),
            ("PostToolUse", None),
            ("PermissionRequest", None),
            ("Elicitation", None),
            ("ElicitationResult", None),
            ("Stop", None),
            ("SessionEnd", None),
        ],
        Integration::Codex => &[
            ("SessionStart", None),
            ("UserPromptSubmit", None),
            ("PreToolUse", None),
            ("PermissionRequest", None),
            ("PostToolUse", None),
            ("Stop", None),
        ],
        Integration::Pi => &[],
    }
}

fn remove_owned_hooks(hooks: &mut Map<String, Value>, slug: &str) -> Result<()> {
    let marker = format!("tmux-seer hook {slug}");
    let mut emptied_owned_events = Vec::new();
    for (event, value) in hooks.iter_mut() {
        let groups = value
            .as_array_mut()
            .with_context(|| format!("hooks.{event} must be an array"))?;
        let mut removed_owned = false;
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                handlers.retain(|handler| {
                    let owned = handler
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| command.contains(&marker));
                    removed_owned |= owned;
                    !owned
                });
            }
        }
        if removed_owned {
            groups.retain(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_none_or(|handlers| !handlers.is_empty())
            });
            if groups.is_empty() {
                emptied_owned_events.push(event.clone());
            }
        }
    }
    for event in emptied_owned_events {
        hooks.remove(&event);
    }
    Ok(())
}

fn unique_backup(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("configuration path has no filename")?;
    for suffix in 0..100 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let candidate =
            path.with_file_name(format!("{file_name}.seer-backup-{}{}", now_ms(), suffix));
        if !candidate.exists() {
            fs::copy(path, &candidate)?;
            return Ok(candidate);
        }
    }
    bail!("could not allocate backup name for {}", path.display())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn remote_probe_finds_agent_added_by_login_and_interactive_shell_startup() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("home");
        let fnm_bin = home.join("fnm/bin");
        fs::create_dir_all(&fnm_bin).unwrap();
        fs::write(
            home.join(".login-env"),
            "export FNM_BIN=\"$HOME/fnm/bin\"\n",
        )
        .unwrap();
        fs::write(
            home.join(".interactive-env"),
            "export PATH=\"$FNM_BIN:$PATH\"\n",
        )
        .unwrap();

        let pi = fnm_bin.join("pi");
        fs::write(&pi, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();

        let login_shell = directory.path().join("login-shell");
        fs::write(
            &login_shell,
            r#"#!/bin/sh
test "$1" = "-lic" || exit 64
. "$HOME/.login-env"
. "$HOME/.interactive-env"
exec /bin/sh -c "$2"
"#,
        )
        .unwrap();
        fs::set_permissions(&login_shell, fs::Permissions::from_mode(0o755)).unwrap();

        let ssh = directory.path().join("ssh");
        fs::write(
            &ssh,
            r#"#!/bin/sh
for argument do remote_command="$argument"; done
HOME="$TMUX_SEER_TEST_HOME" SHELL="$TMUX_SEER_TEST_SHELL" PATH=/tmux-seer-no-agents \
  /bin/sh -c "$remote_command"
"#,
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();

        let items = temp_env::with_vars(
            [
                ("TMUX_SEER_SSH", Some(ssh.as_os_str())),
                ("TMUX_SEER_TEST_HOME", Some(home.as_os_str())),
                ("TMUX_SEER_TEST_SHELL", Some(login_shell.as_os_str())),
            ],
            || probe_remote("remote").unwrap(),
        );

        let pi = items
            .iter()
            .find(|item| item.integration == Integration::Pi)
            .unwrap();
        assert!(pi.available);
    }
}
