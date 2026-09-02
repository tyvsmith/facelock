#!/usr/bin/env bash
# Runs inside the image test/Containerfile.apt-client builds, as root, with the
# repository mounted read-only at /src and no network.
#
# Publishes one package per codenamed suite through the real stable publisher
# under an ephemeral signing key, packs and unpacks the result the way
# release.yml and pages.yml do, then acts as a clean APT client for every suite
# dist/apt/conf/distributions declares: the codenamed pair and the v0.1.4 names
# `main` and `legacy` that stay published until 0.3.0 (#310).
#
#   apt-client-lane.sh [--manifest <suite>=<path>]... [--compat <suite>=<source>]...
#
# A suite without a manifest is published from a stand-in package at this
# tree's stable version. The lane proves the repository shape and the client
# path; the package payload belongs to the suite package gates.
set -euo pipefail

src=/src
work=/work
# shellcheck source=/dev/null
source "$src/scripts/release-versions.sh"

fail() {
    echo "apt client lane: $*" >&2
    exit 1
}

declare -A manifests=()
declare -A compat_source=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --manifest)
            [ "$#" -ge 2 ] || fail "--manifest needs <suite>=<path>"
            manifests["${2%%=*}"]="${2#*=}"
            shift 2
            ;;
        --compat)
            [ "$#" -ge 2 ] || fail "--compat needs <suite>=<source>"
            compat_source["${2%%=*}"]="${2#*=}"
            shift 2
            ;;
        *) fail "unknown argument: $1" ;;
    esac
done

mkdir -p "$work/debs" "$work/download"
mkdir -m 700 "$work/gpgv-home"
mapfile -t declared_suites < <(sed -n 's/^Codename:[[:space:]]*//p' "$src/dist/apt/conf/distributions")
[ "${#declared_suites[@]}" -gt 0 ] || fail "dist/apt/conf/distributions declares no suite"
for suite in "${!compat_source[@]}"; do
    case " ${declared_suites[*]} " in
        *" $suite "*) ;;
        *) fail "compatibility suite $suite is not declared in dist/apt/conf/distributions" ;;
    esac
done

echo "=== Packages ==="
tree_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$src/Cargo.toml" | head -1)"
[ -n "$tree_version" ] || fail "Cargo.toml declares no workspace version"
stable_version="${tree_version%%-*}"
declare -A package_path=()
declare -A package_version=()
for suite in trixie resolute; do
    if [ -n "${manifests[$suite]:-}" ]; then
        manifest="${manifests[$suite]}"
        bash "$src/test/deb-package-contract.sh" --manifest "$manifest"
        mapfile -t packages < <(grep -E '\.deb$' "$manifest")
        [ "${#packages[@]}" -eq 1 ] || fail "$suite manifest must name exactly one .deb payload: $manifest"
        package_path[$suite]="$(dirname "$manifest")/${packages[0]}"
    else
        version="$(release_debian_version "$stable_version" 1 "$suite")"
        stage="$work/stand-in/$suite"
        mkdir -p "$stage/DEBIAN"
        printf '%s\n' \
            'Package: facelock' \
            "Version: $version" \
            'Architecture: amd64' \
            'Section: admin' \
            'Priority: optional' \
            'Maintainer: Facelock APT lane <apt-lane@example.invalid>' \
            "Description: stand-in for the $suite package in the APT client lane" \
            > "$stage/DEBIAN/control"
        package_path[$suite]="$work/debs/facelock_${version}_amd64.deb"
        dpkg-deb --build "$stage" "${package_path[$suite]}" >/dev/null
    fi
    package_version[$suite]="$(dpkg-deb --field "${package_path[$suite]}" Version)"
    echo "$suite: ${package_path[$suite]} (${package_version[$suite]})"
done

echo "=== Ephemeral signing key ==="
keygen_home="$work/keygen"
mkdir -m 700 "$keygen_home"
passphrase="apt-client-lane"
GNUPGHOME="$keygen_home" gpg --batch --quiet --pinentry-mode loopback --passphrase "$passphrase" \
    --quick-generate-key "Facelock APT lane <apt-lane@example.invalid>" ed25519 sign never
private_key="$(GNUPGHOME="$keygen_home" gpg --batch --quiet --pinentry-mode loopback \
    --passphrase "$passphrase" --armor --export-secret-keys)"
GNUPGHOME="$keygen_home" gpgconf --kill gpg-agent

# The real publisher, from the repository root it expects, with the key and
# passphrase reaching it the way the CI secrets do, and its own GNUPGHOME.
repo="$work/apt-repo"
(
    cd "$src"
    GNUPGHOME="$work/publisher-gnupg" APT_GPG_PRIVATE_KEY="$private_key" APT_GPG_PASSPHRASE="$passphrase" \
        bash .github/workflows/scripts/publish-apt.sh "$repo" \
        "trixie=${package_path[trixie]}" "resolute=${package_path[resolute]}"
)

