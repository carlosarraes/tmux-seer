use std::{
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

pub fn root_directory() -> PathBuf {
    root_directory_for_socket(&current_socket())
}

fn root_directory_for_socket(socket: &str) -> PathBuf {
    if let Some(path) = env::var_os("TMUX_SEER_RUNTIME_DIR") {
        return PathBuf::from(path);
    }
    socket_runtime_directory(socket)
}

pub fn socket_runtime_directory(socket: &str) -> PathBuf {
    Path::new(socket)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join("tmux-seer"))
        .unwrap_or_else(|| env::temp_dir().join("tmux-seer"))
}

pub fn state_directory() -> PathBuf {
    if let Some(path) = env::var_os("TMUX_SEER_STATE_DIR") {
        return PathBuf::from(path);
    }
    dirs::state_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(env::temp_dir)
                .join(".local/state")
        })
        .join("tmux-seer")
}

pub fn current_socket() -> String {
    env::var("TMUX")
        .ok()
        .and_then(|value| value.split(',').next().map(str::to_owned))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".to_owned())
}

pub fn server_id(socket: &str) -> String {
    let mut hasher = StableHasher::default();
    socket.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn current_server_id() -> String {
    server_id(&current_socket())
}

pub fn server_directory(server_id: &str) -> PathBuf {
    root_directory().join("servers").join(server_id)
}

pub fn server_directory_for_socket(socket: &str) -> PathBuf {
    root_directory_for_socket(socket)
        .join("servers")
        .join(server_id(socket))
}

pub fn current_server_directory() -> PathBuf {
    server_directory(&current_server_id())
}

pub fn request_refresh() -> Result<()> {
    let token = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    atomic_write(
        &current_server_directory().join("refresh"),
        token.as_bytes(),
    )
}

pub fn refresh_token() -> u128 {
    fs::read_to_string(current_server_directory().join("refresh"))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

pub fn ensure_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create runtime directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect runtime directory {}", path.display()))?;
    }
    Ok(())
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("runtime path has no parent: {}", path.display()))?;
    ensure_private_directory(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, contents)
        .with_context(|| format!("failed to write runtime file {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect runtime file {}", temporary.display()))?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to publish runtime file {}", path.display()))?;
    Ok(())
}

struct StableHasher(u64);

impl Default for StableHasher {
    fn default() -> Self {
        Self(0xcbf29ce484222325)
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}
