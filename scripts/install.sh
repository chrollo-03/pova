#!/usr/bin/env bash
# One-line installer for pova (https://github.com/chrollo-03/pova).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/chrollo-03/pova/main/scripts/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/chrollo-03/pova/main/scripts/install.sh | bash -s -- --method binary
#
# Options:
#   --version TAG      Install a specific tag (default: latest release, e.g. v1.3.0)
#   --method METHOD    auto | rpm | deb | pacman | appimage | binary (default: auto)
#   --prefix DIR       Prefix for binary/AppImage installs (default: ~/.local)
#   --repo OWNER/REPO  GitHub repo to download from (default: chrollo-03/pova)
set -euo pipefail

REPO="${POVA_REPO:-chrollo-03/pova}"
VERSION=""
METHOD="auto"
PREFIX="${HOME}/.local"

usage() {
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION=${2:?missing value for --version}; shift 2 ;;
    --method)  METHOD=${2:?missing value for --method}; shift 2 ;;
    --prefix)  PREFIX=${2:?missing value for --prefix}; shift 2 ;;
    --repo)    REPO=${2:?missing value for --repo}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

[ "$(uname -s)" = "Linux" ] || die "pova currently supports Linux (with PipeWire)."
need curl
need sha256sum

case "$(uname -m)" in
  x86_64|amd64)   ARCH="x86_64";  DEB_ARCH="amd64" ;;
  aarch64|arm64)  ARCH="aarch64"; DEB_ARCH="arm64" ;;
  *) die "unsupported CPU architecture: $(uname -m) (supported: x86_64, aarch64)" ;;
esac

if [ -z "$VERSION" ]; then
  VERSION=$(curl -fsSL -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
  [ -n "$VERSION" ] || die "couldn't determine latest release tag from github.com/${REPO}"
fi

[[ "$VERSION" == v* ]] || VERSION="v${VERSION}"
VER_NUM="${VERSION#v}"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

# Pick the cleanest package format for this machine when --method auto.
if [ "$METHOD" = "auto" ]; then
  if [ "$(id -u)" -eq 0 ] || command -v sudo >/dev/null 2>&1; then
    if command -v dnf >/dev/null 2>&1; then
      METHOD="rpm"
    elif command -v apt-get >/dev/null 2>&1; then
      METHOD="deb"
    elif command -v pacman >/dev/null 2>&1 && [ "$ARCH" = "x86_64" ]; then
      METHOD="pacman"
    else
      METHOD="binary"
    fi
  else
    METHOD="binary"
  fi
fi

run_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo "$@"
  fi
}

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

fetch_verified() {
  local asset=$1
  printf 'Downloading %s (%s)...\n' "$asset" "$VERSION"
  curl -fL --progress-bar -o "$TMPDIR/$asset" "${BASE_URL}/${asset}" \
    || die "failed to download ${BASE_URL}/${asset}"
  curl -fsSL -o "$TMPDIR/${asset}.sha256" "${BASE_URL}/${asset}.sha256" \
    || die "failed to download ${BASE_URL}/${asset}.sha256"
  (cd "$TMPDIR" && sha256sum -c "${asset}.sha256")
}

case "$METHOD" in
  rpm)
    asset="pova-${VER_NUM}-1.${ARCH}.rpm"
    fetch_verified "$asset"
    run_root dnf install -y "$TMPDIR/$asset"
    ;;
  deb)
    asset="pova_${VER_NUM}-1_${DEB_ARCH}.deb"
    fetch_verified "$asset"
    run_root apt-get update -qq
    run_root apt-get install -y "$TMPDIR/$asset"
    ;;
  pacman)
    [ "$ARCH" = "x86_64" ] || die "Arch .pkg.tar.zst is built for x86_64; use --method binary on $ARCH"
    asset="pova-bin-${VER_NUM}-1-x86_64.pkg.tar.zst"
    fetch_verified "$asset"
    run_root pacman -U --noconfirm "$TMPDIR/$asset"
    ;;
  appimage)
    asset="pova-${VERSION}-${ARCH}.AppImage"
    fetch_verified "$asset"
    install -Dm755 "$TMPDIR/$asset" "${PREFIX}/bin/pova"
    printf 'Installed AppImage to %s/bin/pova\n' "$PREFIX"
    ;;
  binary)
    need tar
    asset="pova-${VERSION}-${ARCH}-linux.tar.gz"
    fetch_verified "$asset"
    tar -xzf "$TMPDIR/$asset" -C "$TMPDIR"
    dir="$TMPDIR/pova-${VERSION}-${ARCH}-linux"
    install -Dm755 "$dir/pova" "${PREFIX}/bin/pova"
    install -Dm755 "$dir/scripts/import-cookie.sh" "${PREFIX}/bin/pova-import-cookie"
    printf 'Installed pova and pova-import-cookie to %s/bin\n' "$PREFIX"
    ;;
  *)
    die "unknown --method: $METHOD (use auto, rpm, deb, pacman, appimage, or binary)"
    ;;
esac

if [ "$METHOD" = "binary" ] || [ "$METHOD" = "appimage" ]; then
  case ":${PATH}:" in
    *":${PREFIX}/bin:"*) ;;
    *)
      printf '\nNote: %s/bin is not in your PATH. Add this to your shell profile:\n' "$PREFIX"
      printf '  export PATH="%s/bin:$PATH"\n' "$PREFIX"
      ;;
  esac
  printf '\nRuntime helpers needed: mpv, yt-dlp, pipewire tools (pw-record/pw-dump), and nodejs/deno.\n'
fi

if command -v pova >/dev/null 2>&1; then
  printf '\nInstalled: %s\n' "$(pova --version)"
fi
