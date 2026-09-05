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
# Resolved before the cd below: the mutation re-invocations pass this to bash
# explicitly, so they still find this script regardless of the directory (the
# repo root, or test/) the outer run started from.
self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
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
expected_jobs=("${builders[@]}" publish publish-aur verify-copr trigger-pages)

actual_jobs="$(job_names | LC_ALL=C sort)"
wanted_jobs="$(printf '%s\n' "${expected_jobs[@]}" | LC_ALL=C sort)"
[ "$actual_jobs" = "$wanted_jobs" ] ||
    fail "release jobs drifted; every job needs a permission ceiling and a place in the publish graph: $(echo "$actual_jobs" | tr '\n' ' ')"

# A workflow-level write grant reaches every builder. The floor is deny-all and
# each job asks for exactly what it needs.
if awk '/^permissions:/ { inside = 1; next } inside && /^[A-Za-z]/ { inside = 0 } inside && !/^[[:space:]]*(#|$)/ { print }' \
    "$workflow_path" | grep -q .; then
    fail "workflow-level permissions must be the deny-all floor, not a scope list"
fi
grep -Eq '^permissions:[[:space:]]*\{\}[[:space:]]*$' "$workflow_path" ||
    fail "workflow must declare the deny-all permission floor: permissions: {}"

# "Exactly once" needs one run at a time per tag: two runs inside publish
# both pass verify-creatable, both create or upload, and the survivor can
# carry one run's binaries under the other's digests. A queued run is fine; a
# cancelled one is not, because the first run may be mid-publication.
concurrency_block="$(awk '/^concurrency:/ { inside = 1; next } inside && /^[A-Za-z]/ { inside = 0 } inside && !/^[[:space:]]*(#|$)/ { sub(/^ +/, ""); print }' "$workflow_path")"
[ -n "$concurrency_block" ] || fail "the release workflow must declare a concurrency group per tag"
# shellcheck disable=SC2016
printf '%s\n' "$concurrency_block" | grep -Fxq 'group: release-${{ github.ref }}' ||
    fail "the release concurrency group must be keyed by the tag ref: $concurrency_block"
printf '%s\n' "$concurrency_block" | grep -Fxq 'cancel-in-progress: false' ||
    fail "a release run must queue behind the one in progress, never cancel it: $concurrency_block"

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
    'gh release upload "$TAG" MANIFEST.json' \
    '$HELPER verify-published releases.json "$TAG" MANIFEST.json' 'draft=false'; do
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

# Packit reacts to the published release event, so the COPR build it submits
# cannot even start before publish. A verifier that ran earlier would be
# checking the previous release (#333).
verify_copr_job="$(job_body verify-copr)"
requires_job verify-copr publish ||
    fail "verify-copr waits on a build Packit submits after publication and must run after publish"
printf '%s\n' "$verify_copr_job" | grep -Fq "needs.metadata.outputs.prerelease == 'false'" ||
    fail "verify-copr must be stable-only; a prerelease never reaches production COPR"
printf '%s\n' "$verify_copr_job" | grep -Eq '^ +timeout-minutes: [0-9]+$' ||
    fail "verify-copr polls and must carry its own timeout"
grep -Fq 'check-live-release-channels.py' .github/workflows/scripts/verify-copr.sh ||
    fail "verify-copr must compare the served EVR with the live release-channel checker"

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

# Preflight must not pass on a check it could not make: without gh, or
# unauthenticated, the RELEASE_PAT check fails closed rather than being skipped.
grep -Fq 'UNCHECKED: RELEASE_PAT' justfile ||
    fail "release-preflight must fail closed when it cannot check RELEASE_PAT"

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

# build-nix gates publication through its evaluation, which is deterministic
# against flake.lock; the build itself depends on nixpkgs state and stays
# advisory. A gate that cannot fail is not a gate.
nix_step() {
    job_body build-nix | awk -v needle="$1" '
        function flush() { if (index(step, needle)) printf "%s", step; step = "" }
        /^      - / { flush() }
        { step = step $0 "\n" }
        END { flush() }
    '
}
flake_check_step="$(nix_step 'nix flake check')"
[ -n "$flake_check_step" ] || fail "build-nix must evaluate the flake with nix flake check"
if printf '%s\n' "$flake_check_step" | grep -Eq '^ +continue-on-error:'; then
    fail "the flake evaluation must gate publication; drop continue-on-error from nix flake check"
