#!/usr/bin/env bash
# Contract for the direct release artifacts: builders that only produce
# workflow artifacts, and one publish job that assembles, validates and
# publishes them exactly once.
#
# The release workflow runs only on a `v*` tag, so its shape cannot be proven
# by running it. This gate proves the shape statically and proves the decisions
# it delegates -- the canonical asset allowlist, staging, the tag check, the
# draft checks, the digest attestations and the manifest -- by fixture.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
workflow_path="${FACELOCK_RELEASE_WORKFLOW:-.github/workflows/release.yml}"
helper_path="${FACELOCK_RELEASE_ASSETS:-.github/workflows/scripts/release-assets.sh}"

fail() {
    echo "release artifacts contract: $*" >&2
    exit 1
}

# ------------------------------------------------------------------ workflow

[ -f "$workflow_path" ] || fail "missing release workflow: $workflow_path"
[ -x "$helper_path" ] || fail "release asset helper must be executable: $helper_path"

job_body() {
    awk -v job="$1" '
        $0 == "  " job ":" { inside = 1; next }
        inside && /^  [A-Za-z]/ { inside = 0 }
        inside { print }
    ' "$workflow_path"
}

job_names() {
    awk '
        /^jobs:/ { inside = 1; next }
        inside && /^[A-Za-z]/ { inside = 0 }
        inside && match($0, /^  [a-z][a-z0-9-]*:$/) {
            name = $0
            sub(/^  /, "", name)
            sub(/:$/, "", name)
            print name
        }
    ' "$workflow_path"
}

# The `needs:` list of one job, as space-delimited job names.
job_needs() {
    job_body "$1" | awk '
        /^    needs:/ {
            sub(/^    needs:[[:space:]]*/, "")
            gsub(/[][,]/, " ")
            print
        }
    ' | tr -s ' '
}

requires_job() {
    local job="$1" dependency="$2"
    case " $(job_needs "$job") " in
        *" $dependency "*) return 0 ;;
        *) return 1 ;;
    esac
}

# The `permissions:` body of one job, as `scope: value` lines.
job_permissions() {
    job_body "$1" | awk '
        /^    permissions:/ { inside = 1; next }
        inside && /^    [A-Za-z]/ { inside = 0 }
        inside { sub(/^ +/, ""); print }
    '
}

builders=(
    metadata
    build
    download-ort
    prepare-cargo-vendor
    build-deb
    build-rpm
    build-nix
    publish-apt
)
expected_jobs=("${builders[@]}" publish publish-aur trigger-pages)

actual_jobs="$(job_names | LC_ALL=C sort)"
wanted_jobs="$(printf '%s\n' "${expected_jobs[@]}" | LC_ALL=C sort)"
[ "$actual_jobs" = "$wanted_jobs" ] ||
    fail "release jobs drifted; every job needs a permission ceiling and a place in the publish graph: $(echo "$actual_jobs" | tr '\n' ' ')"

# A workflow-level write grant reaches every builder. The floor is deny-all and
# each job asks for exactly what it needs.
if awk '/^permissions:/ { inside = 1; next } inside && /^[A-Za-z]/ { inside = 0 } inside { print }' \
    "$workflow_path" | grep -q .; then
    fail "workflow-level permissions must be the deny-all floor, not a scope list"
fi
grep -Eq '^permissions:[[:space:]]*\{\}[[:space:]]*$' "$workflow_path" ||
    fail "workflow must declare the deny-all permission floor: permissions: {}"

for job in "${expected_jobs[@]}"; do
    permissions="$(job_permissions "$job")"
    [ -n "$permissions" ] || fail "job $job declares no permissions: block"
    if [ "$job" = publish ]; then
        printf '%s\n' "$permissions" | grep -Eq '^contents:[[:space:]]*write$' ||
            fail "the publish job must hold contents: write"
    else
        if printf '%s\n' "$permissions" | grep -Eq '^contents:[[:space:]]*write$'; then
            fail "only the publish job may hold contents: write, not $job"
        fi
    fi
done

# A builder that can reach the release is a builder that can publish one. Every
# job but `publish` compiles or packages untrusted-by-construction code (every
# dependency's build.rs, rpmbuild, dpkg-buildpackage), so none of them may hold
# the publication credential or a step that writes the release.
for job in "${expected_jobs[@]}"; do
    if [ "$job" = publish ]; then
        continue
    fi
    body="$(job_body "$job")"
    for forbidden in 'RELEASE_PAT' 'action-gh-release' 'gh release'; do
        if printf '%s\n' "$body" | grep -Fq "$forbidden"; then
            fail "job $job must not touch the release ($forbidden); builders produce artifacts only"
        fi
    done
done

build_job="$(job_body build)"
printf '%s\n' "$build_job" | grep -Fq 'name: release-binaries' ||
    fail "the build job must publish its binaries as a workflow artifact"
if printf '%s\n' "$build_job" | grep -Fq 'SHA256SUMS'; then
    fail "the build job must not publish a partial checksum file; MANIFEST.json covers every asset"
fi

for producer in build build-deb build-rpm publish-apt; do
    job_body "$producer" | grep -Eq '^[[:space:]]+name: release-[a-z-]+' ||
        fail "$producer must hand its release payload to publish as a workflow artifact"
done
for digest_producer in build download-ort prepare-cargo-vendor build-deb build-rpm publish-apt; do
    job_body "$digest_producer" | grep -Fq 'release-digests-' ||
        fail "$digest_producer must attest the digests of what it produced"
done

publish_job="$(job_body publish)"
for need in "${builders[@]}"; do
    requires_job publish "$need" ||
        fail "the publish job must wait for $need before publishing"
done
requires_job publish build ||
    fail "the only job with a release step must depend on the builders it publishes"
# The whole reason creation left `build`: the job that holds the publication
# credential must not execute code this project does not own.
publish_statements="$(printf '%s\n' "$publish_job" | grep -v '^[[:space:]]*#')"
for toolchain in '(^|[^-A-Za-z])cargo[[:space:]]' 'rustup' 'rustc' 'rust-toolchain' \
    'rpmbuild' 'dpkg-buildpackage' 'just[[:space:]]+build'; do
    if printf '%s\n' "$publish_statements" | grep -Eq "$toolchain"; then
        fail "the publish job must not compile or package anything ($toolchain); it holds the publication credential"
    fi
done

# Every step assertion reads the comment-stripped statements: a step that was
# commented out, or replaced by `: # disabled`, must not pass as present.
printf '%s\n' "$publish_statements" | grep -Eq '^[[:space:]]+draft: true[[:space:]]*$' ||
    fail "the publish job must create the GitHub release as a draft"
