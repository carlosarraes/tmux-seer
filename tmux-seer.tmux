#!/usr/bin/env bash

set -euo pipefail

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
configured_binary="$(tmux show-option -gqv '@seer_binary')"
expected_version="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' "$CURRENT_DIR/Cargo.toml")"
plugin_binary="$CURRENT_DIR/bin/tmux-seer"

if [[ -n "$configured_binary" && -x "$configured_binary" ]]; then
  binary="$configured_binary"
else
  binary="$plugin_binary"
fi

if [[ "$binary" == "$plugin_binary" ]] &&
  [[ ! -x "$binary" || "$("$binary" --version 2>/dev/null || true)" != "tmux-seer $expected_version" ]]; then
  tmux display-message "Seer: installing the release binary…"
  tmux run-shell -b "TMUX_SEER_VERSION='v$expected_version' '$CURRENT_DIR/install.sh' '$binary' && '$binary' bootstrap"
  exit 0
fi

"$binary" bootstrap
