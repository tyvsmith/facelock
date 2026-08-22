#!/usr/bin/env bash
# Shared release identity conversions. This file is sourced by release tooling.

release_validate_cargo_version() {
    local version="${1:-}"
    if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(alpha|beta|rc)\.(0|[1-9][0-9]*))?$ ]]; then
        echo "invalid Cargo release version '$version' (expected X.Y.Z or X.Y.Z-{alpha,beta,rc}.N)" >&2
        return 1
    fi
}

release_compare_unsigned() {
    local left="${1:-}"
    local right="${2:-}"
    if [[ ! "$left" =~ ^[0-9]+$ || ! "$right" =~ ^[0-9]+$ ]]; then
        echo "cannot compare non-negative integers '$left' and '$right'" >&2
        return 1
    fi
    if [ "${#left}" -lt "${#right}" ]; then
        printf '%s\n' -1
    elif [ "${#left}" -gt "${#right}" ]; then
        printf '%s\n' 1
    elif [[ "$left" < "$right" ]]; then
        printf '%s\n' -1
    elif [[ "$left" > "$right" ]]; then
        printf '%s\n' 1
    else
        printf '%s\n' 0
    fi
}

release_compare_cargo_versions() {
    local left="${1:-}"
    local right="${2:-}"
    local left_stage left_prerelease right_stage right_prerelease comparison index
    local -a left_core right_core
    release_validate_cargo_version "$left" || return 1
    release_validate_cargo_version "$right" || return 1

    [[ "$left" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-(alpha|beta|rc)\.([0-9]+))?$ ]]
    left_core=("${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}")
    left_stage="${BASH_REMATCH[5]:-stable}"
    left_prerelease="${BASH_REMATCH[6]:-0}"
    [[ "$right" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-(alpha|beta|rc)\.([0-9]+))?$ ]]
    right_core=("${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}")
    right_stage="${BASH_REMATCH[5]:-stable}"
    right_prerelease="${BASH_REMATCH[6]:-0}"

    for index in 0 1 2; do
        comparison="$(release_compare_unsigned "${left_core[index]}" "${right_core[index]}")" || return 1
        if [ "$comparison" != 0 ]; then
            printf '%s\n' "$comparison"
            return 0
        fi
    done

    case "$left_stage" in alpha) left_stage=1 ;; beta) left_stage=2 ;; rc) left_stage=3 ;; stable) left_stage=4 ;; esac
    case "$right_stage" in alpha) right_stage=1 ;; beta) right_stage=2 ;; rc) right_stage=3 ;; stable) right_stage=4 ;; esac
    if [ "$left_stage" -lt "$right_stage" ]; then
        printf '%s\n' -1
    elif [ "$left_stage" -gt "$right_stage" ]; then
        printf '%s\n' 1
    elif [ "$left_stage" -eq 4 ]; then
        printf '%s\n' 0
    else
        release_compare_unsigned "$left_prerelease" "$right_prerelease"
    fi
}

release_validate_transition() {
    local previous="${1:-}"
    local next="${2:-}"
    local comparison
    comparison="$(release_compare_cargo_versions "$previous" "$next")" || return 1
    if [ "$comparison" -gt 0 ]; then
        echo "refusing release version regression from $previous to $next" >&2
        return 1
    elif [ "$comparison" -eq 0 ] && ! release_is_prerelease "$next"; then
        echo "refusing repeated stable release version $next" >&2
        return 1
    fi
}

release_cargo_from_tag() {
    local tag="${1:-}"
    if [[ "$tag" != v* ]]; then
        echo "invalid release tag '$tag' (expected leading v)" >&2
        return 1
    fi
    local version="${tag#v}"
    release_validate_cargo_version "$version" || return 1
    printf '%s\n' "$version"
}

release_tag_from_cargo() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    printf 'v%s\n' "$version"
}

release_is_prerelease() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    [[ "$version" == *-* ]]
}

release_github_prerelease() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    if release_is_prerelease "$version"; then
        printf 'true\n'
    else
        printf 'false\n'
    fi
}

release_base_version() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    printf '%s\n' "${version%%-*}"
}

release_prerelease_suffix() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    if release_is_prerelease "$version"; then
        printf '%s\n' "${version#*-}"
    fi
}

release_debian_upstream() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    if release_is_prerelease "$version"; then
        printf '%s~%s\n' "${version%%-*}" "${version#*-}"
    else
        printf '%s\n' "$version"
    fi
}

