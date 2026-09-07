#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../scripts/release-versions.sh
source "$SCRIPT_DIR/../../../scripts/release-versions.sh"

# The release matrix pins the signing key this publisher must import: a
# rotated secret with no matching pin update would otherwise sign and publish
# under a key the docs never named (#346). A test points this at a throwaway
# matrix instead of skipping the check; there is no flag that skips it.
APT_SIGNING_KEY_MATRIX="${APT_SIGNING_KEY_MATRIX:-$SCRIPT_DIR/../../../dist/release-matrix.json}"

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

if [ ! -f "$APT_SIGNING_KEY_MATRIX" ]; then
  echo "APT signing key matrix not found: $APT_SIGNING_KEY_MATRIX" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to read the apt_signing_key pin from $APT_SIGNING_KEY_MATRIX" >&2
  exit 1
fi
matrix_signing_key_field() {
  python3 -c '
import json, sys
path, field = sys.argv[1], sys.argv[2]
try:
    with open(path, encoding="utf-8") as handle:
        value = json.load(handle)["apt_signing_key"][field]
except (OSError, ValueError, KeyError, TypeError) as error:
    sys.exit(f"{path}: cannot read apt_signing_key.{field} ({error}); "
             "the pin is an object with fingerprint, uid, expires and checked_on")
if not isinstance(value, str) or not value:
    sys.exit(f"{path}: apt_signing_key.{field} must be a non-empty string")
print(value)
' "$APT_SIGNING_KEY_MATRIX" "$1"
}
PIN_FPR="$(matrix_signing_key_field fingerprint)"
PIN_UID="$(matrix_signing_key_field uid)"
PIN_EXPIRES="$(matrix_signing_key_field expires)"

# The pin, checked before the key is trusted for anything: a rotated secret
# whose fingerprint, uid, or expiry drifted from the matrix must fail here,
# not sign a keyring the published docs do not match (#346). The secret must
# hold exactly one secret key, so `SignWith: default` cannot pick a different
# key from the one the pin was checked against.
SECRET_FPRS=$(gpg --list-secret-keys --with-colons | awk -F: '/^sec/{found=1} found && /^fpr/{print $10; found=0}')
SECRET_COUNT=$(printf '%s\n' "$SECRET_FPRS" | grep -c .)
if [ "$SECRET_COUNT" -ne 1 ]; then
  echo "APT_GPG_PRIVATE_KEY must hold exactly one secret key; found ${SECRET_COUNT}:" >&2
  printf '%s\n' "$SECRET_FPRS" | sed 's/^/  /' >&2
  exit 1
fi
KEY_FPR="$SECRET_FPRS"
if [ "$KEY_FPR" != "$PIN_FPR" ]; then
  echo "imported APT signing key does not match the ${APT_SIGNING_KEY_MATRIX} pin:" >&2
  echo "  fingerprint: imported ${KEY_FPR}, matrix pins ${PIN_FPR}" >&2
  exit 1
fi
GPG_COLONS="$(gpg --list-keys --with-colons "$KEY_FPR")"
KEY_UID=$(awk -F: '/^pub/{found=1} found && /^uid/{print $10; exit}' <<<"$GPG_COLONS")
KEY_EXPIRES_EPOCH=$(awk -F: '/^pub/{print $7; exit}' <<<"$GPG_COLONS")
if [ -z "$KEY_EXPIRES_EPOCH" ]; then
  echo "imported signing key has no expiration date; the matrix pins ${PIN_EXPIRES}" >&2
  exit 1
fi
KEY_EXPIRES="$(python3 -c '
import datetime, sys
print(datetime.datetime.fromtimestamp(int(sys.argv[1]), tz=datetime.timezone.utc).strftime("%Y-%m-%d"))
' "$KEY_EXPIRES_EPOCH")"

PIN_MISMATCH=()
[ "$KEY_UID" = "$PIN_UID" ] || PIN_MISMATCH+=("uid: imported '${KEY_UID}', matrix pins '${PIN_UID}'")
[ "$KEY_EXPIRES" = "$PIN_EXPIRES" ] || PIN_MISMATCH+=("expiry: imported ${KEY_EXPIRES}, matrix pins ${PIN_EXPIRES}")
if [ "${#PIN_MISMATCH[@]}" -gt 0 ]; then
  echo "imported APT signing key does not match the ${APT_SIGNING_KEY_MATRIX} pin:" >&2
  printf '  %s\n' "${PIN_MISMATCH[@]}" >&2
  exit 1
fi

# Trust the imported key ultimately
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