# Literal workflow expressions; nothing here is a shell expansion.
# shellcheck disable=SC2016
printf '%s\n' "$publish_statements" | grep -Fq 'prerelease: ${{ needs.metadata.outputs.prerelease }}' ||
    fail "the draft must carry the validated prerelease identity"
# The helper invocations are matched with their trailing space so that a
# `verify-digests-disabled` lookalike cannot stand in for the real call.
# shellcheck disable=SC2016
for step in '$HELPER verify-tag ' '$HELPER stage expected-assets' \
    '$HELPER verify-digests artifacts job-outputs.json ' '$HELPER verify-creatable ' \
    'PRERELEASE" final' 'artifacts job-outputs.json assets actual-assets MANIFEST.json' \
    'draft=false'; do
    printf '%s\n' "$publish_statements" | grep -Fq "$step" ||
        fail "the publish job must run ${step% } before the release becomes public"
done

# Every release write goes through the dedicated publication credential.
upload_steps="$(grep -c 'uses: softprops/action-gh-release@' "$workflow_path" || true)"
# shellcheck disable=SC2016
upload_tokens="$(grep -cF 'token: ${{ secrets.RELEASE_PAT }}' "$workflow_path" || true)"
[ "$upload_steps" = 1 ] ||
    fail "exactly one release-writing step may exist, found $upload_steps"
[ "$upload_steps" = "$upload_tokens" ] ||
    fail "every release upload must pass the publication token: $upload_steps step(s), $upload_tokens token(s)"

# Publishing may never mint or move a tag: the maintainer's tag is an input.
if grep -Eq 'tag_name=|target_commitish' "$workflow_path"; then
    fail "publishing must not send a tag or target commitish; the tag is verified, never written"
fi
if grep -Eq 'git([[:space:]]+-C[[:space:]]+[^[:space:]]+)?[[:space:]]+(tag|push)([[:space:]]|$)' "$workflow_path"; then
    fail "the release workflow must not create, move or push a git tag"
fi

requires_job trigger-pages publish ||
    fail "trigger-pages must run after the release is published"

publish_aur_job="$(job_body publish-aur)"
requires_job publish-aur publish ||
    fail "publish-aur consumes published release assets and must run after publish"
if printf '%s\n' "$publish_aur_job" | grep -Fq 'SHA256SUMS'; then
    fail "publish-aur must take its binary digests from MANIFEST.json"
fi
grep -Fq 'MANIFEST.json' .github/workflows/scripts/publish-aur.sh ||
    fail "the AUR publisher must read per-binary digests from the release manifest"
# shellcheck disable=SC2016
if grep -Fq 'releases/download/v${VERSION}/SHA256SUMS' .github/workflows/scripts/publish-aur.sh; then
    fail "the AUR publisher must not fetch the retired SHA256SUMS asset"
fi

# The RPM container digest is a manifest input, and `container.image` cannot
# read an env context: the duplicate must be held equal here.
rpm_job="$(job_body build-rpm)"
rpm_container_image="$(printf '%s\n' "$rpm_job" | awk '/^      image: /{ sub(/^      image: /, ""); print; exit }')"
rpm_env_image="$(printf '%s\n' "$rpm_job" | awk '/^      BUILD_IMAGE: /{ sub(/^      BUILD_IMAGE: /, ""); print; exit }')"
[ -n "$rpm_container_image" ] || fail "build-rpm declares no pinned container image"
[ "$rpm_container_image" = "$rpm_env_image" ] ||
    fail "build-rpm BUILD_IMAGE ('$rpm_env_image') must repeat container.image ('$rpm_container_image')"

# Only what a validator inspected may be published.
printf '%s\n' "$rpm_job" | grep -Fq 'steps.rpm.outputs.rpm' ||
    fail "build-rpm must validate the exact payload package the metadata names"

# The attesting set the helper pins must be exactly the artifacts the workflow
# uploads, each bound to a job output. The artifact store is writable by every
# job in the run; a job output is recorded under the job that produced it and
# no other job can rewrite it, so the output is what makes the artifact
# evidence. Adding or removing an attesting job moves both or fails here.

# The `outputs:` block of one job, as `key: expression` lines.
job_outputs_block() {
    job_body "$1" | awk '
        /^    outputs:/ { inside = 1; next }
        inside && /^    [A-Za-z]/ { inside = 0 }
        inside && /^      [a-z]/ { sub(/^ +/, ""); print }
    '
}

# The step of one job that runs attest-digests.sh, as its own lines.
attest_step() {
    job_body "$1" | awk '
        function flush() { if (step ~ /attest-digests\.sh/) printf "%s", step; step = "" }
        /^      - / { flush() }
        { step = step $0 "\n" }
        END { flush() }
    '
}

workflow_attestations() {
    local wf_job body step attest_job name_pattern outputs line key expression suite slot
    for wf_job in build download-ort prepare-cargo-vendor build-deb build-rpm publish-apt; do
        body="$(job_body "$wf_job")"
        step="$(attest_step "$wf_job")"
        [ -n "$step" ] || fail "job $wf_job runs no attest-digests.sh step"
        printf '%s\n' "$step" | grep -Eq '^        id: attest$' ||
            fail "job $wf_job must give its attestation step the id attest so its digest can be a job output"
        attest_job="$(printf '%s\n' "$step" |
            sed -n 's|.*attest-digests\.sh \([a-z-]*\) .*|\1|p' | head -1)"
        name_pattern="$(printf '%s\n' "$body" |
            sed -n 's/^ *name: release-digests-\(.*\)$/\1/p' | head -1)"
        [ -n "$attest_job" ] || fail "job $wf_job uploads no digest attestation"
        [ -n "$name_pattern" ] || fail "job $wf_job names no digest artifact"
        outputs="$(job_outputs_block "$wf_job" | grep -E '^attestation' || true)"
        [ -n "$outputs" ] ||
            fail "job $wf_job declares no attestation output; publish cannot bind its artifact to the job"
        while IFS= read -r line; do
            key="${line%%:*}"
            expression="${line#*: }"
            # Literal workflow expressions; nothing here is a shell expansion.
            # shellcheck disable=SC2016
            case "$key" in
                attestation)
                    [ "$expression" = '${{ steps.attest.outputs.sha256 }}' ] ||
                        fail "job $wf_job output $key must be the attest step's sha256, not: $expression"
                    printf '%s\t%s\t%s\n' "$name_pattern" "$attest_job" "$key"
                    ;;
                attestation-*)
                    # A matrix job shares one outputs map across its legs. Each
                    # leg fills only its own suite's key and leaves the others
                    # empty, and the service does not record an empty value over
                    # a set one.
                    suite="${key#attestation-}"
                    [ "$expression" = "\${{ matrix.suite == '$suite' && steps.attest.outputs.sha256 || '' }}" ] ||
                        fail "job $wf_job output $key must be set only by the $suite leg, not: $expression"
                    slot="$(printf '%s' "$name_pattern" | sed "s/\${{ matrix.suite }}/$suite/")"
                    [ "$slot" != "$name_pattern" ] ||
                        fail "job $wf_job declares a per-suite output but its artifact name carries no matrix.suite"
                    printf '%s\t%s\t%s\n' "$slot" "$attest_job" "$key"
                    ;;
            esac
        done <<<"$outputs"
    done
}

