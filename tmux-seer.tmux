#!/usr/bin/env bash

set -euo pipefail

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
configured_binary="$(tmux show-option -gqv '@seer_binary')"

if [[ -n "$configured_binary" && -x "$configured_binary" ]]; then
  binary="$configured_binary"
elif [[ -x "$CURRENT_DIR/bin/tmux-seer" ]]; then
  binary="$CURRENT_DIR/bin/tmux-seer"
elif command -v tmux-seer >/dev/null 2>&1; then
  binary="$(command -v tmux-seer)"
elif [[ -x "$CURRENT_DIR/target/release/tmux-seer" ]]; then
  binary="$CURRENT_DIR/target/release/tmux-seer"
else
  binary="$CURRENT_DIR/bin/tmux-seer"
  tmux display-message "Seer: installing the release binary…"
  tmux run-shell -b "'$CURRENT_DIR/install.sh' '$binary' && '$binary' bootstrap"
  exit 0
fi

"$binary" bootstrap
