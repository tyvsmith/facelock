#!/usr/bin/env bash
# Container-free contract for the released-predecessor upgrade lanes (#231).
#
# `just test-upgrade-v014` boots two containers, builds a package in each, and
# takes the better part of an hour. Everything that can rot without a container
# is checked here instead, so the failure arrives in seconds:
#
#   * a declared state shape or fault case with no implementation behind it
#   * a proof function that exists but nothing calls — the rot mode
#     .claude/rules/testing.md names explicitly
#   * the known-embedding digest drifting apart from the Debian lifecycle gate's
#     copy of the same fixture
#   * a lane Containerfile growing its own predecessor pin
#   * a candidate version that does not sort above the predecessor
#
# It is wired into `just check` through the lane recipes and can be run alone.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lane="$repo_root/test/upgrade-v014-lane.sh"
failures=0

fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}

require_file() {
    [ -f "$1" ] || fail "missing lane file: ${1#"$repo_root"/}"
}

# --- the lane files exist and parse ---------------------------------------

for path in \
    "$repo_root/test/upgrade-v014-lane.sh" \
    "$repo_root/test/upgrade-v014-predecessor.sh" \
    "$repo_root/test/upgrade-v014-candidate-version.sh" \
    "$repo_root/test/build-upgrade-v014-image.sh" \
    "$repo_root/test/run-upgrade-v014-systemd.sh" \
    "$repo_root/test/Containerfile.upgrade-v014-deb" \
    "$repo_root/test/Containerfile.upgrade-v014-rpm"; do
    require_file "$path"
done
[ "$failures" -eq 0 ] || exit 1

for script in \
    "$repo_root/test/upgrade-v014-lane.sh" \
    "$repo_root/test/upgrade-v014-predecessor.sh" \
    "$repo_root/test/upgrade-v014-candidate-version.sh" \
    "$repo_root/test/build-upgrade-v014-image.sh" \
    "$repo_root/test/run-upgrade-v014-systemd.sh"; do
    bash -n "$script" || fail "${script#"$repo_root"/} does not parse"
    [ -x "$script" ] || fail "${script#"$repo_root"/} is not executable"
done

# --- every declared shape and fault is implemented and dispatched ---------

lane_array() {
    sed -n "s/^$1=(\(.*\))\$/\1/p" "$lane"
}

shapes="$(lane_array SHAPES)"
faults="$(lane_array FAULTS)"
[ -n "$shapes" ] || fail "the lane declares no state shapes"
[ -n "$faults" ] || fail "the lane declares no fault cases"

for shape in $shapes; do
    function_name="seed_shape_${shape//-/_}"
    grep -Eq "^$function_name\(\)" "$lane" ||
        fail "declared shape '$shape' has no $function_name implementation"
    grep -Eq "^[[:space:]]+$shape\)[[:space:]]+$function_name" "$lane" ||
        fail "declared shape '$shape' is not dispatched by seed_shape"
done

for fault in $faults; do
    function_name="fault_${fault//-/_}"
    grep -Eq "^$function_name\(\)" "$lane" ||
        fail "declared fault '$fault' has no $function_name implementation"
    grep -Eq "^[[:space:]]+$fault\)[[:space:]]+$function_name" "$lane" ||
        fail "declared fault '$fault' is not dispatched by run_faults"
done

# --- every proof the issue names is actually invoked ----------------------
#
# A proof function defined but never called is worse than a missing one: the
# lane still reports PASS, and the name in the source says the thing was
# checked. Each entry here is an acceptance bullet of #231.
run_shape_body="$(sed -n '/^run_shape() {/,/^}/p' "$lane")"
[ -n "$run_shape_body" ] || fail "run_shape not found in the lane harness"
for proof in \
    assert_schema_v7_with_null_columns \
    assert_known_embedding_decrypts \
    assert_enrollment_marker_reconciled \
    assert_key_artifacts_preserved \
    assert_adr010_modes \
    record_pam_enabled_state \
    record_adr010_modes \
    assert_pam_path_intact \
    assert_real_password_behavior \
    assert_no_replacement_key_over_encrypted_state \
    record_swtpm_state \
    assert_swtpm_state_untouched \
    open_database_with_candidate_daemon \
    assert_downgrade_usable; do
    grep -Eq "^$proof\(\)" "$lane" || fail "the lane defines no $proof"
    printf '%s\n' "$run_shape_body" | grep -q "$proof" ||
        fail "run_shape never calls $proof, so no shape proves it"
done

# Both key-refusal variants stay. "Malformed" is the case an operator actually
# hits, and it was absent from the first version of this lane.
for variant in missing malformed; do
    grep -q "assert_key_refusal \"\$shape\" $variant" "$lane" ||
        fail "the lane no longer exercises a $variant key over encrypted rows"
