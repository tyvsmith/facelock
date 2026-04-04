#!/usr/bin/env bash
set -euo pipefail

TPM_DEB="${1:?Usage: publish-apt.sh <TPM_DEB> <LEGACY_DEB> <REPO_DIR>}"
LEGACY_DEB="${2:?Usage: publish-apt.sh <TPM_DEB> <LEGACY_DEB> <REPO_DIR>}"
REPO_DIR="${3:?Usage: publish-apt.sh <TPM_DEB> <LEGACY_DEB> <REPO_DIR>}"

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

# Configure GPG agent for non-interactive signing
mkdir -p ~/.gnupg
chmod 700 ~/.gnupg
echo "allow-preset-passphrase" >> ~/.gnupg/gpg-agent.conf
echo "allow-loopback-pinentry" >> ~/.gnupg/gpg-agent.conf
gpgconf --kill gpg-agent
gpg-agent --daemon

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

# Add TPM .deb to 'main' suite
echo "Adding TPM .deb to main: ${TPM_DEB}"
reprepro -b "${REPO_DIR}" includedeb main "${TPM_DEB}"

# Add legacy .deb to 'legacy' suite
echo "Adding legacy .deb to legacy: ${LEGACY_DEB}"
reprepro -b "${REPO_DIR}" includedeb legacy "${LEGACY_DEB}"

# Export only the signing key (not the entire keyring)
gpg --export "${KEY_FPR}" > "${REPO_DIR}/tysmith-archive-keyring.gpg"
echo "Public keyring exported ($(du -h "${REPO_DIR}/tysmith-archive-keyring.gpg" | cut -f1))"

echo "=== APT repo structure ==="
find "${REPO_DIR}" -type f | sort
echo ""
echo "=== Release file (main) ==="
cat "${REPO_DIR}/dists/main/Release" || true

echo "=== APT repository built ==="