fi
nix_build_step="$(nix_step 'nix build')"
[ -n "$nix_build_step" ] || fail "build-nix must attempt nix build"
printf '%s\n' "$nix_build_step" | grep -Eq '^ +continue-on-error: true$' ||
    fail "nix build depends on nixpkgs state and is documented as advisory; it must keep continue-on-error"

# --------------------------------------- container jobs and checkout ownership
#
# A container job runs every step as root while the workspace keeps the runner
# uid, so git rejects the checkout ("detected dubious ownership") and exits 128.
# actions/checkout writes the exception itself, but under a temporary HOME it
# discards when it finishes, so no later step sees it. This workflow runs only
# on a tag, so no pull request exercises it: v0.2.0-alpha.1 (run 33983928629)
# was the first run of both Debian legs and both died here before building a
# deb.
#
# The trigger is git being installed, not any one recipe. A container job that
# installs git gets a real git checkout, and anything it runs afterwards can
# shell out to git -- `just test-deb-source-contract` reaches
# test/prepare-deb-test-context.sh, and build-deb.sh takes its source tarball
# from `git archive`. Proving the negative, that nothing in a job's transitive
# script closure ever calls git, is the analysis that fails open, so the rule is
# the coarse one. A container job that installs no git carries no requirement:
# build-rpm installs none, actions/checkout falls back to the API tarball there
# and leaves no repository behind. It acquires the requirement the day it
# installs git.
#
# ci.yml already takes the exception in the two jobs that need it, and every
# pull request runs ci.yml, so a regression there is visible immediately. This
# gate covers release.yml because a tag is the only thing that runs it.
trust_step_name='Trust the workspace checkout'
# shellcheck disable=SC2016
trust_step_run='run: git config --global --add safe.directory "$GITHUB_WORKSPACE"'

# One job's body with whole-line comments removed: a commented-out package or
# step must never pass as present. `grep -v` finds no match on an empty body,
# which is a failure under `set -e`, so an empty result is allowed through and
# the caller's own assertion reports it.
job_statements() {
    job_body "$1" | grep -v '^[[:space:]]*#' || true
}

# The first line of each step of one job, in order, as `keyword<TAB>value`.
job_step_heads() {
    awk '
        /^      - / {
            line = $0
            sub(/^      - /, "", line)
            split(line, parts, ": ")
            print parts[1] "\t" substr(line, length(parts[1]) + 3)
        }
    ' <<<"$1"
}

# Whether one job installs git: a package-manager install naming git as a
# package. Line continuations are joined first, so a package list spread over a
# dozen lines reads as the one command it is.
installs_git() {
    local joined
    joined="$(sed -e ':a' -e '/\\$/N; s/\\\n//; ta' <<<"$1")"
    grep -Eq '(^|[[:space:]])(install|-S[a-z]*)[[:space:]].*[[:space:]]git([[:space:]]|$)' <<<"$joined"
}

trusting_jobs=0
for job in "${expected_jobs[@]}"; do
    statements="$(job_statements "$job")"
    grep -Eq '^    container:' <<<"$statements" || continue
    steps="$(job_step_heads "$statements")"
    if ! installs_git "$statements"; then
        # Fail closed in the other direction too: a job holding an exception it
        # does not need has either lost its git install or gained a stray step.
        if grep -Fq "name	$trust_step_name" <<<"$steps"; then
            fail "job $job trusts the workspace checkout but installs no git; drop the step or restore the install"
        fi
        continue
    fi
    trusting_jobs=$((trusting_jobs + 1))
    grep -Fq "name	$trust_step_name" <<<"$steps" ||
        fail "container job $job installs git and must trust the workspace checkout; git exits 128 on a checkout it does not own"
    grep -Fq "$trust_step_run" <<<"$statements" ||
        fail "job $job must trust the workspace checkout with exactly: $trust_step_run"
    # Immediately after the checkout: nothing between them may reach git, and
    # the entry actions/checkout makes for itself is gone by the time it
    # returns.
    after_checkout="$(awk -F'\t' '
        seen { print $2; exit }
        $1 == "uses" && $2 ~ /^actions\/checkout@/ { seen = 1 }
    ' <<<"$steps")"
    [ -n "$after_checkout" ] ||
        fail "job $job has no step after its checkout; the ownership exception has nowhere to go"
    [ "$after_checkout" = "$trust_step_name" ] ||
        fail "job $job must trust the workspace checkout immediately after checking it out, not after '$after_checkout'"
