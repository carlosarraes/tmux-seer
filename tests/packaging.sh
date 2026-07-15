#!/usr/bin/env sh

set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

fail() {
  echo "packaging test failed: $*" >&2
  exit 1
}

test -f "$root/justfile" || fail "justfile is missing"
grep -Eq '^build:' "$root/justfile" || fail "justfile has no build recipe"
grep -Eq '^check:' "$root/justfile" || fail "justfile has no check recipe"
grep -Eq '^release version:' "$root/justfile" || fail "justfile has no release recipe"

test -x "$root/install.sh" || fail "root install.sh is not executable"
grep -Fq 'install.sh' "$root/.github/workflows/release.yml" ||
  fail "release workflow does not publish install.sh"

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

mkdir -p "$temporary/archive" "$temporary/assets" "$temporary/bin"
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

echo "packaging checks passed"