echo "=== Release artifact and Pages tree ==="
# release.yml packs exactly these three; pages.yml unpacks into _site/apt.
site="$work/_site/apt"
tar -czf "$work/apt-repo.tar.gz" -C "$repo" --exclude='conf' --exclude='db' \
    dists pool tysmith-archive-keyring.gpg
mkdir -p "$site"
tar -xzf "$work/apt-repo.tar.gz" -C "$site/"
for suite in "${declared_suites[@]}"; do
    for index in Release InRelease; do
        [ -f "$site/dists/$suite/$index" ] || fail "served tree lacks dists/$suite/$index"
    done
    if ! verify_output="$(GNUPGHOME="$work/gpgv-home" gpgv --keyring "$site/tysmith-archive-keyring.gpg" "$site/dists/$suite/InRelease" 2>&1)"; then
        printf '%s\n' "$verify_output"
        fail "dists/$suite/InRelease does not verify against the published keyring"
    fi
    echo "signed: dists/$suite/InRelease"
done
for suite in "${!compat_source[@]}"; do
    index="$site/dists/$suite/facelock/binary-amd64/Packages"
    [ -f "$index" ] || fail "compatibility suite $suite has no package index"
    if [ "${compat_source[$suite]}" = none ]; then
        ! grep -q '^Package:' "$index" || fail "compatibility suite $suite lists a package"
        echo "empty: dists/$suite"
    else
        cmp -s "$index" "$site/dists/${compat_source[$suite]}/facelock/binary-amd64/Packages" \
            || fail "compatibility suite $suite index differs from ${compat_source[$suite]}"
        echo "mirrors ${compat_source[$suite]}: dists/$suite"
    fi
done

echo "=== APT client ==="
install -d -m 0755 /etc/apt/keyrings
install -m 0644 "$site/tysmith-archive-keyring.gpg" /etc/apt/keyrings/tysmith-archive-keyring.gpg
# Only the Facelock source is under test, and there is no network anyway.
mkdir -p "$work/image-sources"
find /etc/apt/sources.list.d -mindepth 1 -maxdepth 1 -exec mv -t "$work/image-sources" {} +
[ ! -f /etc/apt/sources.list ] || mv /etc/apt/sources.list "$work/image-sources/"

public_base='https://tysmith.me/facelock/apt'
served_base="file://$site"
# The suite a client on this entry is served from: itself, a compatibility
# suite's source, or nothing.
serving_suite() {
    local suite="$1"
    if [ -n "${compat_source[$suite]:-}" ]; then
        printf '%s\n' "${compat_source[$suite]}"
    else
        printf '%s\n' "$suite"
    fi
}
for suite in "${declared_suites[@]}"; do
    # The README entry, identical in v0.1.4 and now but for the suite; only the
    # base is rewritten so the served tree stands in for the public host.
    public_line="deb [signed-by=/etc/apt/keyrings/tysmith-archive-keyring.gpg] $public_base $suite facelock"
    served_line="${public_line/$public_base/$served_base}"
    echo "== $suite"
    echo "entry: $public_line"
    printf '%s\n' "$served_line" > /etc/apt/sources.list.d/facelock.list
    rm -rf /var/lib/apt/lists/*
    if ! update_output="$(apt-get update 2>&1)"; then
        printf '%s\n' "$update_output"
        fail "apt update failed with the $suite source entry"
    fi
    if grep -Eq '^(W|E):' <<<"$update_output"; then
        printf '%s\n' "$update_output"
        fail "apt update warned with the $suite source entry"
    fi
    policy="$(apt-cache policy facelock 2>&1)"
    printf '%s\n' "$policy"
    source_suite="$(serving_suite "$suite")"
    if [ "$source_suite" = none ]; then
        ! grep -q '^  Candidate:' <<<"$policy" || fail "suite $suite offers a facelock candidate; it must serve none"
        echo "ok: $suite updates and serves no package"
        continue
    fi
    expected="${package_version[$source_suite]}"
    grep -qx "  Candidate: $expected" <<<"$policy" || fail "suite $suite candidate is not $expected"
    grep -q " $suite/facelock amd64 Packages" <<<"$policy" || fail "suite $suite candidate does not come from $suite/facelock"
    rm -f "$work"/download/*.deb
    (cd "$work/download" && apt-get download facelock >/dev/null 2>&1) \
        || fail "apt-get download facelock failed from suite $suite"
    downloaded="$(find "$work/download" -maxdepth 1 -name '*.deb' -print -quit)"
    [ -n "$downloaded" ] || fail "apt-get download from suite $suite produced no package"
    cmp -s "$downloaded" "${package_path[$source_suite]}" \
        || fail "package downloaded from suite $suite differs from the published $source_suite package"
    echo "ok: $suite serves $expected from $source_suite"
done

echo ""
echo "APT client lane: OK"
