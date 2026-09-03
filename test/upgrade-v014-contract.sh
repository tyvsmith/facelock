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
    assert_schema_v6_with_null_device_id \
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
lifecycle_digest="$(grep -oE '"[0-9a-f]{64}"' "$repo_root/test/deb-package-lifecycle.sh" |
    tr -d '"' | head -1)"
[ "$lane_digest" = "$lifecycle_digest" ] ||
    fail "known-embedding digest drifted from the Debian lifecycle gate: $lane_digest vs $lifecycle_digest"

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

# --- the candidate sorts above the predecessor ----------------------------

candidate_version="$(bash "$repo_root/test/upgrade-v014-candidate-version.sh" version)" ||
    fail "the candidate version does not resolve"
[ -n "$candidate_version" ] || fail "the candidate version is empty"

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
