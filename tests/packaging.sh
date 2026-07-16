#!/usr/bin/env sh

set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
expected_version="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' "$root/Cargo.toml")"

fail() {
  echo "packaging test failed: $*" >&2
  exit 1
}

test -f "$root/justfile" || fail "justfile is missing"
grep -Eq '^build:' "$root/justfile" || fail "justfile has no build recipe"
grep -Eq '^check:' "$root/justfile" || fail "justfile has no check recipe"
grep -Eq '^release version:' "$root/justfile" || fail "justfile has no release recipe"
version_update_line="$(grep -n 'awk -v version=' "$root/justfile" | cut -d: -f1)"
release_check_line="$(grep -n '^[[:space:]]*just check$' "$root/justfile" | cut -d: -f1)"
test "$version_update_line" -lt "$release_check_line" ||
  fail "release checks must run after updating the package version"

test -x "$root/install.sh" || fail "root install.sh is not executable"
grep -Fq 'install.sh' "$root/.github/workflows/release.yml" ||
  fail "release workflow does not publish install.sh"

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

mkdir -p "$temporary/archive" "$temporary/assets" "$temporary/bin"
cat >"$temporary/bin/uname" <<'UNAME'
#!/usr/bin/env sh
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) exit 1 ;;
esac
UNAME
chmod +x "$temporary/bin/uname"

cat >"$temporary/archive/tmux-seer" <<'BINARY'
#!/usr/bin/env sh
echo "fixture seer"
BINARY
chmod +x "$temporary/archive/tmux-seer"

asset="tmux-seer-x86_64-unknown-linux-gnu.tar.gz"
tar -C "$temporary/archive" -czf "$temporary/assets/$asset" tmux-seer
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$temporary/assets" && sha256sum "$asset" >"$asset.sha256")
else
  (cd "$temporary/assets" && shasum -a 256 "$asset" >"$asset.sha256")
fi

cat >"$temporary/bin/curl" <<'CURL'
#!/usr/bin/env sh
set -eu

destination=""
url=""
while test "$#" -gt 0; do
  case "$1" in
    -o)
      destination="$2"
      shift 2
      ;;
    -* ) shift ;;
    *)
      url="$1"
      shift
      ;;
  esac
done

test -n "$destination"
cp "$FIXTURE_ASSETS/${url##*/}" "$destination"
CURL
chmod +x "$temporary/bin/curl"

destination="$temporary/install/tmux-seer"
PATH="$temporary/bin:$PATH" \
  FIXTURE_ASSETS="$temporary/assets" \
  TMUX_SEER_VERSION="v0.1.0" \
  TMUX_SEER_INSTALL_PATH="$destination" \
  "$root/install.sh" >/dev/null

test -x "$destination" || fail "installer did not install an executable"
test "$("$destination")" = "fixture seer" || fail "installed the wrong binary"

mkdir -p "$temporary/plugin/bin"
cp "$root/tmux-seer.tmux" "$root/Cargo.toml" "$temporary/plugin/"
cat >"$temporary/bin/tmux" <<'TMUX'
#!/usr/bin/env sh
set -eu
printf '%s\n' "$*" >> "$TMUX_SEER_TMUX_LOG"
case "$1" in
  show-option) exit 0 ;;
esac
TMUX
chmod +x "$temporary/bin/tmux"

cat >"$temporary/bin/tmux-seer" <<'STALE_PATH_BINARY'
#!/usr/bin/env sh
printf '%s\n' path-binary >> "$TMUX_SEER_BINARY_LOG"
echo 'tmux-seer 0.0.6'
STALE_PATH_BINARY
chmod +x "$temporary/bin/tmux-seer"

cat >"$temporary/plugin/bin/tmux-seer" <<'STALE_PLUGIN_BINARY'
#!/usr/bin/env sh
if test "${1:-}" = '--version'; then echo 'tmux-seer 0.0.6'; exit 0; fi
printf '%s\n' stale-plugin-bootstrap >> "$TMUX_SEER_BINARY_LOG"
STALE_PLUGIN_BINARY
chmod +x "$temporary/plugin/bin/tmux-seer"

: >"$temporary/tmux.log"
: >"$temporary/binary.log"
PATH="$temporary/bin:$PATH" \
  TMUX_SEER_TMUX_LOG="$temporary/tmux.log" \
  TMUX_SEER_BINARY_LOG="$temporary/binary.log" \
  bash "$temporary/plugin/tmux-seer.tmux"

grep -Fq "TMUX_SEER_VERSION='v$expected_version'" "$temporary/tmux.log" ||
  fail "stale plugin binary did not schedule its matching release"
test ! -s "$temporary/binary.log" ||
  fail "a stale plugin or PATH binary was bootstrapped"

cat >"$temporary/plugin/bin/tmux-seer" <<'CURRENT_PLUGIN_BINARY'
#!/usr/bin/env sh
if test "${1:-}" = '--version'; then echo 'tmux-seer CURRENT_VERSION'; exit 0; fi
printf '%s\n' "$*" >> "$TMUX_SEER_BINARY_LOG"
CURRENT_PLUGIN_BINARY
sed -i "s/CURRENT_VERSION/$expected_version/" "$temporary/plugin/bin/tmux-seer"
chmod +x "$temporary/plugin/bin/tmux-seer"
: >"$temporary/tmux.log"
: >"$temporary/binary.log"
PATH="$temporary/bin:$PATH" \
  TMUX_SEER_TMUX_LOG="$temporary/tmux.log" \
  TMUX_SEER_BINARY_LOG="$temporary/binary.log" \
  bash "$temporary/plugin/tmux-seer.tmux"

test "$(cat "$temporary/binary.log")" = 'bootstrap' ||
  fail "the current plugin-owned binary was not bootstrapped"

echo "packaging checks passed"
