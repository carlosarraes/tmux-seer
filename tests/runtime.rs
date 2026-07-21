use std::path::PathBuf;

use tmux_seer::runtime::{server_id, socket_runtime_directory};

#[test]
fn runtime_ids_are_stable_and_distinguish_inputs() {
    assert_eq!(
        server_id("/tmp/tmux/default"),
        server_id("/tmp/tmux/default")
    );
    assert_ne!(server_id("/tmp/tmux/default"), server_id("/tmp/tmux/work"));
    assert_ne!(server_id("/dev/pts/1"), server_id("/dev/pts/2"));
}

#[test]
fn runtime_directory_follows_the_tmux_socket_parent() {
    assert_eq!(
        socket_runtime_directory("/private/tmp/tmux-501/default"),
        PathBuf::from("/private/tmp/tmux-501/tmux-seer")
    );
}
