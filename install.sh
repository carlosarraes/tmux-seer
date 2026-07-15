#!/usr/bin/env sh

set -eu

repository="${TMUX_SEER_REPOSITORY:-carlosarraes/tmux-seer}"
version="${TMUX_SEER_VERSION:-latest}"
destination="${1:-${TMUX_SEER_INSTALL_PATH:-${HOME}/.local/bin/tmux-seer}}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  *)
    echo "Seer does not publish a binary for $(uname -s) $(uname -m) yet." >&2
    exit 1
    ;;
esac

asset="tmux-seer-${target}.tar.gz"
if [ "$version" = "latest" ]; then
  base="https://github.com/${repository}/releases/latest/download"
else
  base="https://github.com/${repository}/releases/download/${version}"
fi

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

echo "Downloading Seer ${version} for ${target}..."
curl -fsSL "${base}/${asset}" -o "${temporary}/${asset}"
curl -fsSL "${base}/${asset}.sha256" -o "${temporary}/${asset}.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$temporary" && sha256sum -c "${asset}.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$temporary" && shasum -a 256 -c "${asset}.sha256")
else
  echo "Seer requires sha256sum or shasum to verify its download." >&2
  exit 1
fi

tar -xzf "${temporary}/${asset}" -C "$temporary" tmux-seer
mkdir -p "$(dirname "$destination")"
install -m 0755 "${temporary}/tmux-seer" "$destination"

echo "Installed Seer at $destination"
case ":${PATH}:" in
  *":$(dirname "$destination"):"*) ;;
  *)
    echo "Note: add $(dirname "$destination") to the PATH inherited by tmux."
    ;;
esac

cat <<EOF

Next:
  1. Add Seer through TPM, or run: $destination bootstrap
  2. Choose agent integrations: $destination setup
  3. Check the installation: $destination doctor
EOF
