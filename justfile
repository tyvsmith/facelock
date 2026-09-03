# facelock build automation
# Usage: just <recipe>

# Build in debug mode (development)
build:
    cargo build --workspace

# Build in release mode (for install)
build-release:
    cargo build --release --workspace
    cargo build --release -p facelock-cli --features tpm

# Run all unit tests
test:
    cargo test --workspace

# Run all tests including hardware-dependent (ignored) tests
test-all:
    cargo test --workspace -- --include-ignored

# Run clippy with warnings as errors.
#
# `--all-targets` is load-bearing, not tidiness: without it clippy skips test,
# bench and example targets entirely, so a deny-by-default lint in test code
# never reaches this gate. That matters disproportionately here because file
# modes ARE a security contract in this project (0600 database, 0711 state
# dir), and `non_octal_unix_permissions` is exactly the lint that catches a
# `from_mode(600)` — which means 0o1130, not 0o600 — before it ships.

# Keep in sync with .github/workflows/ci.yml.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format check
fmt-check:
    cargo fmt --all -- --check

# Format code
fmt:
    cargo fmt --all

# Scan the dependency tree for RustSec advisories (mirrors the CI cargo-audit job).
# Ignore policy lives in .cargo/audit.toml; deny policy is set here so CI matches.

# Requires cargo-audit: cargo install cargo-audit --locked
audit:
    cargo audit --deny unmaintained --deny unsound

# Verify the PAM module compiles on its OWN and stays off the async-io backend.
# `cargo build --workspace` unifies zbus features with facelock-cli/-polkit, so it
# hides an incoherent feature set in pam-facelock; only a standalone build catches
# it. The dep guard then forbids the async-io runtime backend (async-io/async-signal/
# polling + the async-executor/async-fs/async-lock trio) while allowing
# signal-hook-registry, which the correct tokio backend legitimately pulls via
# tokio's "process" feature. Keep in sync with .github/workflows/ci.yml

# ("Build pam-facelock in isolation" + "Verify pam-facelock dependency surface").
check-pam-standalone:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p pam-facelock
    cargo tree -p pam-facelock --edges normal --prefix none | awk '{print $1}' | sort -u > /tmp/pam-deps
    echo "pam-facelock crate count: $(wc -l < /tmp/pam-deps)"
    if grep -Eq '^(async-io|async-signal|async-executor|async-fs|async-lock|polling)$' /tmp/pam-deps; then
        echo "forbidden async-io backend crates in pam-facelock (expected the tokio backend)" >&2
        exit 1
    fi
    echo "pam-facelock dependency guard passed"

# Verify agent-facing docs and executable documentation contracts.

# Pass a git ref to also run the coupling check against it.
check-agent-docs base='':
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ base }}" ]; then
        python3 test/check-agent-docs.py --base "{{ base }}"
    else
        python3 test/check-agent-docs.py
    fi
    bash test/lifecycle-ownership-contract.sh
    bash test/debian-postrm-purge.sh
    bash test/rpm-authselect-contract.sh

# Pin the trust boundary of the comment-triggered Claude workflow (docs/security.md, CI Trust Boundary).
check-workflow-policy:
    python3 test/check-workflow-policy.py

# Exercise Debian remove/purge policy below disposable fixed roots only.
test-debian-postrm-purge:
    bash test/debian-postrm-purge.sh

# Re-check documented package names against live Arch, AUR, Debian and Fedora.
#
# Opt-in, and deliberately outside `just check`: it needs the network, and a
# repository outage must not turn an unrelated pull request red. The offline
# half runs under `cargo test` -- `conformance::packages` holds every
# documented name to the packaging manifest that declares it. This recipe is
# what proves the manifests themselves are not naming a package that stopped
# existing.
check-package-names-live:
    python3 test/check-package-names-live.py

# Run all checks (test + lint + format + audit + PAM standalone surface + agent docs)
check: test lint fmt-check audit check-pam-standalone check-agent-docs test-source-install-daemon-lifecycle test-cargo-vendor-contract test-deb-source-contract test-deb-package-contract-test test-legacy-system-assets test-locale-install-contract test-classify-changes test-arch-package-select check-workflow-policy

# The path filter that decides whether the packaging gates run on a pull
# request. A pattern that stops matching fails nothing: it reports every deb,
# rpm and Arch lane as skipped and the pull request goes green having built no
# package. Cheap enough to sit in `just check`; the git work is a dozen
# one-line commits in a temporary directory.
test-classify-changes:
    bash test/classify-changes-test.sh

# Prove the Arch lane installs the package makepkg built for pkgname=facelock
# and not the debug split it now emits beside it. Getting this wrong installs
# facelock-debug, and every assertion after it reports facelock as absent --
# nondeterministically, because the losing selection reads directory order
# (#212). Runs in `just check`: it is a few file names in a temporary directory,
# and the lane that would otherwise catch it is a full release build.
test-arch-package-select:
    bash test/arch-package-select-test.sh

# Prove every install path ships compiled gettext catalogs. Static checks run
# everywhere; the compile check needs gettext and skips without it, so that
# `just check` keeps working on a machine that has none.
test-locale-install-contract:
    bash test/locale-install-contract.sh

# Preserve the daemon's pre-install runtime state across source file replacement.
test-source-install-daemon-lifecycle:
    bash test/source-install-daemon-lifecycle.sh

# Exercise the source-install barrier against a real systemd and system bus.
test-source-install-daemon-lifecycle-systemd: _build-test-container
    test/run-source-install-daemon-lifecycle-systemd.sh facelock-pam-test

# Validate immutable system assets and migrate only exact historical /etc copies.
test-legacy-system-assets:
    bash test/legacy-system-assets.sh

# Prove the deterministic, exact Cargo source component used by Debian builds.
test-cargo-vendor-contract:
    bash test/cargo-vendor-contract.sh

# Static Debian source/metadata/release-consumer contract.
test-deb-source-contract:
    bash test/deb-source-contract.sh
    python3 test/deb-source-contract-test.py

# Validate every binary package named by one exact generated manifest.
test-deb-package-contract manifest:
    bash test/deb-package-contract.sh --manifest "{{ manifest }}"

# Exercise exact Debian manifest identity, checksum, and atomic-staging mutations.
test-deb-package-contract-test:
    bash test/deb-package-contract-test.sh

# Build the PAM test container image (uses host-built release binaries).
# Keep in sync with .github/workflows/ci.yml, which builds this same image

# directly rather than going through this recipe.
_build-test-container: build-release
    podman build -t facelock-pam-test -f test/Containerfile .

# Automated PAM smoke tests (Arch container)
test-arch-pam: _build-test-container
    podman run --rm facelock-pam-test
    test/run-arch-package-systemd.sh facelock-pam-test

# Automated state-layout test (Arch container, camera-free).
# Asserts the exact modes and ownership of everything under /var/lib/facelock
# and /var/log/facelock, including that any local user can traverse to its own
# enrollment marker but list nothing and read no secret. This is the only test
# that exercises the packaging wiring (install-files modes + the built-in

# defaults) end to end — unit tests cannot.
test-arch-layout: _build-test-container
    podman run --rm facelock-pam-test /run-layout-tests.sh

# The half of `test-arch-integration` and `test-arch-oneshot` that never opens
# a camera (#139): D-Bus authorization, the AuthAttempted broadcast's audience
# and payload, the rate-limit reply encoding, schema migrations, pre-flight
# exit codes, and the shape of the status document. CI runs this on every pull
# request, which is what the two camera-required tiers below cannot be.
#
# The ONNX models are still needed and are not a camera dependency: `facelock
# daemon` verifies them at startup and refuses to run without them, and half
# of what moved here is daemon-side authorization. The package tiers' opt-out
# is honoured — FACELOCK_ALLOW_MISSING_MODELS=1 downgrades the daemon block to
# a loud skip and runs only the one-shot half.
#

# Automated camera-free E2E tests (Arch container, no camera needed)
test-arch-camera-free: (_require-models "1") _build-test-container
    #!/usr/bin/env bash
    set -euo pipefail
    env_args=()
    if [ "${FACELOCK_ALLOW_MISSING_MODELS:-0}" = "1" ]; then
        env_args+=(-e "FACELOCK_ALLOW_MISSING_MODELS=1")
    fi
    podman run --rm "${env_args[@]}" facelock-pam-test /run-camera-free-tests.sh

# models/*.onnx is gitignored and never tracked, so a fresh clone — and every
# `git worktree add`, which is how this repo is normally worked in — starts
# with an empty models/, while the camera and package test tiers all need the
# two required models baked into the image. Run this once per checkout.
#
# Sources, cheapest first (or pass one: `just link-models /path/to/models`):
#   1. the main checkout's models/. A worktree normally lives inside the main
#      checkout, so this is the same filesystem and the link costs nothing.
#   2. /var/lib/facelock/models/, where `sudo facelock setup` puts them. The
#      models are 0644 under a 0755 dir behind a 0711 state dir, so reading
#      them by name needs no sudo and no group.
#
# Hardlink, never symlink: test/Containerfile does `COPY models/ /build/models/`
# and podman's COPY does not follow a symlink pointing outside the build
# context, so a symlinked models/ would build a clean-looking image with no
# models in it — the exact failure this recipe exists to prevent. Hardlinks
# fall back to a copy when the source is on another filesystem; /var/lib
# usually is, and btrfs refuses links across subvolumes even on one device.
#
# Every file is checked against the sha256 in models/manifest.toml before it
# lands, and lands via a temp name, so an interrupted copy cannot leave a
# truncated model behind for the daemon to reject later.
#