done

# The rollback half has its own acceptance bullets; same rule.
downgrade_body="$(sed -n '/^assert_downgrade_usable() {/,/^}/p' "$lane")"
for proof in pkg_downgrade assert_real_password_behavior assert_swtpm_state_untouched; do
    printf '%s\n' "$downgrade_body" | grep -q "$proof" ||
        fail "assert_downgrade_usable never calls $proof"
done

# A snapshot digest lookup must prove the file exists first. `sha256sum` on a
# missing path prints nothing, and grep with an empty pattern matches every
# line, so the unguarded shape passes loudest exactly when the file is gone.
# assert_file_digest_in_snapshot is the guarded form.
if grep -nE 'grep -F[a-z]*q "\$\(sha256sum' "$lane" >/dev/null; then
    fail "the lane looks up a digest without proving the file exists; use assert_file_digest_in_snapshot"
fi

# The invariant snapshot must hash model file content, not just record name,
# size and mode: an upgrade that rewrites a model at the same size and mode
# would otherwise pass "preserve models" without the bytes ever being compared.
# Deleting the hash line from snapshot_model_files must turn this rule red.
model_snapshot_body="$(sed -n '/^snapshot_model_files() {/,/^}/p' "$lane")"
[ -n "$model_snapshot_body" ] || fail "the lane defines no snapshot_model_files"
printf '%s\n' "$model_snapshot_body" | grep -q 'sha256sum' ||
    fail "snapshot_model_files does not hash model file content"
grep -Eq '^[[:space:]]+snapshot_model_files$' "$lane" ||
    fail "snapshot_invariant_state never calls snapshot_model_files"

# The absent-marker lookup must be whole-line. One key path is a prefix of the
# other, so an unanchored match reports a preserved key as a newly created one
# and the "no replacement key" proof inverts.
if grep -n 'grep -Fq "absent|' "$lane" >/dev/null; then
    fail "the lane matches absent markers unanchored; use grep -Fxq"
fi

# The Fedora lane packages host-built binaries, so it can test code that is not
# this checkout. The freshness guard in the image builder is the only thing that
# notices, and a green run without it means nothing.
grep -q 'is older than the workspace source' "$repo_root/test/build-upgrade-v014-image.sh" ||
    fail "the Fedora lane no longer refuses release binaries older than the source"

# The concurrent-key case has to separate O_EXCL from O_TRUNC. One key file on
# disk is true under both, so counting the racers that logged a creation is the
# assertion that carries the claim, and it must not quietly disappear.
grep -q 'racers that created the key' "$lane" ||
    fail "the concurrent-key case no longer counts how many racers created the key"

# --- one known-embedding fixture, one digest ------------------------------

lane_digest="$(sed -n 's/^KNOWN_EMBEDDING_SHA256=\([0-9a-f]\{64\}\)$/\1/p' "$lane")"
[ -n "$lane_digest" ] || fail "the lane pins no known-embedding digest"
# `|| true` because an empty grep exits 1, and under `set -e` that ends this
# script before either branch below runs: the gate would report nothing at all
# about the one thing this section exists to compare.
lifecycle_digest="$(grep -oE '"[0-9a-f]{64}"' "$repo_root/test/deb-package-lifecycle.sh" |
    tr -d '"' | head -1)" || true
if [ -z "$lifecycle_digest" ]; then
    fail "test/deb-package-lifecycle.sh pins no known-embedding digest to compare against"
elif [ "$lane_digest" != "$lifecycle_digest" ]; then
    fail "known-embedding digest drifted from the Debian lifecycle gate: $lane_digest vs $lifecycle_digest"
fi

# The decryption proof must name the model it reads. Scanning for "the first
# 2048-byte blob" finds the mixed shape's plaintext copy of the same fixture, so
# a decrypt that did nothing would pass.
grep -Eq '^shape_probe_label\(\)' "$lane" ||
    fail "the lane defines no shape_probe_label"
for caller in assert_known_embedding_decrypts assert_downgrade_usable; do
    body="$(sed -n "/^$caller() {/,/^}/p" "$lane")"
    [ -n "$body" ] || fail "$caller not found in the lane harness"
    printf '%s\n' "$body" | grep -q probe_known_embedding_digest ||
        fail "$caller does not compare the known embedding's plaintext digest"
    # shellcheck disable=SC2016 # the lane's own `"$shape"` is the literal sought
    printf '%s\n' "$body" | grep -Eq 'shape_probe_label "\$shape"' ||
        fail "$caller does not pin the row it reads to the shape's model label"
done

# --- the lane Containerfiles take the pin, never carry one ----------------