done
# A renamed `container:` key, or a job list that stopped containing one, would
# turn every assertion above into a no-op.
[ "$trusting_jobs" -gt 0 ] ||
    fail "no containerized release job installs git; the checkout-ownership rule stopped covering anything"

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

# The image one matrix leg builds in, from the job's `include:` list.
matrix_image() {
    job_body "$1" | awk -v suite="$2" '
        /^          - suite: / { current = $3 }
        current == suite && /^            image: / { sub(/^            image: /, ""); print; exit }
    '
}

# Each slot's expected provenance is what its attest-digests.sh call site
# declares: the suite, the image, and the component names. The helper pins the
# same values from dist/release-matrix.json, so a slot cannot report an image
# or a component the release did not build with.
workflow_attestations() {
    local wf_job body step call attest_job name_pattern outputs line key expression suite slot
    local suite_arg image_arg components image
    for wf_job in build download-ort prepare-cargo-vendor build-deb build-rpm publish-apt; do
        body="$(job_body "$wf_job")"
        step="$(attest_step "$wf_job")"
        [ -n "$step" ] || fail "job $wf_job runs no attest-digests.sh step"
        printf '%s\n' "$step" | grep -Eq '^        id: attest$' ||
            fail "job $wf_job must give its attestation step the id attest so its digest can be a job output"
        # The call with its line continuations joined.
        call="$(printf '%s\n' "$step" | sed -e ':a' -e '/\\$/N; s/\\\n//; ta' | grep -F 'attest-digests.sh')"
        attest_job="$(printf '%s\n' "$call" |
            sed -n 's|.*attest-digests\.sh \([a-z-]*\) .*|\1|p' | head -1)"
        name_pattern="$(printf '%s\n' "$body" |
            sed -n 's/^ *name: release-digests-\(.*\)$/\1/p' | head -1)"
        [ -n "$attest_job" ] || fail "job $wf_job uploads no digest attestation"
        [ -n "$name_pattern" ] || fail "job $wf_job names no digest artifact"
        suite_arg="$(printf '%s\n' "$call" | sed -n "s/.*--suite ['\"]\([^'\"]*\)['\"].*/\1/p")"
        image_arg="$(printf '%s\n' "$call" | sed -n "s/.*--image ['\"]\([^'\"]*\)['\"].*/\1/p")"
        components="$(printf '%s\n' "$call" | grep -oE -- '--component(-archive)? [a-z-]+=' |
            sed 's/.* //; s/=$//' | LC_ALL=C sort -u | paste -sd, -)"
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
                    [ -z "$suite_arg" ] || fail "job $wf_job attests a suite but is not a matrix job"
                    image="$image_arg"
                    if [ "$image" = '$BUILD_IMAGE' ]; then
                        image="$(printf '%s\n' "$body" | awk '/^      BUILD_IMAGE: /{ sub(/^      BUILD_IMAGE: /, ""); print; exit }')"
                    fi
                    printf '%s\t%s\t%s\t-\t%s\t%s\n' "$name_pattern" "$attest_job" "$key" \
                        "${image:--}" "${components:--}"
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
                    [ "$suite_arg" = '${{ matrix.suite }}' ] ||
                        fail "job $wf_job must attest the matrix suite, not: $suite_arg"
                    [ "$image_arg" = '${{ matrix.image }}' ] ||
                        fail "job $wf_job must attest the matrix image, not: $image_arg"
                    image="$(matrix_image "$wf_job" "$suite")"
                    [ -n "$image" ] || fail "job $wf_job pins no image for the $suite leg"
                    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$slot" "$attest_job" "$key" "$suite" \
                        "$image" "${components:--}"
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