attesting_set="$(mktemp "${TMPDIR:-/tmp}/facelock-attesting.XXXXXX")"
"$helper_path" expected-attestations false >"$attesting_set"
diff <(workflow_attestations | LC_ALL=C sort) <(LC_ALL=C sort "$attesting_set") >/dev/null ||
    fail "the pinned attesting set differs from the release workflow's attestation outputs: $(diff <(workflow_attestations | LC_ALL=C sort) <(LC_ALL=C sort "$attesting_set") | tr '\n' ' ')"
rm -f "$attesting_set"

# The bindings reach the helper through the environment and a file, never
# through a shell line: a builder controls the text of its own output.
[ "$(printf '%s\n' "$publish_statements" | grep -Ec '^ +[A-Z_]+: \$\{\{ toJSON\(needs\) \}\}$' || true)" = 1 ] ||
    fail "the publish job must pass toJSON(needs) to the helper through exactly one env value"
[ "$(printf '%s\n' "$publish_job" | grep -c 'toJSON(needs)' || true)" = 1 ] ||
    fail "toJSON(needs) may appear in the publish job only as that env value"
# shellcheck disable=SC2016
printf '%s\n' "$publish_statements" | grep -Eq '^ +printf .%s\\n. "\$JOB_OUTPUTS" >job-outputs\.json$' ||
    fail "the publish job must write job-outputs.json from the JOB_OUTPUTS env value"
# shellcheck disable=SC2016
needs_lines="$(printf '%s\n' "$publish_job" | grep -F '${{ needs.' || true)"
while IFS= read -r line; do
    [ -n "$line" ] || continue
    printf '%s\n' "$line" | grep -Eq '^ +[A-Za-z_-]+: \$\{\{ needs\.[a-z-]+\.outputs\.[a-z-]+ \}\}$' ||
        fail "a needs value reaches a shell line of the publish job; pass it through env: $line"
    case "$line" in
        *' run: '*) fail "a needs value is interpolated into a run line of the publish job: $line" ;;
    esac
done <<<"$needs_lines"

# ------------------------------------------------------------- helper: shape

git_command='git([[:space:]]+-C[[:space:]]+[^[:space:]]+)?[[:space:]]+'
grep -Eq "${git_command}tag[[:space:]]+-v" "$helper_path" ||
    fail "tag verification must verify a present signature with git tag -v"
# Allowlist, not blocklist: the only `git tag` the helper may run is a
# verification. Anything else (create, delete, move, list) is refused.
other_tag_commands="$(grep -E "${git_command}tag([[:space:]]|$)" "$helper_path" |
    grep -Ev "${git_command}tag[[:space:]]+(-v|--verify)([[:space:]]|$)" || true)"
[ -z "$other_tag_commands" ] ||
    fail "the release asset helper may only run git tag -v; found: $other_tag_commands"
if grep -Eq "${git_command}push" "$helper_path"; then
    fail "the release asset helper must never push"
fi

# ------------------------------------------------------- helper: by fixture

work="$(mktemp -d "${TMPDIR:-/tmp}/facelock-release-artifacts.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT

assert_rejects() {
    local context="$1" needle="$2"
    shift 2
    local output
    if output="$("$helper_path" "$@" 2>&1)"; then
        fail "$context: helper accepted what it must reject"
    fi
    printf '%s\n' "$output" | grep -Fq "$needle" ||
        fail "$context: rejected for an unrelated reason: $output"
    echo "release artifacts case: $context rejected"
}

# --- the canonical allowlist

stable_expected="$work/expected-stable"
"$helper_path" expected 0.2.0 1 1 false final >"$stable_expected"
for wanted in \
    'facelock-x86_64-linux-gnu' \
    'pam_facelock\.so' \
    'facelock-polkit-agent-x86_64-linux-gnu' \
    'facelock_0\.2\.0-1~deb13u1_amd64\.deb' \
    'facelock_0\.2\.0-1~ubuntu26\.04\.1_amd64\.deb' \
    'facelock-0\.2\.0-1\.fc[0-9]+\.x86_64\.rpm' \
    'facelock-debuginfo-0\.2\.0-1\.fc[0-9]+\.x86_64\.rpm' \
    'facelock-debugsource-0\.2\.0-1\.fc[0-9]+\.x86_64\.rpm' \
    'apt-repo\.tar\.gz' \
    'MANIFEST\.json'; do
    grep -Fq "	$wanted" "$stable_expected" ||
        fail "canonical stable allowlist omits $wanted: $(cat "$stable_expected")"
done

prerelease_expected="$work/expected-prerelease"
"$helper_path" expected 0.2.0-alpha.1 1 1 true final >"$prerelease_expected"
grep -Fq '	facelock_0\.2\.0~alpha\.1-1~deb13u1_amd64\.deb' "$prerelease_expected" ||
    fail "prerelease allowlist does not carry the Debian prerelease upstream version"
grep -Fq '	facelock-0\.2\.0-0\.1\.alpha\.1\.fc[0-9]+\.x86_64\.rpm' "$prerelease_expected" ||
    fail "prerelease allowlist does not carry the monotonic RPM prerelease release"
if grep -Fq 'apt-repo' "$prerelease_expected"; then
    fail "a prerelease publishes no APT repository, so it must not be an expected asset"
fi

builders_expected="$work/expected-builders"
"$helper_path" expected 0.2.0 1 1 false builders >"$builders_expected"
if grep -q 'MANIFEST' "$builders_expected"; then
    fail "the builder-stage allowlist must not expect the manifest the publish job adds"
fi

# --- allowlist enforcement

