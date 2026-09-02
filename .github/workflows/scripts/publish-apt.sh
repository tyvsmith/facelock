#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../scripts/release-versions.sh
source "$SCRIPT_DIR/../../../scripts/release-versions.sh"

REPO_DIR="${1:?Usage: publish-apt.sh <REPO_DIR> <SUITE=DEB>...}"
shift
if [ "$#" -eq 0 ]; then
  echo "publish-apt.sh requires at least one SUITE=DEB input" >&2
  exit 1
fi

declare -a SUITES=()
declare -a DEBS=()
declare -A SEEN_SUITES=()
for suite_deb in "$@"; do
  SUITE="${suite_deb%%=*}"
  DEB="${suite_deb#*=}"
  case "$SUITE" in
    trixie|resolute) ;;
    *) echo "refusing unknown stable APT suite '$SUITE'" >&2; exit 1 ;;
  esac
  if [ -n "${SEEN_SUITES[$SUITE]:-}" ]; then
    echo "duplicate stable APT suite '$SUITE'" >&2
    exit 1
  fi
  if [ "$DEB" = "$suite_deb" ] || [ ! -f "$DEB" ]; then
    echo "invalid APT input '$suite_deb'" >&2
    exit 1
  fi
  SEEN_SUITES[$SUITE]=1
  SUITES+=("$SUITE")
  DEBS+=("$DEB")
done

if [ "${#SUITES[@]}" -ne 2 ]; then
  echo "publish-apt.sh requires exactly one package for each stable suite: trixie, resolute" >&2
  exit 1
fi

for index in "${!DEBS[@]}"; do
  SUITE="${SUITES[index]}"
  DEB="${DEBS[index]}"
  EXPECTED_SUFFIX="$(release_debian_suite_suffix "$SUITE")"
  DEB_VERSION="$(dpkg-deb -f "$DEB" Version)"
  if [[ "$DEB_VERSION" =~ ~(alpha|beta|rc)\. ]]; then
    echo "refusing prerelease $DEB_VERSION in stable APT suite $SUITE" >&2
    exit 1
  fi
  if [[ "$DEB_VERSION" != *"$EXPECTED_SUFFIX" ]]; then
    echo "package version $DEB_VERSION does not match stable APT suite $SUITE ($EXPECTED_SUFFIX)" >&2
    exit 1
  fi
done

echo "=== Building APT repository ==="

if [ -z "${APT_GPG_PRIVATE_KEY:-}" ]; then
  echo "APT_GPG_PRIVATE_KEY secret not configured."
  echo "See docs/releasing.md for setup instructions."
  exit 1
fi

if [ -z "${APT_GPG_PASSPHRASE:-}" ]; then
  echo "APT_GPG_PASSPHRASE secret not configured."
  echo "See docs/releasing.md for setup instructions."
  exit 1
fi

# Configure GPG agent for non-interactive signing. GNUPGHOME is honoured so a
# test can give the publisher its own keyring and agent instead of the user's.
export GNUPGHOME="${GNUPGHOME:-$HOME/.gnupg}"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
echo "allow-preset-passphrase" >> "$GNUPGHOME/gpg-agent.conf"
echo "allow-loopback-pinentry" >> "$GNUPGHOME/gpg-agent.conf"
gpgconf --kill gpg-agent || true
gpg-agent --daemon 2>/dev/null || gpgconf --launch gpg-agent

# Import key
echo "$APT_GPG_PRIVATE_KEY" | gpg --batch --import

# Trust the imported key ultimately
KEY_FPR=$(gpg --list-keys --with-colons | awk -F: '/^pub/{found=1} found && /^fpr/{print $10; exit}')
echo "${KEY_FPR}:6:" | gpg --import-ownertrust

# Preset passphrase into gpg-agent so reprepro can sign non-interactively
KEY_GRIP=$(gpg --list-keys --with-keygrip --with-colons | awk -F: '/^grp/{print $10; exit}')
/usr/lib/gnupg/gpg-preset-passphrase --preset --passphrase "${APT_GPG_PASSPHRASE}" "${KEY_GRIP}"

echo "GPG key imported and passphrase preset: ${KEY_FPR}"

# Set up reprepro base directory
mkdir -p "${REPO_DIR}/conf"
cp dist/apt/conf/distributions "${REPO_DIR}/conf/distributions"

for index in "${!DEBS[@]}"; do
  SUITE="${SUITES[index]}"
  DEB="${DEBS[index]}"
  echo "Adding ${DEB} to ${SUITE}"
  reprepro -b "${REPO_DIR}" includedeb "$SUITE" "$DEB"
  # Clients set up from the v0.1.4 README ask for the `main` suite. They keep
  # receiving the trixie package under that name until 0.3.0 (#310). The step
  # follows its stanza, so retiring the suite is deleting the stanza.
  if [ "$SUITE" = trixie ] && grep -qx 'Codename: main' "${REPO_DIR}/conf/distributions"; then
    echo "Adding ${DEB} to main (compatibility suite for v0.1.4 source entries)"
    reprepro -b "${REPO_DIR}" includedeb main "$DEB"
  fi
done

# `legacy` was the non-TPM suite and nothing is built for it any more. Signed
# empty indexes keep `apt update` succeeding on those clients until 0.3.0.
if grep -qx 'Codename: legacy' "${REPO_DIR}/conf/distributions"; then
  reprepro -b "${REPO_DIR}" export legacy
fi

# Export only the signing key (not the entire keyring)
gpg --export "${KEY_FPR}" > "${REPO_DIR}/tysmith-archive-keyring.gpg"
echo "Public keyring exported ($(du -h "${REPO_DIR}/tysmith-archive-keyring.gpg" | cut -f1))"

echo "=== APT repo structure ==="
find "${REPO_DIR}" -type f | sort
echo ""
for SUITE in $(sed -n 's/^Codename:[[:space:]]*//p' "${REPO_DIR}/conf/distributions"); do
  echo "=== Release file (${SUITE}) ==="
  cat "${REPO_DIR}/dists/${SUITE}/Release" || true
done

echo "=== APT repository built ==="