# Populate models/*.onnx from an existing checkout or install tree
link-models src="": (_link-models "explicit" src)

# mode=explicit (`just link-models`) — try every mechanism, report each file,
#   and fail if a required model is still missing at the end.
# mode=auto (dependency of _require-models) — hardlink only, say nothing when
#   there is nothing to do, and never fail. A copy can be 435MB; that is worth
#   opting into, not something `just test-arch-integration` should spend behind
#   your back. When auto cannot finish the job it stays quiet and leaves the

# diagnosis to _require-models, which owns that message.
_link-models mode="explicit" src="":
    #!/usr/bin/env bash
    set -euo pipefail

    if [ "{{ mode }}" = "explicit" ]; then explicit=1; else explicit=0; fi

    manifest=models/manifest.toml
    if [ ! -f "$manifest" ]; then
        echo "error: $manifest is missing — it is tracked, so this is not a fresh-checkout problem" >&2
        exit 1
    fi

    # filename <TAB> sha256 <TAB> required(1|0), read straight out of the
    # manifest so adding a model there does not need a second list updated here.
    models_meta="$(awk '
        /^\[\[models\]\]/ { if (fn != "") print fn "\t" sha "\t" req; fn = ""; sha = ""; req = 1 }
        $1 == "filename" { gsub(/"/, "", $3); fn = $3 }
        $1 == "sha256" { gsub(/"/, "", $3); sha = $3 }
        $1 == "optional" && $3 == "true" { req = 0 }
        END { if (fn != "") print fn "\t" sha "\t" req }
    ' "$manifest")"

    # need: missing and required — the set that decides which source is usable.
    # wanted: what we will actually place. auto only ever chases `need`.
    declare -A sha_of=()
    need=()
    optional_missing=()
    while IFS=$'\t' read -r fn sha req; do
        if [ -z "$fn" ]; then continue; fi
        sha_of["$fn"]="$sha"
        if [ -f "models/$fn" ]; then continue; fi
        if [ "$req" = "1" ]; then need+=("$fn"); else optional_missing+=("$fn"); fi
    done <<< "$models_meta"

    wanted=("${need[@]}")
    if [ "$explicit" = 1 ]; then wanted+=("${optional_missing[@]}"); fi

    if [ ${#wanted[@]} -eq 0 ]; then
        if [ "$explicit" = 1 ]; then echo "models/ already has every model in $manifest — nothing to do"; fi
        exit 0
    fi

    candidates=()
    if [ -n "{{ src }}" ]; then
        candidates+=("{{ src }}")
    else
        # `git worktree list` prints the main worktree first. That is the
        # checkout a worktree can hardlink from for free.
        main_checkout="$(git worktree list --porcelain 2>/dev/null | sed -n '1s/^worktree //p' || true)"
        if [ -n "$main_checkout" ] && [ "$(realpath -m "$main_checkout")" != "$(realpath .)" ]; then
            candidates+=("$main_checkout/models")
        fi
        candidates+=(/var/lib/facelock/models)
    fi

    # A usable source has every required model we are missing, and at least one
    # file we actually want (so we do not "pick" a source with nothing to give).
    source_dir=""
    for c in "${candidates[@]}"; do
        if [ ! -d "$c" ]; then continue; fi
        usable=1
        for fn in "${need[@]}"; do
            if [ ! -r "$c/$fn" ]; then usable=0; break; fi
        done
        if [ "$usable" = 0 ]; then continue; fi
        for fn in "${wanted[@]}"; do
            if [ -r "$c/$fn" ]; then source_dir="$c"; break; fi
        done
        if [ -n "$source_dir" ]; then break; fi
    done

    if [ -z "$source_dir" ]; then
        # auto: _require-models is about to say the same thing, better.
        if [ "$explicit" = 0 ]; then exit 0; fi
        if [ ${#need[@]} -eq 0 ]; then
            # Both required models are here and only the optional ones are not,
            # with nothing around that has them. The test tiers do not need
            # them, so this is a note, not a failure — and re-running stays a
            # no-op instead of turning into an error the second time.
            echo "models/ has both required models; no candidate source has the optional ones:"
            for fn in "${optional_missing[@]}"; do echo "  $fn"; done
            exit 0
        fi
        echo "error: found no source holding the required models that models/ is missing:" >&2
        for fn in "${need[@]}"; do echo "         $fn" >&2; done
        echo "       Looked in:" >&2
        for c in "${candidates[@]}"; do
            if [ -d "$c" ]; then
                echo "         $c (exists, but does not have them all — or is not readable)" >&2
            else
                echo "         $c (no such directory)" >&2
            fi
        done
        echo "       Download them once, then re-run this:" >&2
        echo "         sudo facelock setup      # downloads to /var/lib/facelock/models" >&2
        echo "       Or point at a directory that already has them:" >&2
        echo "         just link-models /path/to/models" >&2
        exit 1
    fi

    if [ "$explicit" = 1 ]; then echo "source: $source_dir"; fi

    # A partially written model is worse than a missing one: it satisfies the
    # `[ -f ]` guards and then fails sha256 verification inside the container.
    tmp=""
    cleanup() { if [ -n "$tmp" ]; then rm -f "$tmp"; fi; }
    trap cleanup EXIT

    linked=0
    copied=0
    copied_bytes=0
    for fn in "${wanted[@]}"; do
        if [ ! -r "$source_dir/$fn" ]; then continue; fi
        size="$(stat -c %s "$source_dir/$fn")"
        tmp="models/.$fn.tmp.$$"
        rm -f "$tmp"
        if [ "$explicit" = 1 ]; then
            printf '  %-24s %6s  ' "$fn" "$(numfmt --to=iec "$size")"
        fi
        if ln "$source_dir/$fn" "$tmp" 2>/dev/null; then
            method=hardlink
        elif [ "$explicit" = 1 ]; then
            # Another filesystem, or fs.protected_hardlinks refusing a link to a
            # file we do not own. Copying is what is left, and it is not free.
            printf 'copying (hardlink not possible)'
            cp -- "$source_dir/$fn" "$tmp"
            method=copy
        else
            rm -f "$tmp"
            tmp=""
            continue
        fi

        want_sha="${sha_of[$fn]}"
        got_sha="$(sha256sum "$tmp" | cut -d' ' -f1)"
        if [ "$want_sha" != "$got_sha" ]; then
            rm -f "$tmp"
            tmp=""
            if [ "$explicit" = 1 ]; then printf '\n'; fi
            echo "warning: $source_dir/$fn does not match its sha256 in $manifest — skipped" >&2
            echo "           expected $want_sha" >&2
            echo "           got      $got_sha" >&2
            continue
        fi

        mv -f "$tmp" "models/$fn"
        tmp=""
        if [ "$method" = hardlink ]; then
            linked=$((linked + 1))
            if [ "$explicit" = 1 ]; then printf 'hardlink\n'; fi
        else
            copied=$((copied + 1))
            copied_bytes=$((copied_bytes + size))
            printf ' done\n'
        fi
    done

    still_missing=()
    for fn in "${need[@]}"; do
        if [ ! -f "models/$fn" ]; then still_missing+=("$fn"); fi
    done
    if [ ${#still_missing[@]} -gt 0 ]; then
        if [ "$explicit" = 0 ]; then exit 0; fi
        echo "error: models/ is still missing a required model after linking from $source_dir:" >&2
        for fn in "${still_missing[@]}"; do echo "         $fn" >&2; done
        exit 1
    fi

    if [ "$explicit" = 1 ]; then
        echo "models/ has both required models — $linked hardlinked (no extra disk), $copied copied ($(numfmt --to=iec "$copied_bytes"))"
        left_out=()
        for fn in "${optional_missing[@]}"; do
            if [ ! -f "models/$fn" ]; then left_out+=("$fn"); fi
        done
        if [ ${#left_out[@]} -gt 0 ]; then
            echo "optional models not at $source_dir (no test tier needs them): ${left_out[*]}"
        fi
    elif [ "$linked" -gt 0 ]; then
        # Never silent: hardlinks cost nothing but they are still a change to
        # the working tree, made on the way to a test the caller asked for.
        echo "hardlinked $linked model(s) into models/ from $source_dir (just link-models)"
    fi

# Guard for every tier that needs a daemon which can actually load the face
# engine: the camera tiers (the Containerfile bakes models/ into the image with
# a tolerant `|| true`, so a checkout without the ONNX models produces an image
# whose daemon cannot load the engine and whose enroll bails before opening the
# camera) and the package tiers (pkg-validate.sh starts the daemon under the
# hardened unit and walks /proc/<pid>/task/* for CAP_CHOWN — no models, no
# daemon, no assertion). Fail loudly here instead of debugging opaque test
# FAILs, or worse, reading a green summary that skipped the interesting half.
# Check the two non-optional models by name: a checkout holding only the
# optional ones (det_10g.onnx, glintr100.onnx) satisfies a bare *.onnx glob but
# still leaves the default detector/embedder missing at runtime.
#
# `allow_opt_out=1` (package tiers only) lets FACELOCK_ALLOW_MISSING_MODELS=1
# turn the refusal into a warning: those tiers still validate packaging without
# models, and pkg-validate.sh then *counts* what it skipped. The camera tiers
# pass "0" — without models they have nothing left to test.
#
# The _link-models dependency makes the common case not happen at all: a fresh
# worktree hardlinks from the main checkout on the way in, so neither the
# refusal nor the opt-out is reached. It only ever uses free mechanisms, so when

# it declines, everything below still applies unchanged.
_require-models allow_opt_out="0": (_link-models "auto")
    #!/usr/bin/env bash
    set -euo pipefail
    missing=()
    for m in models/scrfd_2.5g_bnkps.onnx models/w600k_r50.onnx; do
        [ -f "$m" ] || missing+=("$m")
    done
    [ ${#missing[@]} -gt 0 ] || exit 0
    if [ "{{ allow_opt_out }}" = "1" ] && [ "${FACELOCK_ALLOW_MISSING_MODELS:-0}" = "1" ]; then
        echo "warning: missing ONNX models, continuing (FACELOCK_ALLOW_MISSING_MODELS=1):" >&2
        for m in "${missing[@]}"; do echo "           $m" >&2; done
        echo "         The daemon-start assertions will be reported as SKIPPED, not passed," >&2
        echo "         and the run cannot produce release evidence: its lane record is" >&2
        echo "         refused by 'just test-packaging-matrix' and 'just release-preflight'." >&2
        exit 0
    fi
    echo "error: missing required ONNX models — this test tier needs them baked into the image:" >&2
    for m in "${missing[@]}"; do echo "         $m" >&2; done
    echo "       They are downloaded, not tracked. Populate models/ from a checkout" >&2
    echo "       or install tree that already has them:" >&2
    echo "         just link-models" >&2
    echo "       It names what it looked at, and how to get the models, if it finds" >&2
    echo "       no source. (models/*.onnx is gitignored, so nothing you link in can" >&2
    echo "       be committed by accident.)" >&2
    if [ "{{ allow_opt_out }}" = "1" ]; then
        echo "       To validate packaging only, with the daemon-start assertions counted" >&2
        echo "       as skipped (a diagnostic run, never release evidence):" >&2
        echo "         FACELOCK_ALLOW_MISSING_MODELS=1 just <recipe>" >&2
    fi
    exit 1

# Automated daemon integration tests (Arch, requires camera)
test-arch-integration: _require-models _build-test-container
    #!/usr/bin/env bash
    set -euo pipefail
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    env_args=()
    # run-integration-tests.sh reads FACELOCK_LIVE_TIMEOUT *inside* the
    # container, so without -e a caller's value is silently ignored and every
    # live step keeps the stock 90s — the one knob that helps when a human has
    # to sit still in frame. The value is interpolated straight into
    # `timeout --foreground`, so it must be a timeout(1) duration.
    if [ -n "${FACELOCK_LIVE_TIMEOUT:-}" ]; then
        if [[ ! "$FACELOCK_LIVE_TIMEOUT" =~ ^[0-9]+(\.[0-9]+)?[smhd]?$ ]]; then
            echo "error: FACELOCK_LIVE_TIMEOUT='$FACELOCK_LIVE_TIMEOUT' is not a timeout(1) duration (e.g. 300s, 5m)" >&2
            exit 1
        fi
        env_args+=(-e "FACELOCK_LIVE_TIMEOUT=$FACELOCK_LIVE_TIMEOUT")
        echo "live steps time out after $FACELOCK_LIVE_TIMEOUT (default 90s)"
    fi
    podman run --rm $devices "${env_args[@]}" facelock-pam-test /run-integration-tests.sh

# FACELOCK_LIVE_TIMEOUT=300s just test-arch-oneshot      # relax the live steps

# Automated oneshot (daemonless) integration tests (Arch, requires camera)
test-arch-oneshot: _require-models _build-test-container
    #!/usr/bin/env bash
    set -euo pipefail
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    env_args=()
    # As in test-arch-integration: the timeout is read inside the container, so
    # it has to be forwarded or the caller's value does nothing.
    if [ -n "${FACELOCK_LIVE_TIMEOUT:-}" ]; then
        if [[ ! "$FACELOCK_LIVE_TIMEOUT" =~ ^[0-9]+(\.[0-9]+)?[smhd]?$ ]]; then
            echo "error: FACELOCK_LIVE_TIMEOUT='$FACELOCK_LIVE_TIMEOUT' is not a timeout(1) duration (e.g. 300s, 5m)" >&2
            exit 1
        fi
        env_args+=(-e "FACELOCK_LIVE_TIMEOUT=$FACELOCK_LIVE_TIMEOUT")
        echo "live steps time out after $FACELOCK_LIVE_TIMEOUT (default 90s)"
    fi
    podman run --rm $devices "${env_args[@]}" facelock-pam-test /run-oneshot-tests.sh

# Refuse to record a hardware run against a tree that is not what will ship.
_require-clean-tree:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "$(git status --porcelain)" ]; then
        echo "error: the working tree is dirty, so a recorded run would name a commit" >&2
        echo "       that is not what was tested. Commit first, then re-run." >&2
        git status --short >&2
        exit 1
    fi

# `just release-preflight` refuses to pass without that record (#139). These
# two tiers are the only automated evidence that face authentication works end
# to end — real D-Bus activation, the real PAM stack, real capture, the
# one-shot fallback — and nothing else runs them, which is how three of their
# assertions rotted undetected. They need a camera and someone sitting in
# front of it, so this is the one gate a human has to perform.
#
# FACELOCK_LIVE_TIMEOUT is forwarded by each tier; relax it here too when the
# stock 90s per live step is not enough time to get into frame.
#

# Both camera-required E2E tiers, recorded for release-preflight (requires camera)
test-arch-camera-required: _require-clean-tree test-arch-integration test-arch-oneshot
    #!/usr/bin/env bash
    set -euo pipefail
    commit="$(git rev-parse HEAD)"
    if [ -n "$(git status --porcelain)" ]; then
        echo "error: the tree changed while the tiers ran; not recording." >&2
        exit 1
    fi
    printf '%s\n' "$commit" > .hardware-tiers-verified
    echo ""
    echo "Recorded: camera-required tiers passed at $commit"
    echo "'just release-preflight' accepts this until HEAD moves."

# Dev shell — interactive Arch container with host models for fast iteration (requires camera)
test-arch-dev-shell: _build-test-container
    #!/usr/bin/env bash
    set -euo pipefail
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts=""
    for f in /var/lib/facelock/models/*.onnx /var/lib/facelock/models/*.toml; do
        [ -f "$f" ] && mounts="$mounts -v $f:/tmp/host-models/$(basename $f):ro"
    done
    echo "Starting dev shell (Arch, binary install, host models). Try:"
    echo "  facelock daemon &"
    echo "  sleep 2"
    echo "  facelock enroll --user testuser --label myface"
    echo "  facelock test --user testuser"
    echo "  pamtester facelock-test testuser authenticate"
    podman run --rm -it $devices $mounts facelock-pam-test \
        bash -c "cp /tmp/host-models/* /var/lib/facelock/models/ 2>/dev/null; exec bash"

# Release shell — clean-room Arch container, real user experience (requires camera)
test-arch-release-shell: _build-test-container
    #!/usr/bin/env bash
    set -euo pipefail
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    echo "Starting release shell (Arch, binary install, clean room). Try:"
    echo "  facelock setup"
    echo "  facelock enroll --user testuser --label myface"
    echo "  facelock test --user testuser"
    echo "  pamtester facelock-test testuser authenticate"
    podman run --rm -it $devices facelock-pam-test /bin/bash

# Needs no models: nothing in this tier starts the daemon or runs inference, so
# it is the one packaging gate that runs unattended in CI as it stands. It is
# also the slow one — the recipe compiles the workspace twice, release for
# build() and debug for check().

# Package test — build the real dist/PKGBUILD with makepkg, install it with pacman, validate
test-arch-pkg:
    #!/usr/bin/env bash
    set -euo pipefail
    staging="$(mktemp -d "${TMPDIR:-/tmp}/facelock-arch-pkg.XXXXXX")"
    # A fixed image tag makes two concurrent runs clobber each other, and the
    # loser fails in ways that read as a code defect. Take the uniqueness
    # mktemp already produced. Layer caching is keyed by content, not by tag,
    # so a fresh tag costs nothing but the final commit.
    image="facelock-arch-pkg-$(basename "$staging" | cut -d. -f2 | tr '[:upper:]' '[:lower:]')"
    trap 'rm -rf -- "$staging"; podman rmi -f "$image" >/dev/null 2>&1 || true' EXIT
    test/build-arch-package-image.sh "$image" "$staging"
    lane_status=0
    podman run --rm -v "$staging/source:/staged-source:ro,Z" "$image" | tee "$staging/lane.log" || lane_status=$?
    # The lane's packaging-evidence record, from the validator's RESULTS_JSON
    # line; see the Debian suite gates for what the claims mean.
    python3 test/packaging-evidence.py record \
        --lane 'test-arch-pkg target=arch channel=aur build_origin=makepkg-source-build runtime_policy=system-ort depth=full' \
        --results-log "$staging/lane.log" --exit-status "$lane_status"
    exit "$lane_status"

# Build release and install to system

# Run as: just install (builds as you, installs as root)
install: build-release
    /usr/bin/sudo /usr/bin/env PATH=/usr/bin:/bin /usr/bin/just install-files

# Install pre-built binaries to system (requires root, no build)
install-files:
    #!/usr/bin/bash -p
    set -euo pipefail
    PATH=/usr/bin:/bin
    export PATH

    # Verify binaries exist
    for f in target/release/facelock target/release/libpam_facelock.so; do
        [ -f "$f" ] || { echo "Error: $f not found. Run 'just build-release' first."; exit 1; }
    done

    # Keep a live daemon available across source upgrades without changing
    # enablement or activating one that was inactive or only D-Bus-activatable.
    source scripts/source-install-daemon-lifecycle.sh
    facelock_source_install_begin

    # Binaries
    install -Dm755 target/release/facelock /usr/bin/facelock
    install -Dm755 target/release/libpam_facelock.so /lib/security/pam_facelock.so

    # Config (don't overwrite existing)
    install -Dm644 config/facelock.toml /etc/facelock/config.toml.default
    [ -f /etc/facelock/config.toml ] || cp /etc/facelock/config.toml.default /etc/facelock/config.toml

    # Hardware quirks database
    install -dm755 /usr/share/facelock/quirks.d
    install -Dm644 config/quirks.d/*.toml /usr/share/facelock/quirks.d/

    # Compiled translations, both gettext domains. Compiled straight from po/
    # rather than from a target/locale a previous `just mo` may or may not have
    # produced. gettext stays optional for a source install (see the
    # localization section below), so a machine without msgfmt gets the
    # compiled-in English rather than a failed install.
    # `bash -p` because this recipe's own `-p` does not reach a child started
    # from its shebang: that child would read an inherited BASH_ENV as root.
    if command -v msgfmt >/dev/null; then
        bash -p scripts/install-locale-catalogs.sh /usr/share/locale
    else
        echo "note: msgfmt not found; skipping translation catalogs (English is compiled in)"
    fi

    # systemd unit
    install -Dm644 systemd/facelock-daemon.service /usr/lib/systemd/system/facelock-daemon.service

    # D-Bus policy and activation
    install -Dm644 dbus/org.facelock.Daemon.conf /usr/share/dbus-1/system.d/org.facelock.Daemon.conf
    install -Dm644 dbus/org.facelock.Daemon.service /usr/share/dbus-1/system-services/org.facelock.Daemon.service
    # Retire only byte-exact historical Facelock copies while the source-install
    # activation barrier is effective. The lifecycle records the fixed mutation
    # identities before the first install write and proves the final state.
    facelock_source_install_stage_and_record_legacy_migration /

    # Polkit agent binary (optional, do NOT install autostart — agent is not production-ready
    # and will steal polkit auth from the DE's agent, causing all privilege prompts to hang)
    [ -f target/release/facelock-polkit-agent ] && install -Dm755 target/release/facelock-polkit-agent /usr/bin/facelock-polkit-agent || true

    # Directories. Must match dist/facelock.tmpfiles.
    # State dir 0711 root:root: traversable by every local user, listable by
    # root only (ADR 010). Models are public, SHA256-verified downloads.
    # enrolled/ 0711 root:root: a user can open its own 0600 marker by name
    # but cannot list who else is enrolled. Audit log and snapshots are
    # root-only (per-user auth history and raw face images).
    install -dm711 -o root -g root /var/lib/facelock
    install -dm755 -o root -g root /var/lib/facelock/models
    install -dm711 -o root -g root /var/lib/facelock/enrolled
    install -dm700 -o root -g root /var/lib/facelock/pam-backups
    install -dm700 -o root -g root /var/log/facelock
    install -dm700 -o root -g root /var/log/facelock/snapshots

    # Fix permissions on existing data
    [ -d /etc/facelock ] && chown root:root /etc/facelock && chmod 755 /etc/facelock || true
    [ -f /etc/facelock/config.toml ] && chown root:root /etc/facelock/config.toml && chmod 644 /etc/facelock/config.toml || true
    [ -f /etc/facelock/config.toml.default ] && chown root:root /etc/facelock/config.toml.default && chmod 644 /etc/facelock/config.toml.default || true
    [ -d /var/lib/facelock ] && chown root:root /var/lib/facelock && chmod 711 /var/lib/facelock || true
    [ -d /var/lib/facelock/models ] && chown root:root /var/lib/facelock/models && chmod 755 /var/lib/facelock/models || true
    [ -d /var/lib/facelock/enrolled ] && chown root:root /var/lib/facelock/enrolled && chmod 711 /var/lib/facelock/enrolled || true
    [ -d /var/lib/facelock/pam-backups ] && chown root:root /var/lib/facelock/pam-backups && chmod 700 /var/lib/facelock/pam-backups || true
    [ -d /var/log/facelock ] && chown root:root /var/log/facelock && chmod 700 /var/log/facelock || true
    [ -d /var/log/facelock/snapshots ] && chown root:root /var/log/facelock/snapshots && chmod 700 /var/log/facelock/snapshots || true
    [ -f /var/log/facelock/audit.jsonl ] && chown root:root /var/log/facelock/audit.jsonl && chmod 600 /var/log/facelock/audit.jsonl || true
    [ -d /run/facelock ] && chown root:root /run/facelock 2>/dev/null || true
    [ -d /var/lib/facelock/models ] && chmod 644 /var/lib/facelock/models/*.onnx 2>/dev/null || true
    # The database and sidecars are root-only: encrypted biometric templates,
    # read by the daemon. Tighten if present, never create.
    [ -f /var/lib/facelock/facelock.db ] && chown root:root /var/lib/facelock/facelock.db && chmod 600 /var/lib/facelock/facelock.db || true
    [ -f /var/lib/facelock/facelock.db-wal ] && chown root:root /var/lib/facelock/facelock.db-wal && chmod 600 /var/lib/facelock/facelock.db-wal || true
    [ -f /var/lib/facelock/facelock.db-shm ] && chown root:root /var/lib/facelock/facelock.db-shm && chmod 600 /var/lib/facelock/facelock.db-shm || true

    # ADR 010 retired the facelock group: nothing is group-owned any more, so
    # remove a group an older install created. Best-effort.
    if getent group facelock >/dev/null 2>&1; then
        groupdel facelock 2>/dev/null || true
    fi

    # Reload the replaced activation assets and restore only the runtime state
    # captured before the protected write interval.
    facelock_source_install_complete
    if [ -d /run/systemd/system ]; then
        echo "D-Bus activation enabled."
    fi

    echo ""
    echo ""

    # Check what's still needed
    NEEDS_SETUP=false
    NEEDS_ORT=false

    # Models present?
    if ! ls /var/lib/facelock/models/*.onnx >/dev/null 2>&1; then
        NEEDS_SETUP=true
    fi

    # Config present?
    if [ ! -f /etc/facelock/config.toml ]; then
        NEEDS_SETUP=true
    fi

    # PAM configured?
    if ! grep -qs pam_facelock /etc/pam.d/sudo 2>/dev/null; then
        NEEDS_SETUP=true
    fi

    # ORT installed? Check file paths directly.
    if [ ! -f /usr/lib/libonnxruntime.so ] && \
       [ ! -f /usr/lib64/libonnxruntime.so ] && \
       [ ! -f /usr/lib/facelock/libonnxruntime.so ]; then
        NEEDS_ORT=true
    fi

    if $NEEDS_SETUP || $NEEDS_ORT; then
        echo "Installed."
        if $NEEDS_ORT; then
            echo ""
            echo "Requires: onnxruntime (pacman -S onnxruntime-cpu)"
            echo "Optional: onnxruntime-opt-cuda (NVIDIA) or onnxruntime-opt-rocm (AMD)"
        fi
        if $NEEDS_SETUP; then
            echo ""
            echo "Run 'sudo facelock setup' to complete configuration."
            echo "  (downloads models, configures PAM services, enrolls your face)"
        fi
    else
        echo "Installed and up to date."
    fi

# Uninstall from system

# Run as: just uninstall (elevates to root with a trusted command path)
uninstall:
    /usr/bin/sudo /usr/bin/env PATH=/usr/bin:/bin /usr/bin/just uninstall-files

# Uninstall files from system (requires root, called by uninstall)
uninstall-files:
    #!/usr/bin/bash -p
    set -euo pipefail
    PATH=/usr/bin:/bin
    export PATH
    # Stop and disable daemon
    systemctl stop facelock-daemon.service 2>/dev/null || true
    systemctl disable facelock-daemon.service 2>/dev/null || true

    # The binary still exists here, so a failed final scan stops before either
    # it or the PAM module is removed.
    facelock pam remove --all

    # Kill facelock polkit agent if running (so the DE's agent can take over)
    pkill -f facelock-polkit-agent 2>/dev/null || true

    # Remove binaries and units
    rm -f /usr/bin/facelock /lib/security/pam_facelock.so
    # Remove only canonical /usr system assets. Historical /etc copies are
    # administrator state and the reviewed migration is their sole owner.
    scripts/uninstall-system-assets.sh /
    rm -f /usr/bin/facelock-polkit-agent
    rm -f /etc/xdg/autostart/org.facelock.AuthAgent.desktop

    # Remove quirks database and source-install config artifacts
    # (these would otherwise collide with a subsequent package install)
    rm -rf /usr/share/facelock
    rm -f /etc/facelock/config.toml.default

    # Remove installed translation catalogs (ours only)
    rm -f /usr/share/locale/*/LC_MESSAGES/facelock.mo /usr/share/locale/*/LC_MESSAGES/pam_facelock.mo

    systemctl daemon-reload 2>/dev/null || true

    # ADR 010 retired the facelock group: nothing is group-owned any more, so
    # remove a group an older install created. Best-effort.
    if getent group facelock >/dev/null 2>&1; then
        groupdel facelock 2>/dev/null || true
    fi

    echo ""
    echo "==> facelock uninstalled. User data preserved at:"
    echo "==>   /etc/facelock/      (config.toml, encryption.key.sealed, setup markers)"
    echo "==>   /var/lib/facelock/  (face database, ONNX models ~100MB)"
    echo "==>   /var/log/facelock/  (audit logs and snapshots)"
    echo "==>"
    echo "==> Retained state cleanup is intentionally not automated."
    echo "==> Cleanup must stay within the fixed roots above, leave configured external paths untouched, and refuse links or mount crossings."
    echo "==> Filesystem deletion does not securely erase SSDs, snapshots, or backups."

# ---------------------------------------------------------------------------
# Localization (optional tooling)
#
# gettext is NOT required to build, test, or install facelock — English is
# compiled in as the fallback. These recipes exist for translators and fail
# with a clear message when the gettext tools are absent.
# ---------------------------------------------------------------------------
# Regenerate translation templates (po/*.pot) from source. The CLI catalog
# extracts every `translate("...")` literal in the message seam
# (crates/facelock-cli/src/message/ — the one place CLI user-facing English
# lives, one module per domain); the PAM catalog extracts `gettext("...")`
# from pam-facelock.
# xgettext has no Rust mode, but --language=C tokenizes these files correctly
# because the seam keeps msgids as single-line plain literals (see the
# "Adding a message" pattern in message/mod.rs). The domain modules are
# globbed and sorted so a new one is picked up without editing this recipe

# and the output stays byte-stable.
pot:
    #!/usr/bin/env bash
    set -euo pipefail
    for tool in xgettext msgen msgfmt; do
        if ! command -v "$tool" >/dev/null; then
            echo "error: $tool not found — install the gettext package." >&2
            echo "       (Translations are optional; building facelock does not need this.)" >&2
            exit 1
        fi
    done
    mapfile -t seam < <(ls crates/facelock-cli/src/message/*.rs | LC_ALL=C sort)
    xgettext --language=C --keyword=translate --from-code=UTF-8 --no-wrap \
        --package-name=facelock --copyright-holder="Facelock Contributors" \
        -o po/facelock.pot "${seam[@]}"

    # Mark the templates that carry `{placeholder}` tokens as brace-format, so
    # `msgfmt --check` (see the `mo` recipe) rejects a translation that drops,
    # renames or invents one. Without a format flag msgfmt validates nothing
    # about the braces: "{pathh}" compiles clean and renders broken at runtime.
    #
    # This is done here rather than with `--flag=translate:1:python-brace-format`
    # because xgettext honours format flags only for the format types its
    # *scanner language* knows, and --language=C knows only c-format — the flag
    # is accepted and silently dropped (verified against gettext 1.0). The seam
    # uses exactly one placeholder syntax (`{lower_snake}`, see `fill` in
    # message/mod.rs), so matching it here is precise, and the two checks below
    # confirm the flags and the placeholders agree in both directions.
    #
    # An entry's msgid can span several physical lines: gettext splits a
    # template at every embedded newline into a bare `msgid ""` opener plus one
    # continuation string per line. The scan therefore reassembles each msgid
    # before matching — testing the opener alone silently skips every
    # multi-line template, which is what it did until #186.
    awk '
        { buf[NR] = $0 }
        END {
            for (i = 2; i <= NR; i++)
                if (buf[i] ~ /^msgid "/) {
                    text = buf[i]
                    for (j = i + 1; j <= NR && buf[j] ~ /^"/; j++) text = text buf[j]
                    if (text ~ /\{[a-z_][a-z0-9_]*\}/) {
                        if (buf[i-1] ~ /^#,/) sub(/$/, ", python-brace-format", buf[i-1])
                        else buf[i-1] = buf[i-1] "\n#, python-brace-format"
                    }
                }
            for (i = 1; i <= NR; i++) print buf[i]
        }
    ' po/facelock.pot > po/facelock.pot.tmp
    mv po/facelock.pot.tmp po/facelock.pot

    # Every flagged msgid must itself parse as a brace-format string: fill the
    # template with English and run the same check translators will hit. A msgid
    # with an unbalanced brace in prose would fail here rather than in a
    # translator's catalog.
    msgen po/facelock.pot | msgfmt --check-format -o /dev/null -

    # That check is one-directional: it proves the flags we wrote are truthful,
    # never that we wrote them all, and a missing flag is silent — the
    # placeholders just go unvalidated forever. Assert the other direction
    # here, streaming the entries rather than reusing the pass above so a bug
    # in one is not a bug in both.
    awk '
        BEGIN      { bad = 0 }
        /^$/       { flagged = 0 }
        /^#,/      { flagged = ($0 ~ /python-brace-format/) }
        /^msgid "/ { in_msgid = 1; braced = 0; start = NR }
        /^msgstr/  {
            if (in_msgid && braced && !flagged) {
                printf "error: msgid at line %d carries a {placeholder} but no python-brace-format flag\n", start > "/dev/stderr"
                bad = 1
            }
            in_msgid = 0
        }
        in_msgid && /\{[a-z_][a-z0-9_]*\}/ { braced = 1 }
        END { exit bad }
    ' po/facelock.pot

    xgettext --language=C --keyword=gettext --from-code=UTF-8 --no-wrap \
        --package-name=pam_facelock --copyright-holder="Facelock Contributors" \
        -o po/pam_facelock.pot crates/pam-facelock/src/lib.rs
    echo "Regenerated po/facelock.pot and po/pam_facelock.pot"

# Compile translations po/<lang>/{facelock,pam_facelock}.po into
# target/locale/<lang>/LC_MESSAGES/<domain>.mo, for verifying a translation
# before it is installed anywhere. To start a new translation:
#   mkdir -p po/de && msginit -i po/facelock.pot -o po/de/facelock.po -l de
# To verify it manually without installing system-wide:
#   just mo && FACELOCK_LOCALEDIR=$PWD/target/locale LANGUAGE=de facelock list
#
# Installing no longer depends on this: every install path — the packages and
# `just install-files` alike — compiles po/ itself through
# scripts/install-locale-catalogs.sh, which is also what this recipe runs.
#
# `msgfmt --check` is what enforces the `{placeholder}` contract: paired with the
# python-brace-format flags `just pot` writes, it rejects a translation that
# typos, drops or invents a placeholder. Dropping --check would silently turn

# that back off.
mo:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v msgfmt >/dev/null; then
        echo "error: msgfmt not found — install the gettext package." >&2
        echo "       (Translations are optional; building facelock does not need this.)" >&2
        exit 1
    fi
    # Always present, so FACELOCK_LOCALEDIR=$PWD/target/locale is a valid
    # override even before the first translation lands.
    mkdir -p target/locale
    scripts/install-locale-catalogs.sh target/locale

# Bump version and prepare a release commit + tag

# Usage: just release 0.2.0
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    source scripts/release-versions.sh

    # Validate version format
    release_validate_cargo_version "$VERSION"

    # Check for clean working tree
    if [ -n "$(git status --porcelain)" ]; then
        echo "Error: Working tree is not clean. Commit or stash changes first."
        exit 1
    fi

    OLD_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    release_validate_transition "$OLD_VERSION" "$VERSION"
    ARCH_VERSION=$(release_arch_pkgver "$VERSION")
    ARCH_RELEASE=$(release_next_arch_revision "$VERSION" dist/PKGBUILD)
    DEBIAN_RELEASE=$(release_next_debian_revision "$VERSION" debian/changelog)
    DEBIAN_VERSION=$(release_debian_common_version "$VERSION" "$DEBIAN_RELEASE")
    RPM_VERSION=$(release_rpm_version "$VERSION")
    RPM_COUNTER=$(release_next_rpm_counter "$VERSION" dist/facelock.spec)
    RPM_RELEASE=$(release_rpm_release "$VERSION" "$RPM_COUNTER")
    echo "Bumping version: $OLD_VERSION → $VERSION"

    # 1. Cargo.toml (workspace version)
    sed -i "s/^version = \"$OLD_VERSION\"/version = \"$VERSION\"/" Cargo.toml
    echo "  ✓ Cargo.toml"

    # 2. dist/PKGBUILD
    if [ -f dist/PKGBUILD ]; then
        sed -i "s/^_tag=.*/_tag=$VERSION/; s/^pkgver=.*/pkgver=$ARCH_VERSION/; s/^pkgrel=.*/pkgrel=$ARCH_RELEASE/" dist/PKGBUILD
        echo "  ✓ dist/PKGBUILD"
    fi

    # 2b. dist/PKGBUILD-bin (per-binary sha256sums are filled in by CI)
    if [ -f dist/PKGBUILD-bin ]; then
        sed -i "s/^_tag=.*/_tag=$VERSION/; s/^pkgver=.*/pkgver=$ARCH_VERSION/; s/^pkgrel=.*/pkgrel=$ARCH_RELEASE/" dist/PKGBUILD-bin
        echo "  ✓ dist/PKGBUILD-bin"
    fi

    # 2c. dist/PKGBUILD-git: the runtime pkgver() function computes the real
    # version from `git describe`, but generate_srcinfo in publish-aur.sh runs
    # without a git checkout, so AUR's web display falls back to this static
    # pkgver. Keep it in sync with the release so the AUR page doesn't drift.
    if [ -f dist/PKGBUILD-git ]; then
        sed -i "s/^pkgver=.*/pkgver=$ARCH_VERSION/" dist/PKGBUILD-git
        echo "  ✓ dist/PKGBUILD-git"
    fi

    # 3. dist/facelock.spec
    if [ -f dist/facelock.spec ]; then
        sed -i "s/^Version:.*/Version:        $RPM_VERSION/; s/^Release:.*/Release:        $RPM_RELEASE%{?dist}/" dist/facelock.spec
        echo "  ✓ dist/facelock.spec"
    fi

    # 4. debian/changelog (prepend new entry)
    if [ -f debian/changelog ]; then
        DATE=$(date -R)
        sed -i "1i facelock ($DEBIAN_VERSION) unstable; urgency=medium\n\n  * Release v$VERSION.\n\n -- Ty Smith <ty@tysmith.me>  $DATE\n" debian/changelog
        echo "  ✓ debian/changelog"
    fi

    # 5. Verify it compiles
    echo ""
    echo "Running cargo check..."
    cargo check --workspace
    echo "  ✓ cargo check passed"

    # 6. Remind to update CHANGELOG.md
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Update CHANGELOG.md with the changes for v$VERSION"
    echo "  then run:"
    echo ""
    echo "    git add -A && git commit -m 'chore: release v$VERSION'"
    echo "    git tag v$VERSION"
    echo "    git push origin main --tags"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Show current version
version:
    @grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/'

# Clean build artifacts
clean:
    cargo clean

# Show installed file locations
show-paths:
    @echo "Binary:   /usr/bin/facelock"
    @echo "PAM:      /lib/security/pam_facelock.so"
    @echo "Config:   /etc/facelock/config.toml"
    @echo "Models:   /var/lib/facelock/models/"
    @echo "Database: /var/lib/facelock/facelock.db"
    @echo "D-Bus:    /usr/share/dbus-1/system.d/org.facelock.Daemon.conf"
    @echo "Service:  /usr/lib/systemd/system/facelock-daemon.service"
    @echo "Logs:     /var/log/facelock/"

# Detect host ONNX Runtime version for container builds.
# Local binaries are built against the host ORT, so bundled ORT must match.
# CI uses 1.20.1 (set in release.yml); local builds use whatever is installed.

[private]
_ort-version := `for ort in /usr/lib/libonnxruntime.so /usr/lib64/libonnxruntime.so; do if [ -e "$ort" ]; then readlink -f "$ort" | grep -oP '\d+\.\d+\.\d+$'; exit; fi; done; echo "1.20.1"`

# Every Fedora recipe below takes a release and resolves its base image through
# test/fedora-lane-image.sh, which reads the digest pin out of
# dist/release-matrix.json and refuses a release that is undeclared, that is not
# a release target (Rawhide), or that has passed its EOL gate. The default stays
# 44 so bare invocations keep their old meaning. Image tags carry the release so
# concurrent lanes cannot overwrite each other's image.
#
# Resolution runs as the first dependency so an expired or undeclared release is
# refused before a host release build, not after one.

[private]
_fedora-lane-image release:
    @bash test/fedora-lane-image.sh '{{ release }}' >/dev/null

# The Fedora lanes stage host-built binaries into the image: build-rpm-prebuilt.sh
# no-ops the spec's cargo lines and tars target/release/ into the source. So the
# lane needs those binaries present, not necessarily built by this machine.
#
# That distinction matters because `just build-release` cannot run everywhere.
# Its second command builds facelock-cli with the tpm feature, and tss-esapi-sys
# now demands tss2 4.1.3 or newer; Ubuntu 24.04 ships 4.0.1, so the build panics
# on the GitHub runner (#229). CI builds them in the same digest-pinned Arch
# container ci.yml uses, and says so with FACELOCK_RELEASE_BINARIES_PREBUILT=1.
#
# The declaration is checked, not trusted: a missing binary is an error, never a
# quietly skipped stage. Without it, this builds exactly as before.

[private]
_require-release-binaries:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "${FACELOCK_RELEASE_BINARIES_PREBUILT:-0}" != "1" ]; then
        exec {{ just_executable() }} build-release
    fi
    missing=()
    for binary in target/release/facelock target/release/facelock-polkit-agent target/release/libpam_facelock.so; do
        [ -f "$binary" ] || missing+=("$binary")
    done
    if [ ${#missing[@]} -gt 0 ]; then
        echo "error: FACELOCK_RELEASE_BINARIES_PREBUILT=1 names binaries that are not here:" >&2
        printf '         %s\n' "${missing[@]}" >&2
        echo "       Either stage them first or unset the variable and let this build them." >&2
        exit 1
    fi
    echo "staging pre-built release binaries (FACELOCK_RELEASE_BINARIES_PREBUILT=1)"

# Test RPM packaging in Fedora container
test-rpm release="44": (_fedora-lane-image release) _require-release-binaries
    #!/usr/bin/env bash
    set -euo pipefail
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" \
        -t facelock-rpm-test-f{{ release }} -f test/Containerfile.fedora .
    podman run --rm facelock-rpm-test-f{{ release }}

# The upgrade fixture is the released v0.1.4 fc44 RPM, the only build of the
# retired-profile package that exists. Releases other than 44 are unproven here:
# an fc44 artifact can fail to install on an older Fedora over a newer glibc or
# library requirement, and no lane has run 43 or 45 through this recipe.

# Static and booted, model-free Fedora authselect retirement lifecycle
test-rpm-authselect release="44": (_fedora-lane-image release)
    #!/usr/bin/env bash
    set -euo pipefail
    bash test/rpm-authselect-contract.sh
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" \
        -t facelock-rpm-authselect-test-f{{ release }} -f test/Containerfile.rpm-authselect .
    bash test/run-rpm-authselect-systemd.sh facelock-rpm-authselect-test-f{{ release }}

# Run both exact supported-suite Debian package gates.
test-deb: test-deb-trixie-pkg test-deb-resolute-pkg

# Needs models/*.onnx: the validation starts the daemon under the hardened unit
# and checks what it holds at runtime. FACELOCK_ALLOW_MISSING_MODELS=1 runs the
# packaging half only, with the rest counted as skipped.
#
# PACKAGING_LANE names the lane and what it claims -- target, channel, how the
# package was built, whose ONNX Runtime it ships, and its lifecycle depth -- and
# the runner writes .packaging-evidence/<lane>.json from the validator's counts.
# test/packaging-evidence.py derives what each lane must claim from
# dist/release-matrix.json, so a claim that drifts from the matrix is refused.

# Debian 13 Trixie package — exact source build, TPM/PCR, and booted lifecycle.
test-deb-trixie-pkg: (_require-models "1")
    #!/usr/bin/env bash
    set -euo pipefail
    artifact_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-trixie.XXXXXX")"
    trap 'rm -rf -- "$artifact_dir"' EXIT
    package="$artifact_dir/facelock.deb"
    test/build-deb-package-image.sh trixie facelock-deb-trixie-pkg "$package"
    podman run --rm -v "$package:/facelock-test-package.deb:ro,Z" \
        facelock-deb-trixie-pkg \
        /bin/bash -c '/deb-package-lifecycle.sh install && exec /tpm-pcr-e2e.sh'
    PACKAGING_LANE='test-deb-trixie-pkg target=debian-trixie channel=apt build_origin=container-source-build runtime_policy=bundled-ort depth=full' \
        test/run-pkg-validate-systemd.sh facelock-deb-trixie-pkg "$package"

# Ubuntu 26.04 Resolute package — exact source build, TPM/PCR, and booted lifecycle.
test-deb-resolute-pkg: (_require-models "1")
    #!/usr/bin/env bash
    set -euo pipefail
    artifact_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-resolute.XXXXXX")"
    trap 'rm -rf -- "$artifact_dir"' EXIT
    package="$artifact_dir/facelock.deb"
    test/build-deb-package-image.sh resolute facelock-deb-resolute-pkg "$package"
    podman run --rm -v "$package:/facelock-test-package.deb:ro,Z" \
        facelock-deb-resolute-pkg \
        /bin/bash -c '/deb-package-lifecycle.sh install && exec /tpm-pcr-e2e.sh'
    PACKAGING_LANE='test-deb-resolute-pkg target=ubuntu-resolute channel=apt build_origin=container-source-build runtime_policy=bundled-ort depth=full' \
        test/run-pkg-validate-systemd.sh facelock-deb-resolute-pkg "$package"

# Same model requirement (and same opt-out) as the two Debian suite package gates.

# Package test — build real .rpm, install via dnf, validate under booted systemd
test-rpm-pkg release="44": (_fedora-lane-image release) (_require-models "1") _require-release-binaries
    #!/usr/bin/env bash
    set -euo pipefail
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" --build-arg ORT_VERSION={{ _ort-version }} \
        -t facelock-rpm-pkg-f{{ release }} -f test/Containerfile.rpm-e2e .
    PACKAGING_LANE='test-rpm-pkg-{{ release }} target=fedora-{{ release }} channel=direct-rpm build_origin=host-binaries runtime_policy=bundled-ort depth=full' \
        test/run-pkg-validate-systemd.sh facelock-rpm-pkg-f{{ release }}

# dist/release-matrix.json gives Fedora 45 a lifecycle depth of build/runtime
# smoke, so this deliberately stops short of the full lifecycle gate and never
# substitutes for a Fedora 43 or 44 result.

# Branched-release lane — build the package, then boot it for a runtime smoke
test-rpm-smoke release="45": (_fedora-lane-image release) _require-release-binaries
    #!/usr/bin/env bash
    set -euo pipefail
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" --build-arg ORT_VERSION={{ _ort-version }} \
        -t facelock-rpm-smoke-f{{ release }} -f test/Containerfile.rpm-e2e .
    PACKAGING_LANE='test-rpm-smoke-{{ release }} target=fedora-{{ release }} channel=direct-rpm build_origin=host-binaries runtime_policy=bundled-ort depth=smoke' \
        bash test/run-rpm-smoke-systemd.sh facelock-rpm-smoke-f{{ release }}

# Full lifecycle for 43 and 44, build plus runtime smoke for branched 45.

# Every declared Fedora release target at its declared lifecycle depth
test-rpm-lanes: (test-rpm-pkg "43") (test-rpm-pkg "44") (test-rpm-smoke "45")

# Packit config schema gate — runs the real `packit` in a digest-pinned Fedora container
test-packit-config:
    bash test/packit-config-validate.sh

# COPR-equivalent build — Packit SRPM + mock from-source rebuild on a Fedora chroot (slow, opt-in)
test-copr release="44": (_fedora-lane-image release)
    #!/usr/bin/env bash
    set -euo pipefail
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" \
        -t facelock-copr-test-f{{ release }} -f test/Containerfile.copr .
    podman run --privileged --rm -e COPR_CHROOT=fedora-{{ release }}-x86_64 \
        -v "$PWD:/repo:ro" facelock-copr-test-f{{ release }}

# Dev shell — interactive .deb container with host models for fast iteration (requires camera)
test-deb-dev-shell:
    #!/usr/bin/env bash
    set -euo pipefail
    artifact_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-dev-shell.XXXXXX")"
    trap 'rm -rf -- "$artifact_dir"' EXIT
    package="$artifact_dir/facelock.deb"
    test/build-deb-package-image.sh resolute facelock-deb-resolute-pkg "$package"
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts=""
    for f in /var/lib/facelock/models/*.onnx /var/lib/facelock/models/*.toml; do
        [ -f "$f" ] && mounts="$mounts -v $f:/tmp/host-models/$(basename $f):ro"
    done
    mounts="$mounts -v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro -v $package:/facelock-test-package.deb:ro,Z"
    echo "Starting dev shell (Ubuntu 26.04, .deb installed, host models). Try:"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-deb-resolute-pkg \
        bash -c "/deb-package-lifecycle.sh install; cp /tmp/container-config.toml /etc/facelock/config.toml; cp /tmp/host-models/* /var/lib/facelock/models/ 2>/dev/null; exec bash"

# Dev shell — interactive .rpm container with host models for fast iteration (requires camera)
test-rpm-dev-shell release="44": (_fedora-lane-image release) build-release
    #!/usr/bin/env bash
    set -euo pipefail
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" --build-arg ORT_VERSION={{ _ort-version }} \
        -t facelock-rpm-pkg-f{{ release }} -f test/Containerfile.rpm-e2e .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts=""
    for f in /var/lib/facelock/models/*.onnx /var/lib/facelock/models/*.toml; do
        [ -f "$f" ] && mounts="$mounts -v $f:/tmp/host-models/$(basename $f):ro"
    done
    mounts="$mounts -v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro"
    echo "Starting dev shell (Fedora, .rpm installed, host models). Try:"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-rpm-pkg-f{{ release }} \
        bash -c "cp /tmp/container-config.toml /etc/facelock/config.toml; cp /tmp/host-models/* /var/lib/facelock/models/ 2>/dev/null; exec bash"

# Release shell — clean-room .deb container, real user experience (requires camera)
test-deb-release-shell:
    #!/usr/bin/env bash
    set -euo pipefail
    artifact_dir="$(mktemp -d "${TMPDIR:-/tmp}/facelock-deb-release-shell.XXXXXX")"
    trap 'rm -rf -- "$artifact_dir"' EXIT
    package="$artifact_dir/facelock.deb"
    test/build-deb-package-image.sh resolute facelock-deb-resolute-pkg "$package"
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts="-v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro -v $package:/facelock-test-package.deb:ro,Z"
    echo "Starting release shell (Ubuntu 26.04, .deb installed, clean room). Try:"
    echo "  facelock setup"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-deb-resolute-pkg \
        bash -c "/deb-package-lifecycle.sh install; cp /tmp/container-config.toml /etc/facelock/config.toml; exec bash"

# Release shell — clean-room .rpm container, real user experience (requires camera)
test-rpm-release-shell release="44": (_fedora-lane-image release) build-release
    #!/usr/bin/env bash
    set -euo pipefail
    image="$(bash test/fedora-lane-image.sh '{{ release }}')"
    podman build --build-arg "BASE_IMAGE=$image" --build-arg ORT_VERSION={{ _ort-version }} \
        -t facelock-rpm-pkg-f{{ release }} -f test/Containerfile.rpm-e2e .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts="-v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro"
    echo "Starting release shell (Fedora, .rpm installed, clean room). Try:"
    echo "  facelock setup"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-rpm-pkg-f{{ release }} \
        bash -c "cp /tmp/container-config.toml /etc/facelock/config.toml; exec bash"

# Publish both exact manifests, or stand-in packages when none are given,
# through the real stable publisher in the pinned Debian trixie container, then
# prove a clean APT client resolves every declared suite from the tree pages.yml
# serves: the codenamed pair and the v0.1.4 compatibility names (#310).
# Needs podman; reprepro, gpg, dpkg-deb and apt all run in the container.
test-apt-repo trixie_manifest='' resolute_manifest='':
    #!/usr/bin/env bash
    set -euo pipefail

    command -v podman >/dev/null || { echo "Error: 'podman' not found. Install it first."; exit 1; }
    if [ ! -f dist/apt/conf/distributions ]; then
        echo "Error: dist/apt/conf/distributions not found"
        exit 1
    fi

    suites=(trixie resolute)
    manifests=(
        "{{ trixie_manifest }}"
        "{{ resolute_manifest }}"
    )
    supplied=0
    for manifest in "${manifests[@]}"; do
        if [ -n "$manifest" ]; then
            supplied=$((supplied + 1))
        fi
    done
    if [ "$supplied" -ne 0 ] && [ "$supplied" -ne "${#suites[@]}" ]; then
        echo "Error: test-apt-repo requires either no manifests or exactly one manifest for each stable suite: trixie, resolute" >&2
        exit 1
    fi

    mounts=(-v "$PWD:/src:ro")
    lane_args=()
    for index in "${!suites[@]}"; do
        suite="${suites[$index]}"
        manifest="${manifests[$index]}"
        [ -n "$manifest" ] || continue
        [ -f "$manifest" ] || {
            echo "Error: missing exact generated manifest for $suite: $manifest" >&2
            exit 1
        }
        manifest_dir=$(cd "$(dirname "$manifest")" && pwd)
        mounts+=(-v "$manifest_dir:/manifests/$suite:ro,Z")
        lane_args+=(--manifest "$suite=/manifests/$suite/$(basename "$manifest")")
    done
    if [ "$supplied" -eq 0 ]; then
        echo "No exact Debian artifact manifest supplied; publishing stand-in packages at this tree's version."
    fi

    # The lane image is the trixie suite image; the compatibility suites and
    # what each serves come from the matrix, for the suites the config declares.
    base_image="$(python3 - dist/release-matrix.json <<'PY'
    import json
    import sys

    with open(sys.argv[1], encoding="utf-8") as handle:
        print(json.load(handle)["apt_suites"]["trixie"]["image"])
    PY
    )"
    mapfile -t compat < <(python3 - dist/release-matrix.json dist/apt/conf/distributions <<'PY'
    import json
    import re
    import sys

    with open(sys.argv[1], encoding="utf-8") as handle:
        compat = json.load(handle)["apt_suites"].get("compat", {})
    with open(sys.argv[2], encoding="utf-8") as handle:
        declared = set(re.findall(r"(?m)^Codename:\s*(\S+)", handle.read()))
    for suite, details in compat.items():
        if suite in declared:
            print(f"{suite}={details['source']}")
    PY
    )
    for entry in "${compat[@]}"; do
        lane_args+=(--compat "$entry")
    done

    podman build --build-arg "BASE_IMAGE=$base_image" -t facelock-apt-client -f test/Containerfile.apt-client test
    podman run --rm --network=none "${mounts[@]}" facelock-apt-client /src/test/apt-client-lane.sh "${lane_args[@]}"

# Quick preflight before tagging a release
# Usage:
#   just release-preflight                 # assume stable release

# just release-preflight v0.2.0-rc.1     # prerelease (stable channels excluded)
release-preflight tag='':
    #!/usr/bin/env bash
    set -euo pipefail

    failed=0
    source scripts/release-versions.sh
    TAG="{{ tag }}"
    if [ -z "$TAG" ]; then
        TAG=$(release_tag_from_cargo "$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')")
    fi
    VERSION=$(release_cargo_from_tag "$TAG")
    if release_is_prerelease "$VERSION"; then prerelease=1; else prerelease=0; fi

    check_cmd() {
        local cmd="$1"
        if command -v "$cmd" >/dev/null 2>&1; then
            echo "OK: found '$cmd'"
        else
            echo "MISSING: '$cmd' not found in PATH"
            failed=1
        fi
    }

    echo "== Local tool checks =="
    check_cmd git
    check_cmd cargo
    check_cmd just
    check_cmd podman

    echo ""
    echo "== Packaging file checks =="
    for f in \
        dist/PKGBUILD \
        dist/PKGBUILD-bin \
        dist/PKGBUILD-git \
        dist/release-matrix.json \
        dist/facelock.spec \
        debian/control \
        debian/rules \
        dist/apt/conf/distributions \
        .packit.yaml \
        scripts/release-versions.sh \
        test/release-version-contract.sh \
        test/check-release-matrix.py \
        test/check-live-release-channels.py \
        .github/workflows/release.yml; do
        if [ -f "$f" ]; then
            echo "OK: $f"
        else
            echo "MISSING: $f"
            failed=1
        fi
    done

    echo ""
    echo "== Packit config schema (pinned Fedora container) =="
    bash test/packit-config-validate.sh || failed=1

    echo ""
    echo "== Release identity and target contract =="
    release_check_metadata "$TAG" || failed=1
    bash test/release-version-contract.sh || failed=1
    RELEASE_MATRIX_VERSION="$VERSION" python3 test/check-release-matrix.py || failed=1
    python3 test/check-live-release-channels.py || failed=1

    echo ""
    echo "== GitHub release secret checks =="
    if [ "$prerelease" -eq 1 ]; then
        echo "Mode: prerelease ($TAG) — stable APT/AUR and production COPR are excluded"
    else
        echo "Mode: stable release — stable APT/AUR secrets and a production COPR release job are required"
    fi
    echo "Note: COPR builds are handled by Packit (.packit.yaml) — no secret required."

    if [ "$prerelease" -eq 1 ]; then
        echo "SKIP: prerelease preflight does not access stable publication secrets"
    elif command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        if gh secret list | grep -q '^AUR_SSH_KEY\b'; then
            echo "OK: AUR_SSH_KEY configured"
        else
            echo "MISSING: AUR_SSH_KEY"
            if [ "$prerelease" -eq 0 ]; then
                failed=1
            fi
        fi

        if gh secret list | grep -q '^APT_GPG_PRIVATE_KEY\b'; then
            echo "OK: APT_GPG_PRIVATE_KEY configured"
        else
            echo "MISSING: APT_GPG_PRIVATE_KEY"
            if [ "$prerelease" -eq 0 ]; then
                failed=1
            fi
        fi

        if gh secret list | grep -q '^APT_GPG_PASSPHRASE\b'; then
            echo "OK: APT_GPG_PASSPHRASE configured"
        else
            echo "MISSING: APT_GPG_PASSPHRASE"
            if [ "$prerelease" -eq 0 ]; then
                failed=1
            fi
        fi
    else
        echo "SKIP: gh not installed or not authenticated; cannot verify repo secrets"
        if [ "$prerelease" -eq 0 ]; then
            failed=1
        fi
    fi

    echo ""
    echo "== Camera-required tier evidence =="
    # #139: `just test-arch-integration` and `just test-arch-oneshot` are the
    # only automated evidence that face authentication works end to end, and
    # they need a camera and a person in frame — so preflight cannot run them.
    # It can refuse to pass while nobody has. `just test-arch-camera-required`
    # runs both and records the commit they passed at; the omission used to be
    # invisible, which is how three of their assertions rotted.
    HEAD_SHA="$(git rev-parse HEAD)"
    RECORDED=""
    if [ -f .hardware-tiers-verified ]; then
        RECORDED="$(head -1 .hardware-tiers-verified)"
    fi
    ACK="${FACELOCK_HARDWARE_TIERS_ACK:-}"
    if [ "$RECORDED" = "$HEAD_SHA" ]; then
        echo "OK: camera-required tiers recorded green at $HEAD_SHA"
    elif [ "${#ACK}" -ge 7 ] && [ "${HEAD_SHA#"$ACK"}" != "$HEAD_SHA" ]; then
        # The acknowledgement has to name this commit, so it cannot be a habit
        # the way a bare =1 would become.
        echo "OK: camera-required tiers acknowledged by hand at $HEAD_SHA"
    else
        if [ -z "$RECORDED" ]; then
            echo "MISSING: no camera-required tier run recorded for any commit"
        else
            echo "STALE: camera-required tiers recorded at $RECORDED, HEAD is $HEAD_SHA"
        fi
        echo "  Run both tiers against this commit, with someone in front of the camera:"
        echo "    just test-arch-camera-required"
        echo "  If they were already run by hand at this exact commit, say so:"
        echo "    FACELOCK_HARDWARE_TIERS_ACK=$HEAD_SHA just release-preflight"
        failed=1
    fi

    echo ""
    echo "== Packaging gate evidence =="
    # #229: the deb, rpm and Arch gates are path-filtered on pull requests, so
    # a release commit can be green on every check and still never have had a
    # package built from it. The nightly matrix does not close that either: it
    # runs against whatever main was at 07:00 UTC, not against this commit.
    # A tag ships to four channels; this is the last place to find out.
    #
    # #313: a run's green conclusion is not the evidence; the lane records its
    # jobs upload are. A path-filtered run skips every lane and still concludes
    # "success", and a local FACELOCK_ALLOW_MISSING_MODELS=1 run used to write
    # the same one-line marker as a complete one. test/packaging-evidence.py
    # aggregates the workflow artifacts, or reads .packaging-matrix-verified,
    # and refuses anything short of every required lane at HEAD with zero skips.
    PACKAGING_EVIDENCE=""
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        for run_id in $(gh run list --workflow packaging.yml --commit "$HEAD_SHA" \
                --status success --limit 5 --json databaseId --jq '.[].databaseId' 2>/dev/null || true); do
            if python3 test/packaging-evidence.py ci-run --commit "$HEAD_SHA" --run "$run_id"; then
                PACKAGING_EVIDENCE="workflow run $run_id"
                break
            fi
        done
    fi
    if [ -n "$PACKAGING_EVIDENCE" ]; then
        echo "OK: packaging $PACKAGING_EVIDENCE carries complete lane evidence at $HEAD_SHA"
    elif python3 test/packaging-evidence.py validate --commit "$HEAD_SHA" .packaging-matrix-verified; then
        echo "OK: packaging matrix evidence recorded at $HEAD_SHA"
    else
        echo "MISSING: no complete packaging evidence for $HEAD_SHA (reasons above)"
        echo "  Run the matrix in CI against this commit, then re-run preflight:"
        echo "    gh workflow run packaging.yml --ref $(git rev-parse --abbrev-ref HEAD)"
        echo "  Or run every lane locally (30-60+ minutes, needs podman and the ONNX models):"
        echo "    just test-packaging-matrix"
        failed=1
    fi

    echo ""
    if [ "$failed" -ne 0 ]; then
        echo "Release preflight: FAILED"
        exit 1
    fi

    echo "Release preflight: OK"
    echo "Next: run 'just check', 'just test-arch-pam' and 'just test-arch-camera-free' before tagging."
    echo "      The packaging matrix (deb, rpm, Arch, version ordering) is the gate above."

# Fast release contract tests that do not require distro package tools.
test-release-contract:
    bash test/release-version-contract.sh
    python3 test/check-release-matrix.py

# Native version comparison tools run only inside disposable, digest-pinned containers.
test-release-native-ordering:
    podman run --rm -v "$PWD:/repo:ro" -w /repo docker.io/library/debian:13@sha256:34cd9e9fd437c0a095ec39cb2e73422c9f30821b0d0848ed74fd0d43bae4d958 bash test/release-native-ordering.sh debian
    podman run --rm -v "$PWD:/repo:ro" -w /repo registry.fedoraproject.org/fedora:44@sha256:fc3ec3da3ce49de0da0bab5a33223e90866c488d5d04fc6612284169e20a1bdb bash -lc 'dnf -qy install rpmdevtools && bash test/release-native-ordering.sh rpm'
    podman run --rm -v "$PWD:/repo:ro" -w /repo docker.io/library/archlinux:base-devel@sha256:714acd1eef9ae997d95691b1c5220ada0076185b77857c1813f02de0fa83cf7b bash test/release-native-ordering.sh arch

# Complete Track V version/matrix gate.
test-release-matrix: test-release-contract test-release-native-ordering

# A marker that outlives the run that wrote it is the failure this guards
# against: the previous marker and every lane record go before the first lane
# starts, so an interrupted or failed run leaves nothing preflight accepts.
_packaging-evidence-reset:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf -- .packaging-matrix-verified .packaging-evidence
    mkdir -p .packaging-evidence
    date -u +%Y-%m-%dT%H:%M:%SZ > .packaging-evidence/started-at

# The same lanes `.github/workflows/packaging.yml` runs, in one command, for a
# maintainer without CI in reach (#229). It is 30-60+ minutes and needs podman
# plus the ONNX models; CI is the faster path, and `just release-preflight`
# accepts a green run of that workflow at HEAD through the evidence artifacts
# its lanes upload, validated the same way as this record.
#
# Clean tree first, for the same reason the camera tiers demand one: a record
# that names a commit the lanes did not actually build is worse than no record.
# Each lane writes .packaging-evidence/<lane>.json; the marker is those records
# folded together by test/packaging-evidence.py, which refuses to write one
# when any lane skipped anything, ran without the ONNX models, names another
# commit, or is missing (#313). A FACELOCK_ALLOW_MISSING_MODELS=1 run therefore
# never records.

# Every packaging lane the release gate requires, recorded for release-preflight
test-packaging-matrix: _require-clean-tree _packaging-evidence-reset test-release-matrix test-arch-pkg test-deb test-rpm-lanes
    #!/usr/bin/env bash
    set -euo pipefail
    commit="$(git rev-parse HEAD)"
    if [ -n "$(git status --porcelain)" ]; then
        echo "error: the tree changed while the lanes ran; not recording." >&2
        exit 1
    fi
    python3 test/packaging-evidence.py aggregate --commit "$commit" --tree-clean \
        --started-at "$(cat .packaging-evidence/started-at)" \
        --evidence-dir .packaging-evidence --output .packaging-matrix-verified
    echo ""
    echo "Recorded: packaging matrix evidence at $commit"
    echo "'just release-preflight' accepts this until HEAD moves."
