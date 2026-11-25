#!/usr/bin/env bash
# Keep every release channel on the same version.
#
#   scripts/release.sh bump 1.3.0   before tagging: Cargo.toml + Cargo.lock,
#                                   the README's install commands, the COPR
#                                   spec and the AUR package
#   scripts/release.sh aur          after the GitHub release is published:
#                                   put the real checksums into the AUR package
#   scripts/release.sh copr         after the tag is pushed: build that version
#                                   on Fedora COPR (needs ~/.config/copr)
#   scripts/release.sh check        every channel matches Cargo.toml
#
# The .rpm, .deb, AppImage, Arch .pkg.tar.zst and tarball are all built by
# .github/workflows/release.yml from the same tag, so they can't drift.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

REPO=${REPO:-chrollo-03/pova}
current() { sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1; }
esc() { printf '%s' "$1" | sed 's/\./\\./g'; }
aur=packaging/aur/pova-bin
spec=packaging/copr/pova.spec

check() {
  local v ok=1
  v=$(current)
  echo "Cargo.toml: $v"
  grep -q "^pkgver=$v$" "$aur/PKGBUILD" || { echo "  AUR PKGBUILD pkgver differs"; ok=0; }
  grep -q "pkgver = $v$" "$aur/.SRCINFO" || { echo "  AUR .SRCINFO pkgver differs"; ok=0; }
  grep -q "^Version: *$v$" "$spec" || { echo "  COPR spec Version differs"; ok=0; }
  grep -q "releases/download/v$v/" README.md || { echo "  README install commands differ"; ok=0; }
  grep -q "^name = \"pova\"" Cargo.lock && grep -A1 '^name = "pova"$' Cargo.lock | grep -q "version = \"$v\"" \
    || { echo "  Cargo.lock differs (run cargo build)"; ok=0; }
  if [ "$ok" = 1 ]; then echo "all channels on $v"; else return 1; fi
}

bump() {
  local new=$1 old
  old=$(current)
  [[ $new =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3" >&2; exit 1; }
  [ "$new" != "$old" ] || { echo "already at $new" >&2; exit 1; }
  local o; o=$(esc "$old")
  sed -i "0,/^version = \"$o\"/s//version = \"$new\"/" Cargo.toml
  cargo metadata --format-version 1 > /dev/null   # refreshes Cargo.lock
  sed -i "s/$o/$new/g" README.md
  sed -i "s/^Version: .*/Version:        $new/" "$spec"
  local stamp; stamp=$(LC_ALL=C date '+%a %b %d %Y')
  sed -i "/^%changelog/a * $stamp chrollo-03 <chrollo-03@users.noreply.github.com> - $new-1\n- pova $new\n" "$spec"
  sed -i "s/^pkgver=.*/pkgver=$new/; s/^pkgrel=.*/pkgrel=1/" "$aur/PKGBUILD"
  sed -i "s/$o/$new/g; s/pkgrel = .*/pkgrel = 1/" "$aur/.SRCINFO"
  echo "Bumped $old -> $new."
  check
}

aur_sums() {
  local v tmp
  v=$(current)
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' RETURN
  local urls=(
    "https://github.com/${REPO}/releases/download/v$v/pova-v$v-x86_64-linux.tar.gz"
    "https://raw.githubusercontent.com/${REPO}/v$v/packaging/appimage/pova.desktop"
    "https://raw.githubusercontent.com/${REPO}/v$v/packaging/appimage/pova.png"
  )
  local sums=()
  for u in "${urls[@]}"; do
    curl -fsSL --retry 3 -o "$tmp/f" "$u" || { echo "can't download $u (is v$v released?)" >&2; return 1; }
    sums+=("$(sha256sum "$tmp/f" | cut -d' ' -f1)")
  done
  python3 - "$aur" "${sums[@]}" <<'PY'
import re, sys
d, sums = sys.argv[1], sys.argv[2:]
p = f"{d}/PKGBUILD"; s = open(p).read()
block = "sha256sums=(" + "\n            ".join(f"'{x}'" for x in sums) + ")"
s = re.sub(r"sha256sums=\([^)]*\)", block, s, count=1)
open(p, "w").write(s)
p = f"{d}/.SRCINFO"; lines = open(p).read().split("\n")
it = iter(sums)
lines = [f"\tsha256sums = {next(it)}" if l.startswith("\tsha256sums = ") else l for l in lines]
open(p, "w").write("\n".join(lines))
PY
  echo "AUR checksums updated for $v:"; printf '  %s\n' "${sums[@]}"
}

copr_build() {
  local v; v=$(current)
  git rev-parse -q --verify "refs/tags/v$v" > /dev/null || { echo "tag v$v doesn't exist yet" >&2; return 1; }
  local copr=(uv tool run --from copr-cli copr-cli)
  command -v copr-cli > /dev/null && copr=(copr-cli)
  "${copr[@]}" edit-package-scm pova --name pova --clone-url "https://github.com/${REPO}" \
    --commit "v$v" --method make_srpm --webhook-rebuild off
  "${copr[@]}" build-package pova --name pova
}

case "${1:-}" in
  bump) bump "${2:?usage: scripts/release.sh bump X.Y.Z}" ;;
  aur) aur_sums ;;
  copr) copr_build ;;
  check) check ;;
  *) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