for containerfile in \
    "$repo_root/test/Containerfile.upgrade-v014-deb" \
    "$repo_root/test/Containerfile.upgrade-v014-rpm"; do
    for arg in FACELOCK_PREDECESSOR_URL FACELOCK_PREDECESSOR_SHA256 \
        FACELOCK_PREDECESSOR_SIZE FACELOCK_PREDECESSOR_VERSION; do
        grep -Eq "^ARG $arg\$" "$containerfile" ||
            fail "${containerfile#"$repo_root"/} does not take $arg as a build arg"
    done
    if grep -qE '\b[0-9a-f]{64}\b' "$containerfile"; then
        fail "${containerfile#"$repo_root"/} carries its own digest instead of the matrix pin"
    fi
done

# --- the pin resolves, and resolves to the matrix -------------------------

for pair in "deb-trixie:deb" "rpm-fedora:rpm"; do
    lane_name="${pair%%:*}"
    resolved="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" "$lane_name" --build-args)" ||
        fail "predecessor lane $lane_name does not resolve"
    for key in URL SHA256 SIZE NAME VERSION; do
        printf '%s\n' "$resolved" | grep -q "^FACELOCK_PREDECESSOR_$key=." ||
            fail "predecessor lane $lane_name resolved no $key"
    done
done

# --- --verify-live judges a live asset the way the pin means it -----------
#
# The real thing needs `gh` and the network, so it never runs in `just check`
# and its comparison logic is exactly the kind that rots unseen. Drive it
# against a stub `gh` instead: what matters is which live answers are fatal
# (a moved name, size or digest) and which are not (an asset GitHub carries no
# digest for, which is null on the wire and must not read as a substitution).

verify_live_stub() {
    local answer="$1" stub_dir status=0
    stub_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-upgrade-gh-stub.XXXXXX")"
    cat >"$stub_dir/gh" <<STUB
#!/usr/bin/env bash
printf '%s\n' "$answer"
STUB
    chmod +x "$stub_dir/gh"
    PATH="$stub_dir:$PATH" bash "$repo_root/test/upgrade-v014-predecessor.sh" \
        deb-trixie --verify-live >"$stub_dir/out" 2>&1 || status=$?
    printf '%s\n' "$status"
    cat "$stub_dir/out"
    rm -rf "$stub_dir"
}

pinned_name="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" deb-trixie name)"
pinned_size="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" deb-trixie size)"
pinned_sha="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" deb-trixie sha256)"
tab="$(printf '\t')"

assert_verify_live() {
    local label="$1" answer="$2" want_status="$3" want_text="$4" result status
    result="$(verify_live_stub "$answer")"
    status="$(printf '%s\n' "$result" | head -1)"
    [ "$status" = "$want_status" ] ||
        fail "--verify-live on $label exited $status, expected $want_status"
    printf '%s\n' "$result" | grep -Fq "$want_text" ||
        fail "--verify-live on $label did not report '$want_text'"
}

assert_verify_live "the pinned asset" \
    "$pinned_name$tab$pinned_size${tab}sha256:$pinned_sha" 0 "unchanged"
assert_verify_live "an asset with no digest" \
    "$pinned_name$tab$pinned_size$tab" 0 "serves no digest"
assert_verify_live "a digest that moved" \
    "$pinned_name$tab$pinned_size${tab}sha256:${pinned_sha//?/0}" 1 \
    "serves a different digest"
assert_verify_live "a size that moved" \
    "$pinned_name$tab$((pinned_size + 1))${tab}sha256:$pinned_sha" 1 \
    "pinned asset $(bash "$repo_root/test/upgrade-v014-predecessor.sh" deb-trixie asset_id) changed"
assert_verify_live "a deleted asset" "" 1 "no longer serves asset id"

# --- the candidate sorts above the predecessor ----------------------------

candidate_version="$(bash "$repo_root/test/upgrade-v014-candidate-version.sh" version)" ||
    fail "the candidate version does not resolve"
[ -n "$candidate_version" ] || fail "the candidate version is empty"

# --- and is spelled the way each packager spells it ------------------------
#
# The candidate version is a Cargo version, and `0.2.0-alpha.3` is legal there:
# test/check-release-matrix.py accepts -alpha.N, -beta.N and -rc.N in the
# workspace version. Neither packager accepts it as written. Debian spells it
# `0.2.0~alpha.3-1~deb13u1`; RPM refuses a hyphen in Version at all and splits
# it into `Version: 0.2.0` / `Release: 0.1.alpha.3`. Splicing the Cargo form
# into a package version installs something that never ships -- and in Debian
# it sorts *above* the real release, so the lane's own upgrade guard stays
# quiet while it proves the wrong thing.

builder="$repo_root/test/build-upgrade-v014-image.sh"
# shellcheck disable=SC2016 # the builder's own source line is the literal sought
grep -Fq 'source "$repo_root/scripts/release-versions.sh"' "$builder" ||
    fail "the image builder no longer derives packaging versions from scripts/release-versions.sh"
