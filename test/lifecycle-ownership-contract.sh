#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
failures=0

fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}

# These token-aware detectors are exercised by mutation fixtures below before
# they inspect shipped guidance.
has_recursive_retained_rm() {
    awk '
        function clean(token) {
            sub(/^[^[:alnum:]_./-]+/, "", token)
            sub(/[^[:alnum:]_./-]+$/, "", token)
            return token
        }

        {
            rm_seen = 0
            recursive = 0
            retained_root = 0

            for (i = 1; i <= NF; i++) {
                token = clean($i)
                if (!rm_seen) {
                    if (token == "rm")
                        rm_seen = 1
                    continue
                }

                if (token == "--recursive" || token ~ /^-[^-]*[rR][^-]*$/)
                    recursive = 1
                if (token ~ /^\/etc\/facelock(\/|$)/ ||
                    token ~ /^\/var\/lib\/facelock(\/|$)/ ||
                    token ~ /^\/var\/log\/facelock(\/|$)/)
                    retained_root = 1
            }

            if (rm_seen && recursive && retained_root) {
                print FNR ":" $0
                found = 1
            }
        }

        END { exit(found ? 0 : 1) }
    '
}

has_nosave_facelock_removal() {
    awk '
        function clean(token) {
            sub(/^[^[:alnum:]_./-]+/, "", token)
            sub(/[^[:alnum:]_./-]+$/, "", token)
            return token
        }

        {
            pacman_seen = 0
            remove = 0
            nosave = 0
            facelock = 0

            for (i = 1; i <= NF; i++) {
                raw = $i
                token = clean(raw)
                if (!pacman_seen) {
                    if (token == "pacman")
                        pacman_seen = 1
                    continue
                }

                if (token == "--remove" || token ~ /^-[^-]*R[^-]*$/)
                    remove = 1
                if (token == "--nosave" || token ~ /^-[^-]*n[^-]*$/)
                    nosave = 1
                if (token == "facelock")
                    facelock = 1

                if (raw ~ /;/ || raw ~ /^#/ || token == "&&" || token == "||")
                    break
            }

            if (pacman_seen && remove && nosave && facelock) {
                print FNR ":" $0
                found = 1
            }
        }

        END { exit(found ? 0 : 1) }
    '
}

assert_unsafe_fixture_detected() {
    local detector="$1"
    local name="$2"
    local fixture="$3"

    if ! "$detector" <<<"$fixture" >/dev/null; then
        fail "mutation fixture escaped $detector: $name"
    fi
}

assert_safe_fixture_allowed() {
    local detector="$1"
    local name="$2"
    local fixture="$3"

    if "$detector" <<<"$fixture" >/dev/null; then
        fail "safe fixture triggered $detector: $name"
    fi
}

# Recursive-rm flag case and ordering must not change the answer.
assert_unsafe_fixture_detected has_recursive_retained_rm "uppercase recursive flag" \
    'sudo rm -R /etc/facelock'
assert_unsafe_fixture_detected has_recursive_retained_rm "grouped recursive flag" \
    'rm -fr /var/lib/facelock'
assert_unsafe_fixture_detected has_recursive_retained_rm "separate reordered flags" \
    'rm -f -r /var/log/facelock'
assert_unsafe_fixture_detected has_recursive_retained_rm "uppercase grouped recursive flag" \
    'rm -Rf /etc/facelock'
assert_unsafe_fixture_detected has_recursive_retained_rm "long recursive flag" \
    'rm --recursive /var/lib/facelock'
assert_safe_fixture_allowed has_recursive_retained_rm "package-owned static cleanup" \
    'rm -rf /usr/share/facelock'
assert_safe_fixture_allowed has_recursive_retained_rm "temporary directory cleanup" \
    "rm -rf \"\$TMPDIR\""

# Pacman save destruction is unsafe only for a Facelock removal command.
assert_unsafe_fixture_detected has_nosave_facelock_removal "grouped nosave" \
    'sudo pacman -Rns facelock'
assert_unsafe_fixture_detected has_nosave_facelock_removal "reordered grouped nosave" \
    'sudo pacman -Rsn facelock'
assert_unsafe_fixture_detected has_nosave_facelock_removal "long nosave" \
    'sudo pacman --remove --nosave facelock'
assert_safe_fixture_allowed has_nosave_facelock_removal "save-preserving removal" \
    'sudo pacman -Rs facelock'
assert_safe_fixture_allowed has_nosave_facelock_removal "different package" \
    'sudo pacman -Rns unrelated-package'
assert_safe_fixture_allowed has_nosave_facelock_removal "unrelated prose" \
    'pacman documents --nosave; facelock retains state'

require_text() {
    local path="$1"
    local text="$2"

    if ! grep -Fq -- "$text" "$repo_root/$path"; then
        fail "$path must contain: $text"
    fi
}

reject_text() {
    local path="$1"
    local text="$2"

    if grep -Fq -- "$text" "$repo_root/$path"; then
        fail "$path must not contain: $text"
    fi
}

require_lifecycle_text() {
    local text="$1"

    if ! grep -Fq -- "$text" <<<"$lifecycle_prose"; then
        fail "Package Lifecycle Ownership section must contain: $text"
    fi
}

require_lifecycle_row() {
    local label="$1"
    shift
    local rows
    local count

    rows="$(grep -F -- "| $label |" <<<"$lifecycle_section" || true)"
    count="$(grep -Fc -- "| $label |" <<<"$lifecycle_section" || true)"
    if [ "$count" -ne 1 ]; then
        fail "Package Lifecycle Ownership must contain exactly one '$label' row (found $count)"
        return
    fi

    local text
    for text in "$@"; do
        if ! grep -Fq -- "$text" <<<"$rows"; then
            fail "'$label' row must contain: $text"
        fi
    done
}

reject_state_purge_command() {
    local path="$1"
    local unsafe_lines

    if unsafe_lines="$(has_recursive_retained_rm <"$repo_root/$path")"; then
        printf '%s\n' "$unsafe_lines" >&2
        fail "$path recommends recursive deletion of retained Facelock state"
    fi
}

reject_nosave_facelock_removal() {
    local path="$1"
    local unsafe_lines

    if unsafe_lines="$(has_nosave_facelock_removal <"$repo_root/$path")"; then
        printf '%s\n' "$unsafe_lines" >&2
        fail "$path recommends removing Facelock with Pacman's no-save option"
    fi
}

contract="docs/contracts.md"
lifecycle_section="$({
    sed -n '/^## Package Lifecycle Ownership$/,/^## Filesystem Paths$/p' \
        "$repo_root/$contract"
} || true)"

