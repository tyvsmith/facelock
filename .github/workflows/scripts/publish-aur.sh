#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: publish-aur.sh <VERSION> <CHECKSUM>}"
CHECKSUM="${2:?Usage: publish-aur.sh <VERSION> <CHECKSUM>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../scripts/release-versions.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/../../../scripts/release-versions.sh"
release_validate_cargo_version "$VERSION"
if release_is_prerelease "$VERSION"; then
  echo "refusing to publish prerelease $VERSION to stable AUR packages" >&2
  exit 1
fi
# The checksum pins the source tarball for every AUR consumer. A malformed
# value, or the digest of empty input (what `curl -sL | sha256sum` yields when
# the download silently fails), must never reach a published recipe.
EMPTY_SHA256="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
if ! [[ "$CHECKSUM" =~ ^[0-9a-f]{64}$ ]] || [ "$CHECKSUM" = "$EMPTY_SHA256" ]; then
  echo "refusing to publish: CHECKSUM is not a plausible source tarball digest: $CHECKSUM" >&2
  exit 1
fi
ARCH_VERSION="$(release_arch_pkgver "$VERSION")"

echo "=== Publishing to AUR ==="

if [ -z "${AUR_SSH_KEY:-}" ]; then
  echo "AUR_SSH_KEY secret not configured. Skipping AUR publish."
  echo "See docs/releasing.md for setup instructions."
  exit 0
fi

# Per-binary checksums for facelock-bin come from the release manifest the
# publish job generates over every published asset (#235). This job runs after
# publication, so the manifest is the released, validated digest of each binary.
# They are read and validated before anything touches SSH, so a bad manifest
# stops here. FACELOCK_RELEASE_MANIFEST_FILE lets the contract test feed one
# in without the network.
# GITHUB_REPOSITORY is set by GitHub Actions; fall back to the canonical repo for local runs.
REPO="${GITHUB_REPOSITORY:-tyvsmith/facelock}"
MANIFEST_FILE="${FACELOCK_RELEASE_MANIFEST_FILE:-}"
if [ -z "$MANIFEST_FILE" ]; then
  MANIFEST_FILE="$(mktemp)"
  trap 'rm -f "$MANIFEST_FILE"' EXIT
  curl -fsSL "https://github.com/${REPO}/releases/download/v${VERSION}/MANIFEST.json" -o "$MANIFEST_FILE"