release_validate_positive_revision() {
    local revision="${1:-}"
    if [[ ! "$revision" =~ ^[1-9][0-9]*$ ]]; then
        echo "invalid package revision '$revision' (expected a positive integer)" >&2
        return 1
    fi
}

release_debian_common_version() {
    local version="${1:-}"
    local revision="${2:-}"
    local upstream
    release_validate_positive_revision "$revision" || return 1
    upstream="$(release_debian_upstream "$version")" || return 1
    printf '%s-%s\n' "$upstream" "$revision"
}

release_debian_suite_suffix() {
    case "${1:-}" in
        trixie) printf '~deb13u1\n' ;;
        resolute) printf '~ubuntu26.04.1\n' ;;
        *)
            echo "unsupported Debian/Ubuntu suite '${1:-}'" >&2
            return 1
            ;;
    esac
}

release_debian_version() {
    local version="${1:-}"
    local revision="${2:-}"
    local suite="${3:-}"
    local common suffix
    common="$(release_debian_common_version "$version" "$revision")" || return 1
    suffix="$(release_debian_suite_suffix "$suite")" || return 1
    printf '%s%s\n' "$common" "$suffix"
}

release_debian_source_basename() {
    local version="${1:-}"
    local revision="${2:-}"
    local suite="${3:-}"
    local debian_version
    debian_version="$(release_debian_version "$version" "$revision" "$suite")" || return 1
    printf 'facelock_%s\n' "$debian_version"
}