for helper in release_debian_version release_rpm_version release_rpm_release; do
    grep -Fq "$helper" "$builder" || fail "the image builder no longer calls $helper"
done
# shellcheck disable=SC2016 # the retired splice is the literal sought
if grep -Fq '${target#*-}' "$builder"; then
    fail "the image builder still cuts the Debian revision at the first hyphen"
fi
for arg in FACELOCK_CANDIDATE_RPM_VERSION FACELOCK_CANDIDATE_RPM_RELEASE; do
    grep -Eq "^ARG $arg\$" "$repo_root/test/Containerfile.upgrade-v014-rpm" ||
        fail "test/Containerfile.upgrade-v014-rpm does not take $arg as a build arg"
done
# shellcheck disable=SC2016 # the helper's own default is the literal sought
grep -Fq 'RPM_RELEASE="${3:-1}"' "$repo_root/test/build-rpm-prebuilt.sh" ||
    fail "test/build-rpm-prebuilt.sh no longer takes an RPM Release field"

# shellcheck source=/dev/null
source "$repo_root/scripts/release-versions.sh"

predecessor_deb="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" deb-trixie package_version)"
predecessor_rpm="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" rpm-fedora package_version)"

assert_version() {
    local expected="$1" actual="$2" context="$3"
    [ "$actual" = "$expected" ] || fail "$context: expected '$expected', got '$actual'"
}

for probe in \
    "0.2.0|0.2.0-1~deb13u1|0.2.0|1" \
    "0.2.0-alpha.3|0.2.0~alpha.3-1~deb13u1|0.2.0|0.1.alpha.3"; do
    IFS='|' read -r probe_version want_deb want_rpm_version want_rpm_release <<<"$probe"
    assert_version "$want_deb" "$(release_debian_version "$probe_version" 1 trixie)" \
        "Debian version for candidate $probe_version"
    assert_version "$want_rpm_version" "$(release_rpm_version "$probe_version")" \
        "RPM Version for candidate $probe_version"
    assert_version "$want_rpm_release" "$(release_rpm_release "$probe_version" 1)" \
        "RPM Release for candidate $probe_version"

    # The native comparators are the authority the lane defers to, so ask them
    # here too where they happen to be installed. A host without them still
    # gets every string above checked; it just does not get the ordering half.
    if command -v dpkg >/dev/null 2>&1; then
        dpkg --compare-versions "$want_deb" gt "$predecessor_deb" ||
            fail "$want_deb does not sort above the pinned predecessor $predecessor_deb"
    else
        dpkg_absent=1
    fi
    if command -v rpmdev-vercmp >/dev/null 2>&1; then
        # rpmdev-vercmp exits 11 when the first argument is the newer one.
        vercmp_status=0
        rpmdev-vercmp "$want_rpm_version-$want_rpm_release" "$predecessor_rpm" \
            >/dev/null 2>&1 || vercmp_status=$?
        [ "$vercmp_status" -eq 11 ] ||
            fail "$want_rpm_version-$want_rpm_release does not sort above the pinned predecessor $predecessor_rpm"
    else
        rpmdev_absent=1
    fi
done
[ -z "${dpkg_absent:-}" ] ||
    echo "NOTE: dpkg is not installed; the Debian ordering half was not run" >&2
[ -z "${rpmdev_absent:-}" ] ||
    echo "NOTE: rpmdev-vercmp is not installed; the RPM ordering half was not run" >&2

# --- the entrypoints stay wired -------------------------------------------

justfile="$repo_root/justfile"
for recipe in test-upgrade-v014 test-upgrade-v014-deb test-upgrade-v014-rpm \
    test-upgrade-v014-contract test-upgrade-v014-pins; do
    grep -Eq "^$recipe([ :])" "$justfile" || fail "justfile has no $recipe recipe"
done
grep -q "test/build-upgrade-v014-image.sh deb" "$justfile" ||
    fail "test-upgrade-v014-deb does not build its lane image"
grep -q "test/build-upgrade-v014-image.sh rpm" "$justfile" ||
    fail "test-upgrade-v014-rpm does not build its lane image"

skill="$repo_root/.claude/skills/packaging-test/SKILL.md"
grep -q 'just test-upgrade-v014' "$skill" ||
    fail "the packaging-test skill does not route anything to just test-upgrade-v014"

if [ "$failures" -ne 0 ]; then
    echo "FAIL: $failures upgrade-lane contract violation(s)" >&2
    exit 1
fi

echo "upgrade lane contract: OK ($(printf '%s' "$shapes" | wc -w) shapes, $(printf '%s' "$faults" | wc -w) faults, candidate $candidate_version)"