fi
manifest_sha256() {
  python3 - "$MANIFEST_FILE" "$1" <<'PY'
import json
import sys

manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for asset in manifest.get("assets", []):
    if asset.get("name") == sys.argv[2]:
        print(asset["sha256"])
        break
PY
}
SHA_FACELOCK="$(manifest_sha256 facelock-x86_64-linux-gnu)"
SHA_PAM="$(manifest_sha256 pam_facelock.so)"
SHA_POLKIT="$(manifest_sha256 facelock-polkit-agent-x86_64-linux-gnu)"
: "${SHA_FACELOCK:?missing facelock binary checksum in the release MANIFEST.json}"
: "${SHA_PAM:?missing pam_facelock.so checksum in the release MANIFEST.json}"
: "${SHA_POLKIT:?missing polkit agent checksum in the release MANIFEST.json}"
# Each one pins a published binary for every facelock-bin consumer; a value
# that is not a SHA-256 must never reach a published recipe.
for binary_digest in SHA_FACELOCK SHA_PAM SHA_POLKIT; do
  if ! [[ "${!binary_digest}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "refusing to publish: $binary_digest from the release MANIFEST.json is not a plausible SHA-256: ${!binary_digest}" >&2
    exit 1
  fi
done

# Set up SSH for AUR
mkdir -p ~/.ssh
echo "$AUR_SSH_KEY" > ~/.ssh/aur
chmod 600 ~/.ssh/aur
{
  echo "Host aur.archlinux.org"
  echo "  IdentityFile ~/.ssh/aur"
  echo "  User aur"
} >> ~/.ssh/config
ssh-keyscan aur.archlinux.org >> ~/.ssh/known_hosts 2>/dev/null

RUNNER_UID="$(id -u)"
RUNNER_GID="$(id -g)"

generate_srcinfo() {
  local dir="$1"
  ( cd "$dir" && docker run --rm -v "$(pwd):/pkg" -w /pkg docker.io/library/archlinux:base-devel@sha256:714acd1eef9ae997d95691b1c5220ada0076185b77857c1813f02de0fa83cf7b bash -c "
      printf '%s\\n' 'Server = https://archive.archlinux.org/repos/2026/08/18/\$repo/os/\$arch' > /etc/pacman.d/mirrorlist
      pacman -Sy --noconfirm pacman-contrib >/dev/null
      useradd -m builder
      chown -R builder:builder /pkg
      su builder -c 'makepkg --printsrcinfo > .SRCINFO'
      chown -R ${RUNNER_UID}:${RUNNER_GID} /pkg
    " )
}

# AUR creates a package on first push to a non-existent repo. If clone fails
# because the repo doesn't exist yet, init a fresh one pointing at the same URL.
# Auth / network failures must surface — don't paper over them with a fresh init.
get_or_init_repo() {
  local dir="$1"
  local repo="$2"
  local url="ssh://aur@aur.archlinux.org/${repo}.git"
  local clone_err
  rm -rf "$dir"
  if clone_err="$(git clone "$url" "$dir" 2>&1)"; then
    echo "Cloned existing AUR repo: ${repo}"
    return 0
  fi
  # AUR returns one of these messages when the package doesn't exist yet.
  if echo "$clone_err" | grep -qE "does not appear to be a git repository|Repository not found|fatal: repository '[^']*' not found"; then
    echo "AUR repo ${repo} not found — initializing for first push"
    rm -rf "$dir"
    mkdir -p "$dir"
    ( cd "$dir"
      git init -b master
      git remote add origin "$url"
    )
    return 0
  fi
  echo "ERROR: git clone of ${url} failed for a reason other than 'not found':" >&2
  echo "$clone_err" >&2
  return 1
}

commit_and_push() {
  local dir="$1"
  local message="$2"
  ( cd "$dir"
    git config user.name "facelock-bot"
    git config user.email "facelock@users.noreply.github.com"
    git add PKGBUILD facelock.install .SRCINFO
    if git diff --cached --quiet; then
      echo "No changes to commit for $(basename "$dir")"
      return 0
    fi
    git commit -m "$message"
    git push --set-upstream origin master
  )
}

publish_facelock() {
  local dir="aur-facelock"
  echo "--- Publishing facelock (source build) ---"
  get_or_init_repo "$dir" facelock
  cp dist/PKGBUILD "$dir/PKGBUILD"
  cp dist/facelock.install "$dir/facelock.install"
  sed -i "s/^_tag=.*/_tag=${VERSION}/; s/^pkgver=.*/pkgver=${ARCH_VERSION}/" "$dir/PKGBUILD"
  # dist/PKGBUILD carries a fail-closed placeholder, never SKIP (#283). If the
  # placeholder is missing the recipe has drifted; stop rather than publish a
  # recipe whose integrity line was never finalized.
  if ! grep -Fq "sha256sums=('__SRC_SHA256__')" "$dir/PKGBUILD"; then
    echo "ERROR: dist/PKGBUILD does not carry the __SRC_SHA256__ placeholder" >&2
    exit 1
  fi
  sed -i "s/__SRC_SHA256__/${CHECKSUM}/" "$dir/PKGBUILD"
  generate_srcinfo "$dir"
  commit_and_push "$dir" "Update to v${VERSION}"
}

publish_facelock_bin() {
  local dir="aur-facelock-bin"
  echo "--- Publishing facelock-bin (prebuilt binaries) ---"
  get_or_init_repo "$dir" facelock-bin
  cp dist/PKGBUILD-bin "$dir/PKGBUILD"
  cp dist/facelock.install "$dir/facelock.install"
  sed -i "s/^_tag=.*/_tag=${VERSION}/; s/^pkgver=.*/pkgver=${ARCH_VERSION}/" "$dir/PKGBUILD"
  sed -i "s/__SRC_SHA256__/${CHECKSUM}/" "$dir/PKGBUILD"
  sed -i "s/__FACELOCK_SHA256__/${SHA_FACELOCK}/" "$dir/PKGBUILD"
  sed -i "s/__PAM_SHA256__/${SHA_PAM}/" "$dir/PKGBUILD"
  sed -i "s/__POLKIT_SHA256__/${SHA_POLKIT}/" "$dir/PKGBUILD"
  generate_srcinfo "$dir"
  commit_and_push "$dir" "Update to v${VERSION}"
}

publish_facelock_git() {
  local dir="aur-facelock-git"
  echo "--- Publishing facelock-git (VCS) ---"
  get_or_init_repo "$dir" facelock-git
  cp dist/PKGBUILD-git "$dir/PKGBUILD"
  cp dist/facelock.install "$dir/facelock.install"
  # pkgver is computed by pkgver() at build time from git; no substitution needed.
  generate_srcinfo "$dir"
  commit_and_push "$dir" "Refresh PKGBUILD for v${VERSION}"
}

publish_facelock
publish_facelock_bin
publish_facelock_git

echo "=== AUR publish complete ==="