canonical_assets() {
    cat <<'ASSETS'
facelock-x86_64-linux-gnu
pam_facelock.so
facelock-polkit-agent-x86_64-linux-gnu
facelock_0.2.0-1~deb13u1_amd64.deb
facelock_0.2.0-1~ubuntu26.04.1_amd64.deb
facelock-0.2.0-1.fc44.x86_64.rpm
facelock-debuginfo-0.2.0-1.fc44.x86_64.rpm
facelock-debugsource-0.2.0-1.fc44.x86_64.rpm
apt-repo.tar.gz
ASSETS
}

{ canonical_assets; echo MANIFEST.json; } >"$work/actual-ok"
"$helper_path" verify "$stable_expected" "$work/actual-ok" >/dev/null ||
    fail "the canonical asset set was rejected"

{ canonical_assets; echo MANIFEST.json; echo "facelock-x86_64-linux-musl"; } >"$work/actual-extra"
assert_rejects "extra release asset" "unexpected release asset" \
    verify "$stable_expected" "$work/actual-extra"

{ canonical_assets; echo MANIFEST.json; } | grep -Fvx 'pam_facelock.so' >"$work/actual-missing"
assert_rejects "missing release asset" "no release asset matches" \
    verify "$stable_expected" "$work/actual-missing"

{ canonical_assets; echo MANIFEST.json; echo "pam_facelock.so"; } >"$work/actual-duplicate"
assert_rejects "duplicate asset name" "duplicate release asset" \
    verify "$stable_expected" "$work/actual-duplicate"

{ canonical_assets; echo MANIFEST.json; echo "facelock-0.2.0-1.fc45.x86_64.rpm"; } >"$work/actual-collision"
assert_rejects "two assets claiming one canonical name" "more than one release asset" \
    verify "$stable_expected" "$work/actual-collision"

# --- staging out of the builders' artifacts

artifacts="$work/artifacts"
build_artifacts() {
    rm -rf "$artifacts"
    mkdir -p "$artifacts"/{release-binaries,release-deb-trixie,release-deb-resolute,release-rpm,release-apt-repo}
    printf 'facelock\n' >"$artifacts/release-binaries/facelock-x86_64-linux-gnu"
    printf 'pam\n' >"$artifacts/release-binaries/pam_facelock.so"
    printf 'polkit\n' >"$artifacts/release-binaries/facelock-polkit-agent-x86_64-linux-gnu"
    printf 'deb\n' >"$artifacts/release-deb-trixie/facelock_0.2.0-1~deb13u1_amd64.deb"
    # The staged Debian manifest travels with the package and is not an asset.
    printf 'manifest\n' >"$artifacts/release-deb-trixie/facelock_0.2.0-1~deb13u1_amd64.manifest"
    printf 'deb\n' >"$artifacts/release-deb-resolute/facelock_0.2.0-1~ubuntu26.04.1_amd64.deb"
    printf 'rpm\n' >"$artifacts/release-rpm/facelock-0.2.0-1.fc44.x86_64.rpm"
    printf 'rpm\n' >"$artifacts/release-rpm/facelock-debuginfo-0.2.0-1.fc44.x86_64.rpm"
    printf 'rpm\n' >"$artifacts/release-rpm/facelock-debugsource-0.2.0-1.fc44.x86_64.rpm"
    printf 'apt\n' >"$artifacts/release-apt-repo/apt-repo.tar.gz"
    # The real emitter, so the attestation shape is the one the builders write.
    # It records each document's digest as a step output; capture that here
    # rather than in the CI step's own GITHUB_OUTPUT.
    local attest="$repo_root/.github/workflows/scripts/attest-digests.sh"
    local -x GITHUB_OUTPUT="$work/github-output"
    : >"$GITHUB_OUTPUT"
    "$attest" build "$artifacts/release-digests-build" \
        "$artifacts"/release-binaries/* >/dev/null
    "$attest" build-deb "$artifacts/release-digests-deb-trixie" \
        --suite trixie --image 'docker.io/library/debian:13@sha256:34cd' \
        "$artifacts"/release-deb-trixie/*.deb >/dev/null
    "$attest" build-deb "$artifacts/release-digests-deb-resolute" \
        --suite resolute --image 'docker.io/library/ubuntu:26.04@sha256:6df9' \
        "$artifacts"/release-deb-resolute/*.deb >/dev/null
    "$attest" build-rpm "$artifacts/release-digests-rpm" \
        --image 'registry.fedoraproject.org/fedora:44@sha256:fc3e' \
        "$artifacts"/release-rpm/*.rpm >/dev/null
    "$attest" publish-apt "$artifacts/release-digests-apt" \
        "$artifacts/release-apt-repo/apt-repo.tar.gz" >/dev/null
    # The two source components carry provenance and no asset of their own.
    printf '{"component":"onnxruntime","version":"1.20.1","library_sha256":"a5faaf78"}\n' \
        >"$work/ort-manifest.json"
    printf 'ort\n' >"$work/onnxruntime-bundle.tar.xz"
    printf 'vendor\n' >"$work/cargo-vendor-bundle.tar.xz"
    "$attest" download-ort "$artifacts/release-digests-onnxruntime" \
        --component "onnxruntime=$work/ort-manifest.json" \
        --component-archive "onnxruntime=$work/onnxruntime-bundle.tar.xz" >/dev/null
    "$attest" prepare-cargo-vendor "$artifacts/release-digests-cargo-vendor" \
        --component-archive "cargo-vendor=$work/cargo-vendor-bundle.tar.xz" >/dev/null
    diff <(sed -n 's/^sha256=//p' "$GITHUB_OUTPUT" | LC_ALL=C sort) \
        <(for document in "$artifacts"/release-digests-*/digests.json; do
              sha256sum "$document" | cut -d' ' -f1
          done | LC_ALL=C sort) >/dev/null ||
        fail "attest-digests.sh did not record each document's SHA-256 as its step output"
    job_outputs_for "$job_outputs"
}

# What toJSON(needs) hands the publish job: every needed job's result and
# outputs, the attestation output being the SHA-256 of each digests.json as the
# tree stands now. A slot with no artifact is a job that was skipped.
job_outputs="$work/job-outputs.json"
job_outputs_for() {
    "$helper_path" expected-attestations "${2:-false}" >"$work/attesting-spec"
    python3 - "$artifacts" "$1" "$work/attesting-spec" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

artifacts, output, spec = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
needs = {"metadata": {"result": "success", "outputs": {"cargo-version": "0.2.0"}}}
for line in spec.read_text(encoding="utf-8").splitlines():
    slot, job, key = line.split("\t")[:3]
    document = artifacts / f"release-digests-{slot}" / "digests.json"
    entry = needs.setdefault(job, {"result": "success", "outputs": {}})
    if document.is_file():
        entry["outputs"][key] = hashlib.sha256(document.read_bytes()).hexdigest()
    elif not entry["outputs"]:
        entry["result"] = "skipped"
Path(output).write_text(json.dumps(needs, indent=2) + "\n", encoding="utf-8")
PY
}

