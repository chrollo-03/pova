#!/usr/bin/env bash
# Build pova's signed multi-arch apt repository (served on GitHub Pages) from
# the .deb files of published releases. Every release's .deb is the one CI
# already built and tested, so apt users get the same binary as every other
# channel.
#
#   scripts/apt-repo.sh fetch <deb-dir> [count]   download the .deb of the last
#                                                 <count> releases (default 3),
#                                                 checking each .sha256
#   scripts/apt-repo.sh build <deb-dir> <site>    write the repository into <site>
#   scripts/apt-repo.sh test  <site> <image>      add the repository on a clean
#                                                 container and apt install pova
#
# `build` signs with the armored private key in $APT_SIGNING_KEY (optional
# passphrase in $APT_SIGNING_PASSPHRASE). Without a key it makes a throwaway
# one, for tests: never publish that. `fetch` needs gh, `test` docker (or
# DOCKER=podman). Layout: dists/stable/main/binary-<arch>, pool/main/p/pova/<arch>,
# and the public key as pova.gpg / pova.asc.
set -euo pipefail

SUITE=stable
COMPONENT=main
REPO=${GITHUB_REPOSITORY:-chrollo-03/pova}
OWNER=${REPO%%/*}
URL=${APT_URL:-https://${OWNER}.github.io/pova/apt}

die() { echo "apt-repo: $*" >&2; exit 1; }

fetch() {
  local dir=${1:?deb dir} count=${2:-3}
  mkdir -p "$dir"
  local tags
  tags=$(gh release list --repo "$REPO" --exclude-drafts --exclude-pre-releases --limit "$count" --json tagName -q '.[].tagName')
  [ -n "$tags" ] || die "no published releases found in $REPO"
  for tag in $tags; do
    # A stalled download must not hang a CI job: time out and retry.
    local ok=
    for attempt in 1 2 3; do
      if timeout 300 gh release download "$tag" --repo "$REPO" -D "$dir" -p '*.deb' -p '*.deb.sha256' --clobber; then
        ok=1
        break
      fi
      echo "apt-repo: downloading $tag failed (attempt $attempt), retrying" >&2
    done
    [ -n "$ok" ] || die "couldn't download the .deb of $tag"
  done
  (cd "$dir" && for s in *.deb.sha256; do sha256sum -c "$s"; done)
  rm -f "$dir"/*.sha256
  ls -l "$dir"
}

# Import the signing key into a private keyring; prints the key's fingerprint.
signing_key() {
  if [ -z "${APT_SIGNING_KEY:-}" ]; then
    echo "apt-repo: no APT_SIGNING_KEY, using a throwaway test key" >&2
    gpg --batch --pinentry-mode loopback --passphrase '' \
        --quick-gen-key "pova apt test key <test@invalid>" ed25519 sign 1d 2>/dev/null
  else
    printf '%s\n' "$APT_SIGNING_KEY" | gpg --batch --import 2>/dev/null
  fi
  gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr:/ {print $10; exit}'
}

build() {
  local debs=${1:?deb dir} site=${2:?output dir}
  ls "$debs"/*.deb >/dev/null 2>&1 || die "no .deb files in $debs"
  command -v apt-ftparchive >/dev/null || die "needs apt-ftparchive (apt-utils)"
  rm -rf "$site"

  local arches=()
  local f base arch
  for f in "$debs"/*.deb; do
    base=$(basename "$f" .deb)
    arch=${base##*_}
    [ -n "$arch" ] || die "couldn't detect arch from $f"
    local pool="$site/pool/$COMPONENT/p/pova/$arch"
    local bin="$site/dists/$SUITE/$COMPONENT/binary-$arch"
    mkdir -p "$pool" "$bin"
    cp "$f" "$pool/"
    if [[ ! " ${arches[*]} " == *" $arch "* ]]; then
      arches+=("$arch")
    fi
  done

  IFS=$'\n' arches=($(printf '%s\n' "${arches[@]}" | sort))
  unset IFS

  for arch in "${arches[@]}"; do
    local bin="$site/dists/$SUITE/$COMPONENT/binary-$arch"
    (cd "$site" && apt-ftparchive packages "pool/$COMPONENT/p/pova/$arch" > "dists/$SUITE/$COMPONENT/binary-$arch/Packages")
    gzip -9nk "$bin/Packages"
  done

  local arch_list="${arches[*]}"
  apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=pova \
    -o APT::FTPArchive::Release::Label=pova \
    -o APT::FTPArchive::Release::Suite=$SUITE \
    -o APT::FTPArchive::Release::Codename=$SUITE \
    -o "APT::FTPArchive::Release::Architectures=$arch_list" \
    -o APT::FTPArchive::Release::Components=$COMPONENT \
    -o APT::FTPArchive::Release::Description="pova, YouTube Music in the terminal" \
    release "$site/dists/$SUITE" > "$site/dists/$SUITE/Release.tmp"
  mv "$site/dists/$SUITE/Release.tmp" "$site/dists/$SUITE/Release"

  # A private keyring, so the runner's (or your) own keys are never touched.
  GNUPGHOME=$(mktemp -d)
  export GNUPGHOME
  trap 'rm -rf "$GNUPGHOME"' RETURN
  local fpr
  fpr=$(signing_key)
  [ -n "$fpr" ] || die "couldn't load the signing key"
  local sign=(gpg --batch --yes --pinentry-mode loopback --local-user "$fpr")
  [ -n "${APT_SIGNING_PASSPHRASE:-}" ] && sign+=(--passphrase "$APT_SIGNING_PASSPHRASE")
  "${sign[@]}" --clearsign -o "$site/dists/$SUITE/InRelease" "$site/dists/$SUITE/Release"
  "${sign[@]}" --armor --detach-sign -o "$site/dists/$SUITE/Release.gpg" "$site/dists/$SUITE/Release"
  gpg --batch --export "$fpr" > "$site/pova.gpg"
  gpg --batch --armor --export "$fpr" > "$site/pova.asc"

  local latest
  latest=$(find "$site/pool" -name '*.deb' -printf '%f\n' | sort -V | tail -1)
  cat > "$site/index.html" <<EOF
<!doctype html>
<meta charset="utf-8">
<title>pova apt repository</title>
<h1>pova apt repository</h1>
<p>For Debian, Ubuntu, Linux Mint, Pop!_OS and other apt-based distros (${arch_list}):</p>
<pre>sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL $URL/pova.gpg | sudo tee /etc/apt/keyrings/pova.gpg &gt; /dev/null
echo "deb [signed-by=/etc/apt/keyrings/pova.gpg] $URL $SUITE $COMPONENT" | sudo tee /etc/apt/sources.list.d/pova.list
sudo apt update &amp;&amp; sudo apt install pova</pre>
<p>Latest: $latest. Signing key fingerprint: <code>$fpr</code></p>
<p><a href="https://github.com/$REPO">github.com/$REPO</a></p>
EOF
  echo "built $site ($arch_list): $(find "$site/pool" -name '*.deb' -printf '%f ')(key $fpr)"
}

# Serve <site> over HTTP and install pova from it on <image>, the way a user
# would (signed-by keyring, apt update, apt install).
test_repo() {
  local site=${1:?site dir} image=${2:?image}
  local docker=${DOCKER:-docker} port=8642
  site=$(cd "$site" && pwd)
  python3 -m http.server "$port" --bind 127.0.0.1 -d "$site" >/dev/null 2>&1 &
  local server=$!
  trap 'kill $server 2>/dev/null' RETURN
  sleep 1
  local limits=()
  [ "$docker" = podman ] && limits=(--memory 2g --cpus 2)
  "$docker" run --rm "${limits[@]}" --network host -e PORT="$port" "$image" bash -euc '
    export DEBIAN_FRONTEND=noninteractive
    for f in /etc/apt/sources.list.d/ubuntu.sources /etc/apt/sources.list; do
      if [ -f "$f" ]; then sed -i -E "s#http://(archive|security)\.ubuntu\.com#http://azure.archive.ubuntu.com#g" "$f"; fi
    done
    apt-get update -qq -o Acquire::Retries=3
    apt-get install -y -qq curl ca-certificates > /dev/null
    install -d -m 0755 /etc/apt/keyrings
    curl -fsSL "http://127.0.0.1:$PORT/pova.gpg" -o /etc/apt/keyrings/pova.gpg
    echo "deb [signed-by=/etc/apt/keyrings/pova.gpg] http://127.0.0.1:$PORT stable main" \
      > /etc/apt/sources.list.d/pova.list
    apt-get update -qq -o Acquire::Retries=3
    apt-cache policy pova | sed -n 1,3p
    apt-get install -y -qq pova > /dev/null
    pova --version
    command -v pova-import-cookie mpv yt-dlp pw-record > /dev/null
    echo "apt install pova: OK"'
}

case "${1:-}" in
  fetch) shift; fetch "$@" ;;
  build) shift; build "$@" ;;
  test) shift; test_repo "$@" ;;
  *) sed -n '2,18p' "$0"; exit 1 ;;
esac