# --- the AUR publisher's binary digests

# The AUR recipe pins the published binaries by the digests MANIFEST.json
# carries; a value that is not a SHA-256 must never reach a published PKGBUILD.
# The publisher validates them before it touches SSH or the network, so this
# runs hermetically with a manifest of its own.
aur_publisher=.github/workflows/scripts/publish-aur.sh
aur_home="$work/aur-home"
mkdir -p "$aur_home"
python3 - "$work/aur-manifest-bad.json" <<'PY'
import json, sys
json.dump({"assets": [
    {"name": "facelock-x86_64-linux-gnu", "sha256": "not-a-digest"},
    {"name": "pam_facelock.so", "sha256": "a" * 64},
    {"name": "facelock-polkit-agent-x86_64-linux-gnu", "sha256": "b" * 64},
]}, open(sys.argv[1], "w"))
PY
if aur_output="$(HOME="$aur_home" AUR_SSH_KEY=fixture FACELOCK_RELEASE_MANIFEST_FILE="$work/aur-manifest-bad.json" \
    bash "$aur_publisher" 0.2.0 "$(printf 'c%.0s' $(seq 64))" 2>&1)"; then
    fail "the AUR publisher accepted a binary digest that is not a SHA-256"
fi
printf '%s\n' "$aur_output" | grep -Fq 'not a plausible SHA-256' ||
    fail "the AUR publisher refused the bad digest for an unrelated reason: $aur_output"
[ ! -e "$aur_home/.ssh" ] || fail "the AUR publisher touched SSH before validating the manifest digests"
echo "release artifacts case: AUR publisher refusing a malformed binary digest rejected"

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

# --- the release matrix is an input the allowlist cannot do without

# A matrix that cannot be read must stop the allowlist, not shorten it: with
# the Debian entries silently dropped, every other check would still pass.
printf '{"apt_suites": \n' >"$work/matrix-broken.json"
python3 - "$work/matrix-compat-only.json" <<'PY'
import json, sys
json.dump({"apt_suites": {"compat": {"main": {"source": "trixie"}}}, "platforms": []}, open(sys.argv[1], "w"))
PY
FACELOCK_RELEASE_MATRIX="$work/matrix-broken.json" assert_rejects "allowlist from an unreadable matrix" \
    "cannot read the Debian suites" expected 0.2.0 1 1 false final
FACELOCK_RELEASE_MATRIX="$work/matrix-compat-only.json" assert_rejects "allowlist from a matrix with no suite" \
    "names no Debian suite" expected 0.2.0 1 1 false final
FACELOCK_RELEASE_MATRIX="$work/matrix-broken.json" assert_rejects "attesting set from an unreadable matrix" \
    "cannot read the Debian suites" expected-attestations false