if [ -z "$lifecycle_section" ]; then
    fail "$contract is missing the Package Lifecycle Ownership section"
fi
lifecycle_prose="$(tr '\n' ' ' <<<"$lifecycle_section")"

# Freeze status and the ownership classes are scoped to the lifecycle section,
# not satisfied by an unrelated mention elsewhere in this large contract file.
require_lifecycle_text "This is the Wave 0 ownership freeze for issue #232."
require_lifecycle_text "does not claim that the current"
require_lifecycle_text "Debian purge script already implements the bounded purge"
require_lifecycle_text "Ordinary removal is not data deletion."
require_lifecycle_row "Package-owned static integration" \
    "Remove through the package manager" \
    "recreated byte-for-byte by reinstalling the package"
require_lifecycle_row "Biometric and operational state" \
    "database and its WAL/SHM sidecars" \
    "encryption keys and sealed keys" \
    "downloaded models" \
    "enrollment markers" \
    "audit logs" \
    "Preserve all of it"
require_lifecycle_row "PAM integration and provenance" \
    "Delete provenance only after the corresponding PAM cleanup is proven complete"
require_lifecycle_row "Externally configured state" \
    "Never package-owned" \
    "Leave it untouched and report it as an external remnant"

# Bind each package operation to its own row so remove, purge, and erase cannot
# borrow one another's promises.
require_lifecycle_row "Debian \`remove\`" \
    "Debian conffile" \
    "remains at its installed path" \
    "Biometric and operational state also remains"
require_lifecycle_row "Debian \`purge\`" \
    "removes the conffile" \
    "only safe remnants inside the compiled roots" \
    "Unsafe and external remnants are retained and reported"
require_lifecycle_row "RPM erase" \
    "RPM \`%config(noreplace)\`" \
    "\`.rpmsave\` is retained state" \
    "not something a Facelock script deletes"
require_lifecycle_row "Arch package removal" \
    "pacman's native saved-configuration behavior" \
    "\`.pacsave\`"
require_lifecycle_row "\`just uninstall\`" \
    "preserves \`/etc/facelock\`" \
    "biometric and operational state"

require_lifecycle_text "PAM provenance and rollback files are not biometric state."
require_lifecycle_text "Preserve PAM provenance when cleanup is incomplete"
require_lifecycle_text "The only purge roots are the compiled Facelock roots"
require_lifecycle_text "Configured paths outside those roots are external remnants"
require_lifecycle_text "leave them untouched, report that they were refused as external"
require_lifecycle_text "never follow a symbolic link"
require_lifecycle_text "hard-linked object"
require_lifecycle_text "never cross a mount point"
require_lifecycle_text "must not strand package-manager state"
require_lifecycle_text "does not promise secure erasure"
require_lifecycle_text "Debian \`postrm purge\` is self-contained"
require_lifecycle_text "never invokes the already-removed \`facelock\` binary"

for guide in docs/quickstart.md book/src/quickstart.md; do
    require_text "$guide" "Package lifecycle and retained data"
done

lifecycle_messages=(
    justfile
    dist/debian/postrm
    dist/facelock.spec
    dist/facelock.install
)

lifecycle_guidance=(
    docs/quickstart.md
    book/src/quickstart.md
    "${lifecycle_messages[@]}"
)

for surface in "${lifecycle_guidance[@]}"; do
    reject_state_purge_command "$surface"
done

for message in "${lifecycle_messages[@]}"; do
    reject_text "$message" "To remove all face data"
    require_text "$message" "Retained state cleanup is intentionally not automated."
    require_text "$message" "Cleanup must stay within the fixed roots above, leave configured external paths untouched, and refuse links or mount crossings."
    require_text "$message" "Filesystem deletion does not securely erase SSDs, snapshots, or backups."
done

if [ "$failures" -ne 0 ]; then
    echo "lifecycle ownership contract: $failures failure(s)" >&2
    exit 1
fi

echo "lifecycle ownership contract: OK"