build_artifacts
staged="$work/assets"
rm -rf "$staged"
"$helper_path" stage "$builders_expected" "$artifacts" "$staged" >"$work/actual-staged"
diff <(canonical_assets | LC_ALL=C sort) <(LC_ALL=C sort "$work/actual-staged") >/dev/null ||
    fail "staging did not collect exactly the canonical assets: $(tr '\n' ' ' <"$work/actual-staged")"
"$helper_path" verify "$builders_expected" "$work/actual-staged" >/dev/null ||
    fail "the staged asset set was rejected by its own allowlist"
[ -f "$staged/apt-repo.tar.gz" ] || fail "staging did not copy the assets it named"
if [ -e "$staged/facelock_0.2.0-1~deb13u1_amd64.manifest" ]; then
    fail "staging copied a file the allowlist does not name"
fi

"$helper_path" verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false >/dev/null ||
    fail "the staged assets did not match the attestations in the same artifact tree"

cat >"$artifacts/release-binaries/digests.json" <<'JSON'
{"job": "download-ort", "image": "evil.example/img@sha256:dead", "assets": {},
 "components": {"onnxruntime": {"version": "666", "library_sha256": "forged"}}}
JSON
assert_rejects "payload artifact claiming another builder's provenance" "inside a payload artifact" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

rm -f "$artifacts/release-rpm/facelock-debugsource-0.2.0-1.fc44.x86_64.rpm"
assert_rejects "builder artifact missing a canonical asset" "no builder artifact provides" \
    stage "$builders_expected" "$artifacts" "$work/assets-missing"

build_artifacts
cp "$artifacts/release-binaries/pam_facelock.so" "$artifacts/release-rpm/pam_facelock.so"
assert_rejects "two builders providing one canonical asset" "more than one builder artifact provides" \
    stage "$builders_expected" "$artifacts" "$work/assets-ambiguous"
build_artifacts

# --- the maintainer tag

fixture_repo="$work/repo"
(
    # Each fixture subshell isolates itself from the host git configuration.
    # shellcheck disable=SC2030,SC2031
    export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
    git init -q -b main "$fixture_repo"
    cd "$fixture_repo"
    git config user.email release@example.invalid
    git config user.name Release
    git config commit.gpgsign false
    git config tag.gpgsign false
    echo release >file
    git add file
    git commit -qm "release"
    git tag v0.2.0
    echo other >file
    git commit -qam "after the tag"
) >/dev/null
tag_commit="$(git -C "$fixture_repo" rev-list -n1 v0.2.0)"
head_commit="$(git -C "$fixture_repo" rev-parse HEAD)"

"$helper_path" verify-tag v0.2.0 0.2.0 "$tag_commit" "$fixture_repo" >/dev/null ||
    fail "the maintainer tag at the built commit was rejected"

assert_rejects "tag that does not name the validated version" "does not match the validated version" \
    verify-tag v0.2.1 0.2.0 "$tag_commit" "$fixture_repo"
assert_rejects "tag that does not exist" "does not exist" \
    verify-tag v9.9.9 9.9.9 "$tag_commit" "$fixture_repo"
assert_rejects "tag that does not point at the built commit" "does not point at the built commit" \
    verify-tag v0.2.0 0.2.0 "$head_commit" "$fixture_repo"

# An annotated tag makes GITHUB_SHA the tag object, not the commit it names.
(
    # shellcheck disable=SC2030,SC2031
    export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
    cd "$fixture_repo"
    git config tag.gpgsign false
    git tag -a -m "annotated release" v0.2.0-beta.1 "$tag_commit"
    printf '%s\n' "signed release" "-----BEGIN PGP SIGNATURE-----" "not a signature" \
        "-----END PGP SIGNATURE-----" | git tag -a -F - v0.2.0-rc.1 "$tag_commit"
) >/dev/null
annotated_object="$(git -C "$fixture_repo" rev-parse v0.2.0-beta.1)"
[ "$annotated_object" != "$tag_commit" ] || fail "the annotated tag fixture is not a tag object"
"$helper_path" verify-tag v0.2.0-beta.1 0.2.0-beta.1 "$annotated_object" "$fixture_repo" >/dev/null ||
    fail "an annotated tag was rejected because GITHUB_SHA names the tag object"

assert_rejects "tag whose signature cannot be verified" "signature verification failed" \
    verify-tag v0.2.0-rc.1 0.2.0-rc.1 "$tag_commit" "$fixture_repo"

# --- the draft is created once and published once

cat >"$work/releases-none.json" <<'JSON'
[]
JSON
"$helper_path" verify-creatable v0.2.0 "$work/releases-none.json" >/dev/null ||
    fail "a tag with no release yet was refused its first draft"

cat >"$work/releases-listing.json" <<'JSON'
[{"id": 7, "tag_name": "v0.1.9", "draft": false, "prerelease": false,
  "assets": [{"name": "facelock-x86_64-linux-gnu"}]},
 {"id": 42, "tag_name": "v0.2.0", "draft": true, "prerelease": false,
  "assets": [{"name": "pam_facelock.so"}, {"name": "MANIFEST.json"}]}]
JSON
"$helper_path" verify-creatable v0.2.0 "$work/releases-listing.json" >/dev/null ||
    fail "the draft an interrupted run left behind was refused"
[ "$("$helper_path" release-id "$work/releases-listing.json" v0.2.0)" = 42 ] ||
    fail "the release id was not read from the API listing"
[ "$("$helper_path" names "$work/releases-listing.json" v0.2.0 | tr '\n' ' ')" = "pam_facelock.so MANIFEST.json " ] ||
    fail "the draft asset names were not read from the API listing"
"$helper_path" verify-draft v0.2.0 false "$work/releases-listing.json" >/dev/null ||
    fail "the draft for this tag was not selected out of the API listing"
assert_rejects "tag with no release of its own" "expected exactly one release" \
    verify-draft v0.3.0 false "$work/releases-listing.json"

