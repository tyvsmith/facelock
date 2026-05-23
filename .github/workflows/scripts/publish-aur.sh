#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: publish-aur.sh <VERSION> <CHECKSUM>}"
CHECKSUM="${2:?Usage: publish-aur.sh <VERSION> <CHECKSUM>}"

echo "=== Publishing to AUR ==="

if [ -z "${AUR_SSH_KEY:-}" ]; then
  echo "AUR_SSH_KEY secret not configured. Skipping AUR publish."
  echo "See docs/releasing.md for setup instructions."
  exit 0
fi

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

# Per-binary checksums for facelock-bin come from the Release SHA256SUMS file.
# GITHUB_REPOSITORY is set by GitHub Actions; fall back to the canonical repo for local runs.
REPO="${GITHUB_REPOSITORY:-tyvsmith/facelock}"
SHA_FILE="$(mktemp)"
trap 'rm -f "$SHA_FILE"' EXIT
curl -fsSL "https://github.com/${REPO}/releases/download/v${VERSION}/SHA256SUMS" -o "$SHA_FILE"
SHA_FACELOCK=$(awk '/[[:space:]]facelock-x86_64-linux-gnu$/{print $1}' "$SHA_FILE")
SHA_PAM=$(awk '/[[:space:]]pam_facelock\.so$/{print $1}' "$SHA_FILE")
SHA_POLKIT=$(awk '/[[:space:]]facelock-polkit-agent-x86_64-linux-gnu$/{print $1}' "$SHA_FILE")
: "${SHA_FACELOCK:?missing facelock binary checksum in Release SHA256SUMS}"
: "${SHA_PAM:?missing pam_facelock.so checksum in Release SHA256SUMS}"
: "${SHA_POLKIT:?missing polkit agent checksum in Release SHA256SUMS}"

RUNNER_UID="$(id -u)"
RUNNER_GID="$(id -g)"

generate_srcinfo() {
  local dir="$1"
  ( cd "$dir" && docker run --rm -v "$(pwd):/pkg" -w /pkg archlinux:base-devel bash -c "
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
  sed -i "s/^pkgver=.*/pkgver=${VERSION}/" "$dir/PKGBUILD"
  sed -i "s/sha256sums=('SKIP')/sha256sums=('${CHECKSUM}')/" "$dir/PKGBUILD"
  generate_srcinfo "$dir"
  commit_and_push "$dir" "Update to v${VERSION}"
}

publish_facelock_bin() {
  local dir="aur-facelock-bin"
  echo "--- Publishing facelock-bin (prebuilt binaries) ---"
  get_or_init_repo "$dir" facelock-bin
  cp dist/PKGBUILD-bin "$dir/PKGBUILD"
  cp dist/facelock.install "$dir/facelock.install"
  sed -i "s/^pkgver=.*/pkgver=${VERSION}/" "$dir/PKGBUILD"
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