FACELOCK_RELEASE_MATRIX="$work/matrix-compat-only.json" assert_rejects "attesting set from a matrix with no suite" \
    "names no Debian suite" expected-attestations false

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
# The image the release pins for one slot, so the fixture attests what the
# loader now requires.
"$helper_path" expected-attestations false >"$work/attesting-spec-stable"
image_of() {
    awk -F'\t' -v slot="$1" '$1 == slot { print $5 }' "$work/attesting-spec-stable"
}
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
        --suite trixie --image "$(image_of deb-trixie)" \
        "$artifacts"/release-deb-trixie/*.deb >/dev/null
    "$attest" build-deb "$artifacts/release-digests-deb-resolute" \
        --suite resolute --image "$(image_of deb-resolute)" \
        "$artifacts"/release-deb-resolute/*.deb >/dev/null
    "$attest" build-rpm "$artifacts/release-digests-rpm" \
        --image "$(image_of rpm)" \
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
# A builder's extra output under a canonical name fails closed, and the
# failure says what to do: re-running only the failed job keeps the artifact.
assert_rejects "two builders providing one canonical asset" "fix the builder and re-run all jobs" \
    stage "$builders_expected" "$artifacts" "$work/assets-ambiguous"
build_artifacts

# A symlink lets a builder point a canonical name at bytes it does not own,
# including bytes outside the artifact tree entirely; staging must refuse it
# rather than follow it and copy the link target.
printf 'forged\n' >"$work/forged-pam-facelock.so"
rm -f "$artifacts/release-binaries/pam_facelock.so"
ln -s "$work/forged-pam-facelock.so" "$artifacts/release-binaries/pam_facelock.so"
assert_rejects "symlink standing in for a canonical asset" "refusing to stage a symlink" \
    stage "$builders_expected" "$artifacts" "$work/assets-symlink"
build_artifacts

# A symlink to a directory is still a symlink; the walk must refuse it on
# sight rather than recurse through it looking for more files to stage.
ln -s "$artifacts/release-rpm" "$artifacts/release-binaries/sneaky-dir"
assert_rejects "symlink to a directory under artifacts" "refusing to stage a symlink" \
    stage "$builders_expected" "$artifacts" "$work/assets-symlink-dir"
build_artifacts

# A whole payload artifact directory replaced by a symlink is still caught by
# the same walk: every entry it yields is checked with is_symlink() before
# is_file(), so the directory itself is refused before its target is ever
# read as though it were a builder's own output.
rm -rf "$artifacts/release-binaries"
ln -s "$artifacts/release-rpm" "$artifacts/release-binaries"
assert_rejects "symlinked payload artifact directory" "refusing to stage a symlink" \
    stage "$builders_expected" "$artifacts" "$work/assets-symlink-payload-dir"
build_artifacts

# A `release-digests-*` slot itself can be a symlink, pointed at a directory
# outside the downloaded attestation tree entirely. Path.is_dir() and
# Path.rglob() both follow it, so the slot must be refused as a symlink
# before either ever runs, not merely once something under it is inspected.
rm -rf "$artifacts/release-digests-rpm"
ln -s "$artifacts/release-rpm" "$artifacts/release-digests-rpm"
assert_rejects "symlinked attestation slot directory" "is a symlink" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
assert_rejects "symlinked attestation slot directory reaching the manifest" "is a symlink" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
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

# Two drafts for one tag is what two concurrent runs leave behind. The
# concurrency group prevents it; the refusal names the remedy anyway.
cat >"$work/releases-two-drafts.json" <<'JSON'
[{"id": 42, "tag_name": "v0.2.0", "draft": true, "prerelease": false, "assets": []},
 {"id": 43, "tag_name": "v0.2.0", "draft": true, "prerelease": false, "assets": []}]
JSON
assert_rejects "two drafts for one tag before creation" "delete the other with" \
    verify-creatable v0.2.0 "$work/releases-two-drafts.json"
assert_rejects "two drafts for one tag before the flip" "delete the other with" \
    verify-draft v0.2.0 false "$work/releases-two-drafts.json"
{ "$helper_path" verify-creatable v0.2.0 "$work/releases-two-drafts.json" 2>&1 || true; } |
    grep -Fq 'releases/43' ||
    fail "the two-drafts refusal must name the ids to delete"

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

# --- each slot declares exactly the provenance the release expects of it

# An attestation is a self-report. Without a pin, a compromised builder in its
# own slot could record another image, invent a suite so the pinned image is
# never recorded under its key, or add component keys the release never built
# with; the manifest would carry all of it with every asset digest intact.
restate() {
    python3 - "$(attestation "$1")" "$2" <<'PY'
import json, sys
path, change = sys.argv[1], json.loads(sys.argv[2])
document = json.load(open(path))
for key, value in change.items():
    if value is None:
        document.pop(key, None)
    else:
        document[key] = value
open(path, "w").write(json.dumps(document, indent=2) + "\n")
PY
    job_outputs_for "$job_outputs"
}
restate rpm '{"image": "registry.fedoraproject.org/fedora:44@sha256:dead"}'
assert_rejects "attestation swapping its build image" "declares the build image" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
assert_rejects "swapped build image reaching the manifest" "declares the build image" \
    manifest v0.2.0 0.2.0 0000000000000000000000000000000000000000 false \
    tyvsmith/facelock deadbeef "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" "$work/forged.json"
build_artifacts
restate rpm '{"suite": "reproducible"}'
assert_rejects "attestation inventing a suite" "declares suite" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts
restate deb-trixie '{"suite": "resolute"}'
assert_rejects "attestation claiming another suite" "declares suite" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts
restate rpm '{"components": {"sbom": {"sha256": "forged"}}}'
assert_rejects "attestation adding a component" "declares components" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts
restate onnxruntime '{"components": null}'
assert_rejects "attestation omitting its component" "declares components" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts
restate build '{"image": "docker.io/library/ubuntu:24.04@sha256:dead"}'
assert_rejects "attestation declaring an image its slot has none of" "declares the build image" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts
restate build '{"toolchain": {"rustc": "1.95.0"}}'
assert_rejects "attestation declaring a field the release does not record" "declares keys" \
    verify-digests "$artifacts" "$job_outputs" "$staged" "$work/actual-staged" false
build_artifacts

# Two attestations claiming one provenance key is a contradiction, not a merge;
# the per-slot pin refuses the second claim before the collision is reached.
python3 - "$(attestation cargo-vendor)" <<'PY'
import json, sys
path = sys.argv[1]
document = json.load(open(path))
document.setdefault("components", {})["onnxruntime"] = {"version": "666"}
open(path, "w").write(json.dumps(document, indent=2) + "\n")
PY
job_outputs_for "$job_outputs"
assert_rejects "two attestations claiming one component" "declares components" \
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

# --- the final readback holds every published asset to the manifest

# Names alone would let one run's binaries sit under another run's digests.
# The listing the API returns carries each asset's size, and a digest where
# the API exposes one; both are compared with MANIFEST.json, and the manifest
# itself with the local file.
published_listing() {
    # <output> <mode>: the release as the API would list it after upload.
    python3 - "$manifest" "$1" "$2" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

manifest_path, output, mode = sys.argv[1:4]
manifest = json.loads(Path(manifest_path).read_text())
payload = Path(manifest_path).read_bytes()
assets = [
    {"name": entry["name"], "size": entry["size"], "digest": f"sha256:{entry['sha256']}"}
    for entry in manifest["assets"]
]
assets.append({"name": "MANIFEST.json", "size": len(payload),
               "digest": "sha256:" + hashlib.sha256(payload).hexdigest()})
if mode == "no-digest":
    for asset in assets:
        del asset["digest"]
elif mode == "wrong-size":
    assets[0]["size"] += 1
elif mode == "wrong-digest":
    assets[1]["digest"] = "sha256:" + "0" * 64
elif mode == "wrong-manifest":
    assets[-1]["size"] -= 1
elif mode == "unknown-digest":
    assets[2]["digest"] = "sha512:" + "0" * 128
elif mode == "unlisted":
    assets.append({"name": "facelock-x86_64-linux-musl", "size": 1})
release = {"id": 42, "tag_name": "v0.2.0", "draft": True, "prerelease": False, "assets": assets}
Path(output).write_text(json.dumps([release]) + "\n")
PY
}
published_listing "$work/published-ok.json" exact
"$helper_path" verify-published "$work/published-ok.json" v0.2.0 "$manifest" >/dev/null ||
    fail "a published asset list matching the manifest was rejected"
published_listing "$work/published-no-digest.json" no-digest
"$helper_path" verify-published "$work/published-no-digest.json" v0.2.0 "$manifest" >/dev/null ||
    fail "an API listing without digests must still pass on sizes"
echo "release artifacts case: published assets matching the manifest accepted"
published_listing "$work/published-wrong-size.json" wrong-size
assert_rejects "published asset whose size differs from the manifest" "size" \
    verify-published "$work/published-wrong-size.json" v0.2.0 "$manifest"
published_listing "$work/published-wrong-digest.json" wrong-digest
assert_rejects "published asset whose digest differs from the manifest" "digest" \
    verify-published "$work/published-wrong-digest.json" v0.2.0 "$manifest"
published_listing "$work/published-wrong-manifest.json" wrong-manifest
assert_rejects "published manifest that is not the generated one" "MANIFEST.json" \
    verify-published "$work/published-wrong-manifest.json" v0.2.0 "$manifest"
published_listing "$work/published-unknown-digest.json" unknown-digest
assert_rejects "published asset with a digest that cannot be compared" "digest" \
    verify-published "$work/published-unknown-digest.json" v0.2.0 "$manifest"
published_listing "$work/published-unlisted.json" unlisted
assert_rejects "published asset the manifest does not cover" "not covered" \
    verify-published "$work/published-unlisted.json" v0.2.0 "$manifest"
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
        if output="$(FACELOCK_RELEASE_ATTESTATIONS="$mutant" bash "$self" 2>&1)"; then
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
    assert_loader_mutation_rejected "build images taken from the attestation" \
        's/^        if document.get\("image"\) != image:$/        if False:/' \
        "attestation swapping its build image: helper accepted"
    assert_loader_mutation_rejected "component keys taken from the attestation" \
        's/^        if sorted\(document.get\("components", \{\}\)\) != components:$/        if False:/' \
        "attestation adding a component: helper accepted"

    # The helper itself, mutated into a tree of its own so the checkout stays
    # clean: it resolves the repository root from its own location.
    assert_helper_mutation_rejected() {
        local context="$1" expression="$2" needle="$3"
        local root mutant
        root="$mutation_root/helper-$(printf '%s' "$context" | tr ' ' '-')"
        mkdir -p "$root/.github/workflows/scripts"
        ln -s "$repo_root/scripts" "$root/scripts"
        ln -s "$repo_root/dist" "$root/dist"
        cp .github/workflows/scripts/release_attestations.py "$root/.github/workflows/scripts/"
        mutant="$root/.github/workflows/scripts/release-assets.sh"
        sed -E "$expression" "$helper_path" >"$mutant"
        chmod +x "$mutant"
        if cmp -s "$helper_path" "$mutant"; then
            fail "$context mutation did not change the release asset helper"
        fi
        local output
        if output="$(FACELOCK_RELEASE_ASSETS="$mutant" bash "$self" 2>&1)"; then
            fail "release artifacts contract accepted $context"
        fi
        printf '%s\n' "$output" | grep -Fq "$needle" ||
            fail "$context mutation failed for an unrelated reason: $output"
        echo "release artifacts mutation: $context rejected"
    }

    # shellcheck disable=SC2016
    assert_helper_mutation_rejected "an empty suite list tolerated" \
        's/^    \[ -n "\$listing" \] \|\| fail "the release matrix names no Debian suite.*$/    [ -n "$listing" ] || return 0/' \
        "allowlist from a matrix with no suite: helper accepted"
    assert_helper_mutation_rejected "python running with the working directory on its path" \
        's/^export PYTHONSAFEPATH=1$/: # safe path disabled/' \
        "imported a module from its working directory"

    assert_workflow_mutation_rejected() {
        local context="$1" expression="$2" needle="$3"
        local mutated
        mutated="$mutation_root/release-$(printf '%s' "$context" | tr ' ' '-').yml"
        sed -E "$expression" "$workflow_path" >"$mutated"
        if cmp -s "$workflow_path" "$mutated"; then
            fail "$context mutation did not change the workflow"
        fi
        local output
        if output="$(FACELOCK_RELEASE_WORKFLOW="$mutated" bash "$self" 2>&1)"; then
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
    assert_workflow_mutation_rejected "manifest upload skipped" \
        's|^( *)run: gh release upload "\$TAG" MANIFEST\.json --clobber$|\1: # disabled|' \
        'must run gh release upload "$TAG" MANIFEST.json'
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "no revalidation before the flip" \
        's|^( *)\$HELPER expected "\$VERSION" "\$DEBIAN_REVISION" "\$RPM_COUNTER" "\$PRERELEASE" final .*|\1: # final readback disabled|' \
        'must run PRERELEASE" final'
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "final readback comparing names only" \
        's|^( *)\$HELPER verify-published .*|\1: # verify-published disabled|' \
        'must run $HELPER verify-published'
    assert_workflow_mutation_rejected "two runs publishing one tag" \
        '/^concurrency:$/,/^$/d' \
        "must declare a concurrency group"
    assert_workflow_mutation_rejected "runs cancelling the one in progress" \
        's/^  cancel-in-progress: false$/  cancel-in-progress: true/' \
        "never cancel it"
    assert_workflow_mutation_rejected "flake evaluation that cannot fail" \
        's|^        run: nix flake check ./dist/nix --no-build$|        run: nix flake check ./dist/nix --no-build\n        continue-on-error: true|' \
        "flake evaluation must gate publication"

    # The checkout-ownership exception. Removed, neutralized, displaced, or
    # owed by a job that just acquired git: each is a tag-time-only failure,
    # which is the class this whole gate exists for.
    assert_workflow_mutation_rejected "a container job left untrusting" \
        '/^      - name: Trust the workspace checkout$/,+1d' \
        "must trust the workspace checkout"
    assert_workflow_mutation_rejected "the ownership exception neutralized" \
        's|^        run: git config --global --add safe\.directory .*$|        run: ": # trust disabled"|' \
        "must trust the workspace checkout with exactly"
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "the ownership exception taken late" \
        's|^      - name: Trust the workspace checkout$|      - name: Report the workspace\n        run: ls "$GITHUB_WORKSPACE"\n\n      - name: Trust the workspace checkout|' \
        "immediately after checking it out"
    assert_workflow_mutation_rejected "a container job gaining git untrusted" \
        's|^            rust cargo clang-devel|            git rust cargo clang-devel|' \
        "container job build-rpm installs git"
    assert_workflow_mutation_rejected "an exception kept past its git install" \
        's|^            git$||' \
        "trusts the workspace checkout but installs no git"
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
    assert_workflow_mutation_rejected "COPR verification dropped" \
        '/^  verify-copr:$/,/^  trigger-pages:$/{/^  trigger-pages:$/!d}' \
        "release jobs drifted"
    assert_workflow_mutation_rejected "COPR verification open to prereleases" \
        '/^  verify-copr:$/,/^  trigger-pages:$/{/^    if: /d}' \
        "verify-copr must be stable-only"
    assert_workflow_mutation_rejected "COPR verification polling without a deadline" \
        '/^  verify-copr:$/,/^  trigger-pages:$/{/^    timeout-minutes: /d}' \
        "verify-copr polls and must carry its own timeout"
    assert_workflow_mutation_rejected "release upload without the publication token" \
        '0,/^          token: \$\{\{ secrets.RELEASE_PAT \}\}$/s///' \
        "every release upload must pass the publication token"
    # shellcheck disable=SC2016
    assert_workflow_mutation_rejected "tag rewritten at publication" \
        's/^      TAG: \$\{\{ github.ref_name \}\}$/      TAG: ${{ github.ref_name }}\n      TAG_TARGET: target_commitish/' \
        "publishing must not send a tag or target commitish"

    # Quote style is not semantics: a double-quoted --suite value must parse
    # the same as the single-quoted one the workflow uses today, so extraction
    # cannot silently go blind the day someone swaps the quoting.
    suite_double_quoted="$mutation_root/release-suite-double-quoted.yml"
    sed -E 's/--suite .(\$\{\{ matrix\.suite \}\})./--suite "\1"/' \
        "$workflow_path" >"$suite_double_quoted"
    cmp -s "$workflow_path" "$suite_double_quoted" &&
        fail "double-quoted suite mutation did not change the workflow"
    if ! output="$(FACELOCK_RELEASE_WORKFLOW="$suite_double_quoted" bash "$self" 2>&1)"; then
        fail "release artifacts contract rejected a double-quoted --suite value: $output"
    fi
    echo "release artifacts mutation: double-quoted --suite value accepted"
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