# gh emits a paginated stream and slurped pages as well as one array.
printf '%s\n' \
    '{"id": 7, "tag_name": "v0.1.9", "draft": false, "prerelease": false}' \
    '{"id": 42, "tag_name": "v0.2.0", "draft": true, "prerelease": false, "assets": []}' \
    >"$work/releases-stream.json"
[ "$("$helper_path" release-id "$work/releases-stream.json" v0.2.0)" = 42 ] ||
    fail "a paginated release stream was not read"
cat >"$work/releases-slurped.json" <<'JSON'
[[{"id": 7, "tag_name": "v0.1.9", "draft": false}],
 [{"id": 42, "tag_name": "v0.2.0", "draft": true, "prerelease": false, "assets": []}]]
JSON
[ "$("$helper_path" release-id "$work/releases-slurped.json" v0.2.0)" = 42 ] ||
    fail "slurped release pages were not read"

cat >"$work/release-published.json" <<'JSON'
{"id": 42, "tag_name": "v0.2.0", "draft": false, "prerelease": false}
JSON
assert_rejects "publish-only rerun against a published release" "already published" \
    verify-draft v0.2.0 false "$work/release-published.json"
assert_rejects "second run against an already published tag" "already published" \
    verify-creatable v0.2.0 "$work/release-published.json"

cat >"$work/release-other-tag.json" <<'JSON'
{"id": 42, "tag_name": "v0.1.9", "draft": true, "prerelease": false}
JSON
assert_rejects "draft belonging to another tag" "another tag" \
    verify-draft v0.2.0 false "$work/release-other-tag.json"

cat >"$work/release-wrong-channel.json" <<'JSON'
{"id": 42, "tag_name": "v0.2.0", "draft": true, "prerelease": true}
JSON
assert_rejects "draft whose channel differs from the validated identity" "prerelease identity" \
    verify-draft v0.2.0 false "$work/release-wrong-channel.json"

# A run interrupted between the manifest upload and the flip must be able to
# run again: the draft already carries MANIFEST.json, and the final allowlist
# expects exactly that.
cat >"$work/release-rerun.json" <<'JSON'
{"id": 42, "tag_name": "v0.2.0", "draft": true, "prerelease": false, "assets": [
  {"name": "facelock-x86_64-linux-gnu"}, {"name": "pam_facelock.so"},
  {"name": "facelock-polkit-agent-x86_64-linux-gnu"},
  {"name": "facelock_0.2.0-1~deb13u1_amd64.deb"},
  {"name": "facelock_0.2.0-1~ubuntu26.04.1_amd64.deb"},
  {"name": "facelock-0.2.0-1.fc44.x86_64.rpm"},
  {"name": "facelock-debuginfo-0.2.0-1.fc44.x86_64.rpm"},
  {"name": "facelock-debugsource-0.2.0-1.fc44.x86_64.rpm"},
  {"name": "apt-repo.tar.gz"}, {"name": "MANIFEST.json"}]}
JSON
"$helper_path" verify-creatable v0.2.0 "$work/release-rerun.json" >/dev/null ||
    fail "a rerun over the draft of an interrupted run was refused"
"$helper_path" names "$work/release-rerun.json" v0.2.0 >"$work/actual-rerun"
"$helper_path" verify "$stable_expected" "$work/actual-rerun" >/dev/null ||
    fail "a rerun whose draft already carries MANIFEST.json was rejected"
echo "release artifacts case: rerun over a draft that already carries the manifest accepted"

# --- builder digest attestations

# One tree, the one the publish job hands to both readers: payload artifacts
# beside the attestations that cover them.
attested_digest() { sha256sum "$staged/$1" | cut -d' ' -f1; }
attestation() { printf '%s' "$artifacts/release-digests-$1/digests.json"; }

"$helper_path" verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false >/dev/null ||
    fail "assets matching their builder attestations were rejected"

# --- every attestation is bound to the output its job recorded

# The artifact store is shared and writable by every job in the run: a later
# builder can replace an earlier builder's payload and its attestation with a
# matching pair. The job output is the one record another job cannot rewrite,
# so an attestation whose bytes are not the ones its job recorded is refused
# before anything in it is read.
python3 - "$(attestation build)" <<'PY'
import json, sys
path = sys.argv[1]
document = json.load(open(path))
open(path, "w").write(json.dumps(document, indent=4) + "\n")
PY
assert_rejects "attestation replaced after its job recorded it" "is not the document build recorded" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
assert_rejects "replaced attestation reaching the manifest" "is not the document build recorded" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
build_artifacts

unbind() {
    python3 - "$job_outputs" "$work/job-outputs-$1.json" "$1" "$2" <<'PY'
import json, sys
source, target, job, value = sys.argv[1:5]
needs = json.load(open(source))
if value == "-":
    del needs[job]["outputs"]
else:
    needs[job]["outputs"]["attestation"] = value
open(target, "w").write(json.dumps(needs) + "\n")
PY
    printf '%s' "$work/job-outputs-$1.json"
}
assert_rejects "attesting job that recorded no output" "not bound to a job output" \
    verify-digests "$artifacts" "$(unbind build -)" "$staged" "$work/actual-staged" false
assert_rejects "job output that is not a digest" "not bound to a job output" \
    verify-digests "$artifacts" "$(unbind build-rpm abc)" "$staged" "$work/actual-staged" false
printf '[]\n' >"$work/job-outputs-list.json"
assert_rejects "job outputs that are not an object" "is not a JSON object" \
    verify-digests "$artifacts" "$work/job-outputs-list.json" "$staged" "$work/actual-staged" false
printf '{"build": \n' >"$work/job-outputs-broken.json"
assert_rejects "job outputs that are not JSON" "is not valid JSON" \
    verify-digests "$artifacts" "$work/job-outputs-broken.json" "$staged" "$work/actual-staged" false

# A prerelease skips publish-apt, so its slot has no artifact and no output.
rm -rf "$artifacts/release-digests-apt" "$artifacts/release-apt-repo"
job_outputs_for "$work/job-outputs-prerelease.json" true
"$helper_path" expected 0.2.0 1 1 true builders >"$work/expected-prerelease-builders"
rm -rf "$work/assets-prerelease"
"$helper_path" stage "$work/expected-prerelease-builders" "$artifacts" "$work/assets-prerelease" \
    >"$work/actual-prerelease"
"$helper_path" verify-digests "$artifacts" "$work/job-outputs-prerelease.json" \
    "$work/assets-prerelease" "$work/actual-prerelease" true >/dev/null ||
    fail "a prerelease with the APT publisher skipped was rejected"
