#!/usr/bin/env bash
# Every package name all three PKGBUILDs declare must resolve in the pinned
# Arch repositories.
#
# #209 was exactly this: a package named something Arch does not ship. It
# surfaces on a user's machine, at install time, on the platform most facelock
# users are on. The names are checked here rather than only inside the full
# build because a typo has to fail in seconds, and because only dist/PKGBUILD
# gets built end to end — dist/PKGBUILD-git is what Omarchy's installer pulls
# through `omarchy-pkg-aur-add facelock-git`, and its depends list is not the
# same one.
#
# Resolution goes through pacman, not a repository listing, so `provides` counts:
# `onnxruntime` and `cargo` are both virtual today, satisfied by
# `onnxruntime-cpu` and `rust`. A listing-based check would reject both.
set -euo pipefail

RECIPE=${FACELOCK_RECIPE_DIR:-/recipe}
BUILDER=builder

fail() {
    echo "arch dependency contract: $*" >&2
    exit 1
}

[ -d "$RECIPE" ] || fail "no recipe directory at $RECIPE"

# makepkg treats the recipe directory as BUILDDIR, refuses to read one it
# cannot write, and refuses to run as root at all. Work from a copy the
# unprivileged builder owns.
workdir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-arch-srcinfo.XXXXXX")"
trap 'rm -rf -- "$workdir"' EXIT
cp -- "$RECIPE"/* "$workdir/"
chown -R "$BUILDER:$BUILDER" "$workdir"

pacman -Sy --noconfirm >/dev/null

failures=0
checked=0

for recipe in PKGBUILD PKGBUILD-bin PKGBUILD-git; do
    [ -f "$workdir/$recipe" ] || fail "missing recipe: $RECIPE/$recipe"

    # Ask the recipe what it declares instead of parsing the arrays here: this
    # expands the same variables makepkg would and drops architecture-specific
    # and version-constrained forms into one flat list.
    srcinfo="$(runuser -u "$BUILDER" -- \
        bash -c "cd '$workdir' && makepkg -p '$recipe' --printsrcinfo")" ||
        fail "$recipe does not parse"

    mapfile -t names < <(
        printf '%s\n' "$srcinfo" |
            sed -n -E 's/^[[:space:]]*(depends|makedepends|checkdepends|optdepends) = //p' |
            # optdepends carry a ": reason" suffix; every kind may carry a
            # >=/<=/= version constraint. Keep the bare package name.
            sed -E 's/:.*$//; s/[<>=].*$//' |
            sed -E 's/[[:space:]]+$//' |
            grep -v '^$' |
            LC_ALL=C sort -u
    )
    [ "${#names[@]}" -gt 0 ] || fail "$recipe declares no dependencies at all"

    for name in "${names[@]}"; do
        checked=$((checked + 1))
        if pacman -Sp --print-format '%n' -- "$name" >/dev/null 2>&1; then
            printf '  ok    %-14s %s\n' "$recipe" "$name"
        else
            printf '  FAIL  %-14s %s  --  no repository package provides it\n' \
                "$recipe" "$name"
            failures=$((failures + 1))
        fi
    done
done

echo ""
if [ "$failures" -ne 0 ]; then
    echo "=== Arch dependency contract: $failures of $checked declared names do not resolve ==="
    exit 1
fi
echo "=== Arch dependency contract: all $checked declared names resolve ==="
