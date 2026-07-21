use std::io::{self, Read};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tmux_seer::{
    adapters::{normalize, NativeEvent},
    hook_state::HookStateStore,
    model::AgentKind,
    navigation::Navigator,
    snapshot::AgentKey,
    tmux::{now_ms, Tmux},
};

#[derive(Debug, Parser)]
#[command(name = "tmux-seer", version, about = "See every agent in tmux")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install the tmux binding, status widget, and daemon.
    Bootstrap,
    /// Run the per-tmux-server collector.
    Daemon,
    /// Open the Seer dashboard.
    Popup {
        #[arg(long)]
        client: Option<String>,
    },
    /// Print a versioned snapshot of the current tmux server.
    Snapshot {
        #[arg(long, default_value = "local")]
        host: String,
        #[arg(long, default_value_t = true)]
        json: bool,
    },
    /// Jump the invoking client to an agent pane.
    Jump {
        host: String,
        session_id: String,
        window_id: String,
        pane_id: String,
        #[arg(long)]
        client: Option<String>,
    },
    /// Receive one native agent hook event.
    Hook {
        agent: AgentKind,
        event: String,
        #[arg(long)]
        matcher: Option<String>,
    },
    /// Select and install native agent integrations.
    Setup {
        #[arg(long)]
        uninstall: bool,
        #[arg(long, hide = true)]
        non_interactive: bool,
        #[arg(long, value_delimiter = ',', hide = true)]
        agents: Vec<tmux_seer::setup::Integration>,
    },
    /// Check tmux, SSH hosts, binaries, and hook configuration.
    Doctor,
    /// Print bounded daemon logs.
    Logs {
        /// Continue printing new log entries.
        #[arg(long)]
        follow: bool,
    },
    #[command(hide = true)]
    Wake {
        #[arg(default_value = "manual")]
        reason: String,
    },
    #[command(hide = true)]
    Stream {
        #[arg(long)]
        host: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Hook {
            agent,
            event,
            matcher,
        } => {
            if std::env::var_os("TMUX_PANE").is_none() {
                return Ok(());
            }
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            let payload: serde_json::Value = if input.trim().is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_str(&input)?
            };
            let native = NativeEvent {
                agent,
                event,
                matcher,
                session_id: string_field(&payload, &["session_id", "sessionId"]),
                turn_id: string_field(&payload, &["turn_id", "turnId"]),
                reason: string_field(&payload, &["reason", "message"]),
            };
            let pane = std::env::var("TMUX_PANE")?;
            HookStateStore::from_env().apply(&pane, normalize(native), now_ms())
        }
        Command::Snapshot { host, json: _ } => {
            let snapshot = Tmux::new().snapshot(&host)?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        Command::Daemon => tmux_seer::daemon::run().await,
        Command::Popup { client } => {
            if let Some(target) = tmux_seer::ui::run(client.clone())? {
                Navigator::new(Tmux::new()).navigate(&target, client.as_deref())?;
            }
            Ok(())
        }
        Command::Jump {
            host,
            session_id,
            window_id,
            pane_id,
            client,
        } => Navigator::new(Tmux::new()).jump(
            &AgentKey {
                host,
                session_id,
                window_id,
                pane_id,
            },
            client.as_deref(),
        ),
        Command::Setup {
            uninstall,
            non_interactive,
            agents,
        } => {
            if non_interactive {
                let agents = if agents.is_empty() {
                    tmux_seer::setup::Integration::ALL.to_vec()
                } else {
                    agents
                };
                tmux_seer::setup::run_noninteractive(&agents, uninstall)
            } else {
                tmux_seer::setup::run(uninstall)
            }
        }
        Command::Doctor => {
            println!("{}", tmux_seer::setup::doctor()?);
            Ok(())
        }
        Command::Logs { follow } => tmux_seer::diagnostics::print_logs(follow),
        Command::Wake { reason: _ } => tmux_seer::runtime::request_refresh(),
        Command::Stream { host } => tmux_seer::collector::run_stream(host).await,
        Command::Bootstrap => {
            let binary = std::env::current_exe()?;
            tmux_seer::bootstrap::restart_existing_daemons()?;
            tmux_seer::bootstrap::bootstrap(Tmux::new(), &binary.to_string_lossy())
        }
    }
}

fn string_field(value: &serde_json::Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(name).and_then(|field| field.as_str()))
        .map(str::to_owned)
}