echo "release artifacts case: prerelease without the skipped APT publisher accepted"
build_artifacts

# A document that is not a JSON object is an attestation error, not a crash.
printf '{\n' >"$(attestation rpm)"
job_outputs_for "$job_outputs"
assert_rejects "attestation that is not JSON" "is not valid JSON" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
assert_rejects "unparseable attestation reaching the manifest" "is not valid JSON" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
printf '[]\n' >"$(attestation rpm)"
job_outputs_for "$job_outputs"
assert_rejects "attestation that is not a JSON object" "is not a JSON object" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

printf 'tampered\n' >"$staged/pam_facelock.so"
assert_rejects "release asset mutated after its builder attested it" "does not match the digest" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
cp "$artifacts/release-binaries/pam_facelock.so" "$staged/pam_facelock.so"

printf 'smuggled\n' >"$staged/extra-asset"
{ cat "$work/actual-staged"; echo extra-asset; } >"$work/actual-unattested"
assert_rejects "release asset no builder attested" "attested by no builder" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-unattested" false
rm -f "$staged/extra-asset"

# An attesting artifact may not claim an asset another one produced.
python3 - "$(attestation rpm)" "$(attested_digest pam_facelock.so)" <<'PY'
import json, sys
path = sys.argv[1]
document = json.load(open(path))
document["assets"]["pam_facelock.so"] = sys.argv[2]
open(path, "w").write(json.dumps(document, indent=2) + "\n")
PY
job_outputs_for "$job_outputs"
assert_rejects "two builders claiming one asset" "attested by more than one builder" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

# --- the attesting set is pinned, not merely deduplicated

# Anything running in a builder can upload an artifact of its own. An extra
# attestation claiming another job's identity would reach MANIFEST.json with
# every asset digest still checking out.
mkdir -p "$artifacts/release-digests-zzz-supplychain"
cat >"$artifacts/release-digests-zzz-supplychain/digests.json" <<'JSON'
{"job": "download-ort", "image": "evil.example/img@sha256:dead", "assets": {},
 "components": {"onnxruntime": {"version": "666", "library_sha256": "forged"}}}
JSON
assert_rejects "extra digest artifact forging another job's provenance" "no builder attests as" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
assert_rejects "extra digest artifact reaching the manifest" "no builder attests as" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
build_artifacts

python3 - "$(attestation build)" <<'PY'
import json, sys
path = sys.argv[1]
document = json.load(open(path))
document["job"] = "build-deb"
open(path, "w").write(json.dumps(document, indent=2) + "\n")
PY
job_outputs_for "$job_outputs"
assert_rejects "attestation declaring a job that is not its slot" "belongs to" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

rm -rf "$artifacts/release-digests-apt"
assert_rejects "attesting job that did not attest" "no attestation from" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

mkdir -p "$artifacts/release-digests-rpm/second"
cp "$(attestation rpm)" "$artifacts/release-digests-rpm/second/digests.json"
assert_rejects "attesting artifact holding two documents" "documents, expected one" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

# A prerelease publishes no APT repository, so publish-apt attests nothing.
assert_rejects "stable attestation set on a prerelease" "no builder attests as" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" true

# A payload artifact carrying an attestation is a builder claiming provenance
# for work it did not do; the manifest would record its image and components.
cat >"$artifacts/release-binaries/digests.json" <<'JSON'
{"job": "download-ort", "image": "evil.example/img@sha256:dead", "assets": {},
 "components": {"onnxruntime": {"version": "666", "library_sha256": "forged"}}}
JSON
assert_rejects "digest attestation planted in a payload artifact" "inside a payload artifact" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
assert_rejects "forged provenance reaching the manifest" "inside a payload artifact" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
build_artifacts

# Two attestations claiming one provenance key is a contradiction, not a merge.
python3 - "$(attestation cargo-vendor)" <<'PY'
import json, sys
path = sys.argv[1]
document = json.load(open(path))
document.setdefault("components", {})["onnxruntime"] = {"version": "666"}
open(path, "w").write(json.dumps(document, indent=2) + "\n")
PY
job_outputs_for "$job_outputs"
assert_rejects "two attestations claiming one component" "claim the component" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
build_artifacts

# --- the publication manifest

manifest="$work/MANIFEST.json"
"$helper_path" manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$manifest" >/dev/null ||
    fail "manifest generation failed for a complete release"

python3 - "$manifest" "$(attested_digest facelock-x86_64-linux-gnu)" <<'PY' ||
import json, sys
manifest = json.load(open(sys.argv[1]))
assets = {entry["name"]: entry for entry in manifest["assets"]}
missing = {"facelock-x86_64-linux-gnu", "pam_facelock.so", "apt-repo.tar.gz"} - set(assets)
assert not missing, f"manifest omits {sorted(missing)}"
assert assets["facelock-x86_64-linux-gnu"]["sha256"] == sys.argv[2], "manifest digest is not the asset digest"
assert all("size" in entry for entry in manifest["assets"]), "manifest omits asset sizes"
assert manifest["tag"] == "v0.2.0" and manifest["version"] == "0.2.0", "manifest identity drifted"
assert manifest["source"]["sha256"] == "deadbeef", "manifest omits the source digest"
assert "archive/refs/tags/v0.2.0" in manifest["source"]["url"], "manifest omits the source archive"
images = manifest["build_images"]
assert any("debian:13@sha256" in value for value in images.values()), "manifest omits a build image digest"
assert manifest["components"]["onnxruntime"]["library_sha256"] == "a5faaf78", "manifest omits the ORT digest"
assert "cargo-vendor" in manifest["components"], "manifest omits the Cargo vendor component"
PY
    fail "the generated manifest does not cover every asset and every reviewed digest"

rm -f "$staged/pam_facelock.so"
assert_rejects "manifest over an asset that was never staged" "is not present" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$manifest"
cp "$artifacts/release-binaries/pam_facelock.so" "$staged/pam_facelock.so"
# ------------------------------------------------------------------ mutations