release_debian_binary_basename() {
    local version="${1:-}"
    local revision="${2:-}"
    local suite="${3:-}"
    local architecture="${4:-}"
    local source_basename
    if [[ ! "$architecture" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
        echo "invalid Debian architecture '$architecture'" >&2
        return 1
    fi
    source_basename="$(release_debian_source_basename "$version" "$revision" "$suite")" || return 1
    printf '%s_%s\n' "$source_basename" "$architecture"
}

release_arch_pkgver() {
    local version="${1:-}"
    release_validate_cargo_version "$version" || return 1
    if release_is_prerelease "$version"; then
        local suffix="${version#*-}"
        printf '%s%s\n' "${version%%-*}" "${suffix//./}"
    else
        printf '%s\n' "$version"
    fi
}

release_arch_version() {
    local version="${1:-}"
    local revision="${2:-}"
    local pkgver
    release_validate_positive_revision "$revision" || return 1
    pkgver="$(release_arch_pkgver "$version")" || return 1
    printf '%s-%s\n' "$pkgver" "$revision"
}

release_rpm_version() {
    release_base_version "${1:-}"
}

release_rpm_release() {
    local version="${1:-}"
    local counter="${2:-}"
    release_validate_cargo_version "$version" || return 1
    if release_is_prerelease "$version"; then
        release_validate_positive_revision "$counter" || return 1
        printf '0.%s.%s\n' "$counter" "${version#*-}"
    else
        printf '1\n'
    fi
}

release_rpm_evr() {
    local version="${1:-}"
    local counter="${2:-}"
    local rpm_version rpm_release
    rpm_version="$(release_rpm_version "$version")" || return 1
    rpm_release="$(release_rpm_release "$version" "$counter")" || return 1
    printf '%s-%s\n' "$rpm_version" "$rpm_release"
}

release_next_arch_revision() {
    local version="${1:-}"
    local pkgbuild="${2:-}"
    local new_pkgver old_pkgver old_pkgrel
    new_pkgver="$(release_arch_pkgver "$version")" || return 1
    old_pkgver="$(sed -n 's/^pkgver=//p' "$pkgbuild" | head -1)"
    old_pkgrel="$(sed -n 's/^pkgrel=//p' "$pkgbuild" | head -1)"
    if [ "$old_pkgver" = "$new_pkgver" ] && [[ "$old_pkgrel" =~ ^[1-9][0-9]*$ ]]; then
        printf '%s\n' "$((old_pkgrel + 1))"
    else
        printf '1\n'
    fi
}

release_next_debian_revision() {
    local version="${1:-}"
    local changelog="${2:-}"
    local new_upstream current
    new_upstream="$(release_debian_upstream "$version")" || return 1
    current="$(sed -n 's/^facelock (\([^)]*\)).*/\1/p' "$changelog" | head -1)"
    if [[ "$current" =~ ^(.+)-([1-9][0-9]*)(~.*)?$ ]] && [ "${BASH_REMATCH[1]}" = "$new_upstream" ]; then
        printf '%s\n' "$((BASH_REMATCH[2] + 1))"
    else
        printf '1\n'
    fi
}

release_current_debian_revision() {
    local version="${1:-}"
    local changelog="${2:-}"
    local expected_upstream current
    expected_upstream="$(release_debian_upstream "$version")" || return 1
    current="$(sed -n 's/^facelock (\([^)]*\)).*/\1/p' "$changelog" | head -1)"
    if [[ "$current" =~ ^(.+)-([1-9][0-9]*)(~.*)?$ ]] && [ "${BASH_REMATCH[1]}" = "$expected_upstream" ]; then
        printf '%s\n' "${BASH_REMATCH[2]}"
    else
        echo "Debian changelog '$current' does not carry release $version" >&2
        return 1
    fi
}

release_next_rpm_counter() {
    local version="${1:-}"
    local spec="${2:-}"
    local new_version old_version old_release
    new_version="$(release_rpm_version "$version")" || return 1
    old_version="$(sed -n 's/^Version:[[:space:]]*//p' "$spec" | head -1)"
    old_release="$(sed -n 's/^Release:[[:space:]]*//p' "$spec" | head -1)"
    if [ "$old_version" != "$new_version" ]; then
        printf '1\n'
    elif [[ "$old_release" =~ ^0\.([1-9][0-9]*)\.(alpha|beta|rc)\.([0-9]+)(%\{\?dist\})?$ ]]; then
        local previous="$old_version-${BASH_REMATCH[2]}.${BASH_REMATCH[3]}"
        local previous_counter="${BASH_REMATCH[1]}"
        release_validate_transition "$previous" "$version" || return 1
        printf '%s\n' "$((previous_counter + 1))"
    elif [[ "$old_release" =~ ^1(%\{\?dist\})?$ ]]; then
        if release_is_prerelease "$version"; then
            echo "refusing prerelease $version after stable RPM Version $old_version" >&2
            return 1
        fi
        printf '1\n'
    else
        echo "cannot derive the next RPM prerelease counter from Release '$old_release'" >&2
        return 1
    fi
}

release_current_rpm_counter() {
    local version="${1:-}"
    local spec="${2:-}"
    local expected_version old_version old_release
    expected_version="$(release_rpm_version "$version")" || return 1
    old_version="$(sed -n 's/^Version:[[:space:]]*//p' "$spec" | head -1)"
    old_release="$(sed -n 's/^Release:[[:space:]]*//p' "$spec" | head -1)"
    [ "$old_version" = "$expected_version" ] || { echo "RPM Version '$old_version' != '$expected_version'" >&2; return 1; }
    if release_is_prerelease "$version"; then
        if [[ "$old_release" =~ ^0\.([1-9][0-9]*)\.(alpha|beta|rc)\.([0-9]+)%\{\?dist\}$ ]] &&
            [ "${BASH_REMATCH[2]}.${BASH_REMATCH[3]}" = "${version#*-}" ]; then
            printf '%s\n' "${BASH_REMATCH[1]}"
        else
            echo "RPM Release '$old_release' does not carry prerelease $version" >&2
            return 1
        fi
    elif [ "$old_release" = '1%{?dist}' ]; then
        printf '1\n'
    else
        echo "RPM Release '$old_release' is not stable release 1" >&2
        return 1
    fi
}

release_packit_has_production_release_job() {
    local config="${1:-.packit.yaml}"
    python3 - "$config" <<'PY'
import json
import sys
from pathlib import Path

try:
    config = json.loads(Path(sys.argv[1]).read_text())
    jobs = config["jobs"]
    if not isinstance(jobs, list):
        raise TypeError("jobs must be a list")
except (OSError, KeyError, TypeError, json.JSONDecodeError) as error:
    print(f"invalid JSON-subset Packit config: {error}", file=sys.stderr)
    raise SystemExit(2)

found = any(
    isinstance(job, dict)
    and job.get("job") == "copr_build"
    and job.get("trigger") == "release"
    and job.get("project") == "facelock"
    for job in jobs
)
raise SystemExit(0 if found else 1)
PY
}

release_validate_packit_channel() {
    local version="${1:-}"
    local config="${2:-.packit.yaml}"
    local packit_status
    release_validate_cargo_version "$version" || return 1
    if release_packit_has_production_release_job "$config"; then
        packit_status=0
    else
        packit_status=$?
        if [ "$packit_status" -ne 1 ]; then
            return "$packit_status"
        fi
    fi
    if release_is_prerelease "$version"; then
        if [ "$packit_status" -eq 0 ]; then
            echo "prerelease $version cannot carry a release-triggered production COPR job" >&2
            return 1
        fi
    elif [ "$packit_status" -ne 0 ]; then
        echo "stable $version requires deliberate production COPR restoration in its stable-tagged config" >&2
        return 1
    fi
}

release_check_metadata() {
    local tag="${1:-}"
    local version cargo_version arch_pkgver debian_upstream rpm_version top_debian spec_release
    version="$(release_cargo_from_tag "$tag")" || return 1
    cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
    arch_pkgver="$(release_arch_pkgver "$version")" || return 1
    debian_upstream="$(release_debian_upstream "$version")" || return 1
    rpm_version="$(release_rpm_version "$version")" || return 1

    [ "$cargo_version" = "$version" ] || { echo "Cargo.toml version '$cargo_version' != tag version '$version'" >&2; return 1; }
    for pkgbuild in dist/PKGBUILD dist/PKGBUILD-bin; do
        grep -Fqx "_tag=$version" "$pkgbuild" || { echo "$pkgbuild _tag does not match $version" >&2; return 1; }
        grep -Fqx "pkgver=$arch_pkgver" "$pkgbuild" || { echo "$pkgbuild pkgver does not match $arch_pkgver" >&2; return 1; }
        grep -Eq '^pkgrel=[1-9][0-9]*$' "$pkgbuild" || { echo "$pkgbuild has invalid pkgrel" >&2; return 1; }
    done
    grep -Fqx "pkgver=$arch_pkgver" dist/PKGBUILD-git || { echo "dist/PKGBUILD-git pkgver does not match $arch_pkgver" >&2; return 1; }
    grep -Fqx "Version:        $rpm_version" dist/facelock.spec || { echo "RPM Version does not match $rpm_version" >&2; return 1; }
    spec_release="$(sed -n 's/^Release:[[:space:]]*//p' dist/facelock.spec | head -1)"
    if release_is_prerelease "$version"; then
        if [[ ! "$spec_release" =~ ^0\.[1-9][0-9]*\.(alpha|beta|rc)\.([0-9]+)%\{\?dist\}$ ]] ||
            [ "${BASH_REMATCH[1]:-}.${BASH_REMATCH[2]:-}" != "${version#*-}" ]; then
            echo "RPM Release '$spec_release' does not match $version" >&2
            return 1
        fi
    else
        [ "$spec_release" = '1%{?dist}' ] || { echo "stable RPM Release must be 1%{?dist}" >&2; return 1; }
    fi
    top_debian="$(sed -n 's/^facelock (\([^)]*\)).*/\1/p' debian/changelog | head -1)"
    [[ "$top_debian" == "$debian_upstream-"* ]] || { echo "Debian changelog '$top_debian' does not match $version" >&2; return 1; }
    release_validate_packit_channel "$version" .packit.yaml
}

release_write_github_outputs() {
    local tag="${1:-}"
    local output_file="${2:-}"
    local version prerelease arch_pkgver debian_upstream debian_revision rpm_version rpm_counter
    version="$(release_cargo_from_tag "$tag")" || return 1
    prerelease="$(release_github_prerelease "$version")" || return 1
    arch_pkgver="$(release_arch_pkgver "$version")" || return 1
    debian_upstream="$(release_debian_upstream "$version")" || return 1
    debian_revision="$(release_current_debian_revision "$version" debian/changelog)" || return 1
    rpm_version="$(release_rpm_version "$version")" || return 1
    rpm_counter="$(release_current_rpm_counter "$version" dist/facelock.spec)" || return 1
    {
        printf 'cargo-version=%s\n' "$version"
        printf 'prerelease=%s\n' "$prerelease"
        printf 'arch-pkgver=%s\n' "$arch_pkgver"
        printf 'debian-upstream=%s\n' "$debian_upstream"
        printf 'debian-revision=%s\n' "$debian_revision"
        printf 'rpm-version=%s\n' "$rpm_version"
        printf 'rpm-counter=%s\n' "$rpm_counter"
    } >> "$output_file"
}