if [ -z "${FACELOCK_RELEASE_WORKFLOW:-}" ] && [ -z "${FACELOCK_RELEASE_ASSETS:-}" ] &&
    [ -z "${FACELOCK_RELEASE_ATTESTATIONS:-}" ]; then
    mutation_root="$work/mutations"
    mkdir -p "$mutation_root"

    # The attesting set is the only thing standing between an extra artifact
    # and forged provenance in MANIFEST.json, so prove the check is load-bearing.
    assert_loader_mutation_rejected() {
        local context="$1" expression="$2" needle="$3"
        local loader=.github/workflows/scripts/release_attestations.py
        local mutant
        mutant="$mutation_root/release_attestations-$(printf '%s' "$context" | tr ' ' '-').py"
        sed -E "$expression" "$loader" >"$mutant"
        if cmp -s "$loader" "$mutant"; then
            fail "$context mutation did not change the attestation loader"
        fi
        local output
        if output="$(FACELOCK_RELEASE_ATTESTATIONS="$mutant" bash "$0" 2>&1)"; then
            fail "release artifacts contract accepted $context"
        fi
        printf '%s\n' "$output" | grep -Fq "$needle" ||
            fail "$context mutation failed for an unrelated reason: $output"
        echo "release artifacts mutation: $context rejected"
    }

    assert_loader_mutation_rejected "an unpinned attesting set" \
        's/^    unexpected = sorted\(set\(present\) - set\(expected\)\)$/    unexpected = []/' \
        "extra digest artifact forging another job's provenance: helper accepted"
    assert_loader_mutation_rejected "attestations trusted to name their own job" \
        's/^        if document.get\("job"\) != job:$/        if False:/' \
        "attestation declaring a job that is not its slot: helper accepted"
    assert_loader_mutation_rejected "attestations not held to their job outputs" \
        's/^        if actual != recorded:$/        if False:/' \
        "attestation replaced after its job recorded it: helper accepted"
    assert_loader_mutation_rejected "a missing job output tolerated" \
        's/^        if not isinstance\(recorded, str\) or not DIGEST.fullmatch\(recorded\):$/        if False:/' \
        "attesting job that recorded no output"

    assert_workflow_mutation_rejected() {
        local context="$1" expression="$2" needle="$3"
        local mutated
        mutated="$mutation_root/release-$(printf '%s' "$context" | tr ' ' '-').yml"
        sed -E "$expression" "$workflow_path" >"$mutated"
        if cmp -s "$workflow_path" "$mutated"; then
            fail "$context mutation did not change the workflow"
        fi
        local output
        if output="$(FACELOCK_RELEASE_WORKFLOW="$mutated" bash "$0" 2>&1)"; then
            fail "release artifacts contract accepted $context"
        fi
        printf '%s\n' "$output" | grep -Fq "$needle" ||
            fail "$context mutation failed for an unrelated reason: $output"
        echo "release artifacts mutation: $context rejected"
    }

    assert_workflow_mutation_rejected "public release creation" \
        's/^          draft: true$//' \
        "must create the GitHub release as a draft"
    assert_workflow_mutation_rejected "builder holding the release write scope" \
        '0,/^      contents: read$/s//      contents: write/' \
        "only the publish job may hold contents: write"
    # The mutation expressions below name literal workflow text, not shell
    # expansions.
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "builder holding the publication credential" \
        's/^      - name: Build release$/      - name: Build release\n        env:\n          TOKEN: ${{ secrets.RELEASE_PAT }}/' \
        "builders produce artifacts only"
    assert_workflow_mutation_rejected "builder writing the release directly" \
        's|^      - name: Upload the APT repository artifact$|      - name: Write the release\n        uses: softprops/action-gh-release@0000\n\n      - name: Upload the APT repository artifact|' \
        "builders produce artifacts only"
    assert_workflow_mutation_rejected "compiling in the publishing job" \
        's|^      - name: Verify the maintainer tag$|      - name: Rebuild\n        run: cargo build --release\n\n      - name: Verify the maintainer tag|' \
        "must not compile or package anything"
    # Neutralized, not deleted: the step stays in place as a no-op with its
    # old text in a comment, which is what a careless edit leaves behind.
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "unverified tag" \
        's|^( *)\$HELPER verify-tag .*|\1: # verify-tag disabled|' \
        'must run $HELPER verify-tag'
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "unverified builder attestations" \
        's|^( *)run: \$HELPER verify-digests .*|\1run: ": # verify-digests disabled"|' \
        'must run $HELPER verify-digests'
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "no revalidation before the flip" \
        's|^( *)\$HELPER expected "\$VERSION" "\$DEBIAN_REVISION" "\$RPM_COUNTER" "\$PRERELEASE" final .*|\1: # final readback disabled|' \
        'must run PRERELEASE" final'
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "attestation left unbound to its job" \
        '0,/^      attestation: \$\{\{ steps.attest.outputs.sha256 \}\}$/s///' \
        "declares no attestation output"
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "one suite of the matrix left unbound" \
        '/^      attestation-resolute: /d' \
        "differs from the release workflow's attestation outputs"
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "job outputs not passed through env" \
        's/^          JOB_OUTPUTS: \$\{\{ toJSON\(needs\) \}\}$/          JOB_OUTPUTS: fixed/' \
        "through exactly one env value"
    assert_workflow_mutation_rejected "pages rebuild before publication" \
        's/^    needs: \[publish-apt, publish\]$/    needs: [publish-apt]/' \
        "trigger-pages must run after the release is published"
    assert_workflow_mutation_rejected "release upload without the publication token" \
        '0,/^          token: \$\{\{ secrets.RELEASE_PAT \}\}$/s///' \
        "every release upload must pass the publication token"
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "tag rewritten at publication" \
        's/^      TAG: \$\{\{ github.ref_name \}\}$/      TAG: ${{ github.ref_name }}\n      TAG_TARGET: target_commitish/' \
        "publishing must not send a tag or target commitish"
fi

# The helper's Python runs from whatever directory the workflow is in; a module
# planted there must not shadow the standard library.
case "$helper_path" in
    /*) helper_abs="$helper_path" ;;
    *) helper_abs="$repo_root/$helper_path" ;;
esac
mkdir -p "$work/shadow"
printf 'raise SystemExit("json shadowed from the working directory")\n' >"$work/shadow/json.py"
[ "$(cd "$work/shadow" && "$helper_abs" release-id "$work/releases-listing.json" v0.2.0)" = 42 ] ||
    fail "the release asset helper imported a module from its working directory"
(cd "$work/shadow" && "$helper_abs" expected 0.2.0 1 1 false final >/dev/null) ||
    fail "the release asset helper imported a module from its working directory"
echo "release artifacts case: working-directory module shadowing ignored"

# Loading the attestation module must not leave bytecode in the checkout: the
# recipes that demand a clean tree run right after this gate.
if [ -e .github/workflows/scripts/__pycache__ ]; then
    fail "the release asset helper left __pycache__ in the checkout"
fi

echo "release artifacts contract: ok"
