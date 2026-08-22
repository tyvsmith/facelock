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
    bash test/rpm-authselect-contract.sh

# Run all checks (test + lint + format + audit + PAM standalone surface + agent docs)
check: test lint fmt-check audit check-pam-standalone check-agent-docs test-cargo-vendor-contract test-deb-source-contract test-deb-package-contract-test

# Prove the deterministic, exact Cargo source component used by Debian builds.
test-cargo-vendor-contract:
    bash test/cargo-vendor-contract.sh

# Static Debian source/metadata/release-consumer contract.
test-deb-source-contract:
    bash test/deb-source-contract.sh

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
        echo "         The daemon-start assertions will be reported as SKIPPED, not passed." >&2
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
        echo "       as skipped: FACELOCK_ALLOW_MISSING_MODELS=1 just <recipe>" >&2
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

# Build release and install to system

# Run as: just install (builds as you, installs as root)
install: build-release
    sudo env PATH="$PATH" just install-files

# Install pre-built binaries to system (requires root, no build)
install-files:
    #!/usr/bin/env bash
    set -euo pipefail

    # Verify binaries exist
    for f in target/release/facelock target/release/libpam_facelock.so; do
        [ -f "$f" ] || { echo "Error: $f not found. Run 'just build-release' first."; exit 1; }
    done

    # Binaries
    install -Dm755 target/release/facelock /usr/bin/facelock
    install -Dm755 target/release/libpam_facelock.so /lib/security/pam_facelock.so

    # Config (don't overwrite existing)
    install -Dm644 config/facelock.toml /etc/facelock/config.toml.default
    [ -f /etc/facelock/config.toml ] || cp /etc/facelock/config.toml.default /etc/facelock/config.toml

    # Hardware quirks database
    install -dm755 /usr/share/facelock/quirks.d
    install -Dm644 config/quirks.d/*.toml /usr/share/facelock/quirks.d/

    # Compiled translations (optional; produced by `just mo`, absent otherwise)
    if [ -d target/locale ]; then
        (cd target/locale && find . -name '*.mo' | while read -r mo; do
            install -Dm644 "$mo" "/usr/share/locale/${mo#./}"
        done)
    fi

    # systemd unit
    install -Dm644 systemd/facelock-daemon.service /usr/lib/systemd/system/facelock-daemon.service
    if [ -f /etc/systemd/system/facelock-daemon.service ] && \
       grep -q 'ExecStart=/usr/bin/facelock daemon' /etc/systemd/system/facelock-daemon.service; then
        install -Dm644 systemd/facelock-daemon.service /etc/systemd/system/facelock-daemon.service
    fi

    # D-Bus policy and activation
    install -Dm644 dbus/org.facelock.Daemon.conf /usr/share/dbus-1/system.d/org.facelock.Daemon.conf
    install -Dm644 dbus/org.facelock.Daemon.service /usr/share/dbus-1/system-services/org.facelock.Daemon.service
    if [ -f /etc/dbus-1/system.d/org.facelock.Daemon.conf ] && \
       grep -q 'org.facelock.Daemon' /etc/dbus-1/system.d/org.facelock.Daemon.conf; then
        install -Dm644 dbus/org.facelock.Daemon.conf /etc/dbus-1/system.d/org.facelock.Daemon.conf
    fi
    if [ -f /etc/dbus-1/system-services/org.facelock.Daemon.service ] && \
       grep -q 'org.facelock.Daemon' /etc/dbus-1/system-services/org.facelock.Daemon.service; then
        install -Dm644 dbus/org.facelock.Daemon.service /etc/dbus-1/system-services/org.facelock.Daemon.service
    fi
    # The bus may not have noticed the policy change yet; ask (best-effort).
    dbus-send --system --type=method_call --dest=org.freedesktop.DBus \
        /org/freedesktop/DBus org.freedesktop.DBus.ReloadConfig 2>/dev/null || true

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

    # Enable D-Bus activation (if systemd present)
    if [ -d /run/systemd/system ]; then
        systemctl stop facelock-daemon.service 2>/dev/null || true
        systemctl daemon-reload
        systemctl reset-failed facelock-daemon.service 2>/dev/null || true
        systemctl enable facelock-daemon.service 2>/dev/null || true
        echo "D-Bus activation enabled."
    fi

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

# Run as: just uninstall (elevates to root, preserving PATH)
uninstall:
    sudo env PATH="$PATH" just uninstall-files

# Uninstall files from system (requires root, called by uninstall)
uninstall-files:
    #!/usr/bin/env bash
    set -euo pipefail
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
    # Decide whether the /etc/systemd/system/ override matches the installed unit
    # (we want to clean up our own override, but never clobber an admin customization).
    # We must compare BEFORE removing /usr/lib/systemd/system/facelock-daemon.service.
    SYSTEM_OVERRIDE="/etc/systemd/system/facelock-daemon.service"
    INSTALLED_UNIT="/usr/lib/systemd/system/facelock-daemon.service"
    REMOVE_OVERRIDE=false
    if [ -f "$SYSTEM_OVERRIDE" ] && [ -f "$INSTALLED_UNIT" ] && cmp -s "$INSTALLED_UNIT" "$SYSTEM_OVERRIDE"; then
        REMOVE_OVERRIDE=true
    fi
    rm -f "$INSTALLED_UNIT"
    if [ "$REMOVE_OVERRIDE" = true ]; then
        rm -f "$SYSTEM_OVERRIDE"
        echo "Removed $SYSTEM_OVERRIDE (matched installed unit)"
    elif [ -f "$SYSTEM_OVERRIDE" ]; then
        echo "Kept $SYSTEM_OVERRIDE (admin-modified or installed unit not present for comparison)"
    fi
    rm -f /usr/share/dbus-1/system.d/org.facelock.Daemon.conf
    rm -f /usr/share/dbus-1/system-services/org.facelock.Daemon.service
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
    # message/mod.rs), so matching it here is precise, and the check below
    # makes gettext itself confirm every flag we wrote is truthful.
    awk '
        { buf[NR] = $0 }
        END {
            for (i = 2; i <= NR; i++)
                if (buf[i] ~ /^msgid "/ && buf[i] ~ /\{[a-z_][a-z0-9_]*\}/) {
                    if (buf[i-1] ~ /^#,/) sub(/$/, ", python-brace-format", buf[i-1])
                    else buf[i-1] = buf[i-1] "\n#, python-brace-format"
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

    xgettext --language=C --keyword=gettext --from-code=UTF-8 --no-wrap \
        --package-name=pam_facelock --copyright-holder="Facelock Contributors" \
        -o po/pam_facelock.pot crates/pam-facelock/src/lib.rs
    echo "Regenerated po/facelock.pot and po/pam_facelock.pot"

# Compile translations po/<lang>/{facelock,pam_facelock}.po into
# target/locale/<lang>/LC_MESSAGES/<domain>.mo. `just install` installs
# target/locale/ under /usr/share/locale if present. To start a new
# translation: mkdir -p po/de && msginit -i po/facelock.pot -o po/de/facelock.po -l de
# To verify a translation manually without installing system-wide:
#   FACELOCK_LOCALEDIR=$PWD/target/locale LANGUAGE=de facelock list
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
    found=0
    for po in po/*/*.po; do
        [ -e "$po" ] || continue
        found=1
        lang=$(basename "$(dirname "$po")")
        domain=$(basename "$po" .po)
        out="target/locale/$lang/LC_MESSAGES/$domain.mo"
        mkdir -p "$(dirname "$out")"
        msgfmt --check -o "$out" "$po"
        echo "  $po -> $out"
    done
    if [ "$found" = 0 ]; then
        echo "no .po files under po/<lang>/ — nothing to compile"
    fi

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

# Test RPM packaging in Fedora container
test-rpm: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t facelock-rpm-test -f test/Containerfile.fedora .
    podman run --rm facelock-rpm-test

# Static and booted, model-free Fedora authselect retirement lifecycle.
test-rpm-authselect:
    bash test/rpm-authselect-contract.sh
    podman build -t facelock-rpm-authselect-test -f test/Containerfile.rpm-authselect .
    bash test/run-rpm-authselect-systemd.sh facelock-rpm-authselect-test

# Run both exact supported-suite Debian package gates.
test-deb: test-deb-trixie-pkg test-deb-resolute-pkg

# Needs models/*.onnx: the validation starts the daemon under the hardened unit
# and checks what it holds at runtime. FACELOCK_ALLOW_MISSING_MODELS=1 runs the
# packaging half only, with the rest counted as skipped.

# Debian 13 Trixie package — exact source build, TPM/PCR, and booted lifecycle.
test-deb-trixie-pkg: (_require-models "1")
    #!/usr/bin/env bash
    set -euo pipefail
    test/build-deb-package-image.sh trixie facelock-deb-trixie-pkg
    podman run --rm facelock-deb-trixie-pkg /tpm-pcr-e2e.sh
    test/run-pkg-validate-systemd.sh facelock-deb-trixie-pkg

# Ubuntu 26.04 Resolute package — exact source build, TPM/PCR, and booted lifecycle.
test-deb-resolute-pkg: (_require-models "1")
    #!/usr/bin/env bash
    set -euo pipefail
    test/build-deb-package-image.sh resolute facelock-deb-resolute-pkg
    podman run --rm facelock-deb-resolute-pkg /tpm-pcr-e2e.sh
    test/run-pkg-validate-systemd.sh facelock-deb-resolute-pkg

# Same model requirement (and same opt-out) as the two Debian suite package gates.

# Package test — build real .rpm, install via dnf, validate under booted systemd
test-rpm-pkg: (_require-models "1") build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build --build-arg ORT_VERSION={{ _ort-version }} -t facelock-rpm-pkg -f test/Containerfile.rpm-e2e .
    test/run-pkg-validate-systemd.sh facelock-rpm-pkg

# COPR-equivalent build — Packit SRPM + mock from-source rebuild on a Fedora chroot (slow, opt-in)
test-copr:
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t facelock-copr-test -f test/Containerfile.copr .
    podman run --privileged --rm -v "$PWD:/repo:ro" facelock-copr-test

# Dev shell — interactive .deb container with host models for fast iteration (requires camera)
test-deb-dev-shell:
    #!/usr/bin/env bash
    set -euo pipefail
    test/build-deb-package-image.sh resolute facelock-deb-resolute-pkg
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts=""
    for f in /var/lib/facelock/models/*.onnx /var/lib/facelock/models/*.toml; do
        [ -f "$f" ] && mounts="$mounts -v $f:/tmp/host-models/$(basename $f):ro"
    done
    mounts="$mounts -v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro"
    echo "Starting dev shell (Ubuntu 26.04, .deb installed, host models). Try:"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-deb-resolute-pkg \
        bash -c "cp /tmp/container-config.toml /etc/facelock/config.toml; cp /tmp/host-models/* /var/lib/facelock/models/ 2>/dev/null; exec bash"

# Dev shell — interactive .rpm container with host models for fast iteration (requires camera)
test-rpm-dev-shell: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build --build-arg ORT_VERSION={{ _ort-version }} -t facelock-rpm-pkg -f test/Containerfile.rpm-e2e .
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
    podman run --rm -it $devices $mounts facelock-rpm-pkg \
        bash -c "cp /tmp/container-config.toml /etc/facelock/config.toml; cp /tmp/host-models/* /var/lib/facelock/models/ 2>/dev/null; exec bash"

# Release shell — clean-room .deb container, real user experience (requires camera)
test-deb-release-shell:
    #!/usr/bin/env bash
    set -euo pipefail
    test/build-deb-package-image.sh resolute facelock-deb-resolute-pkg
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts="-v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro"
    echo "Starting release shell (Ubuntu 26.04, .deb installed, clean room). Try:"
    echo "  facelock setup"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-deb-resolute-pkg \
        bash -c "cp /tmp/container-config.toml /etc/facelock/config.toml; exec bash"

# Release shell — clean-room .rpm container, real user experience (requires camera)
test-rpm-release-shell: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build --build-arg ORT_VERSION={{ _ort-version }} -t facelock-rpm-pkg -f test/Containerfile.rpm-e2e .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    mounts="-v $(pwd)/test/container-config.toml:/tmp/container-config.toml:ro"
    echo "Starting release shell (Fedora, .rpm installed, clean room). Try:"
    echo "  facelock setup"
    echo "  facelock enroll --user root --label myface"
    echo "  facelock test --user root"
    podman run --rm -it $devices $mounts facelock-rpm-pkg \
        bash -c "cp /tmp/container-config.toml /etc/facelock/config.toml; exec bash"

# Test APT repo generation locally from both exact manifests (requires reprepro + gpg).
test-apt-repo trixie_manifest='' resolute_manifest='':
    #!/usr/bin/env bash
    set -euo pipefail

    # Check tools
    for cmd in reprepro dpkg-deb; do
        command -v "$cmd" >/dev/null || { echo "Error: '$cmd' not found. Install it first."; exit 1; }
    done

    # Verify config exists
    if [ ! -f dist/apt/conf/distributions ]; then
        echo "Error: dist/apt/conf/distributions not found"
        exit 1
    fi

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    REPO_DIR="${TMPDIR}/repo"
    mkdir -p "${REPO_DIR}/conf"
    cp dist/apt/conf/distributions "${REPO_DIR}/conf/distributions"

    # For local testing without GPG, strip SignWith lines
    sed -i '/^SignWith:/d' "${REPO_DIR}/conf/distributions"

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

    if [ "$supplied" -eq 0 ]; then
        echo "No exact Debian artifact manifest supplied."
        echo "Validating reprepro config only..."
        reprepro -b "${REPO_DIR}" check
        echo ""
        echo "APT repo config: OK"
        echo "Pass the trixie and resolute manifests to include their exact .deb payloads."
        exit 0
    fi
    if [ "$supplied" -ne "${#suites[@]}" ]; then
        echo "Error: test-apt-repo requires either no manifests or exactly one manifest for each stable suite: trixie, resolute" >&2
        exit 1
    fi

    for index in "${!suites[@]}"; do
        suite="${suites[$index]}"
        manifest="${manifests[$index]}"
        [ -n "$manifest" ] || {
            echo "Error: missing exact generated manifest for $suite" >&2
            exit 1
        }
        bash test/deb-package-contract.sh --manifest "$manifest"
        manifest_dir=$(cd "$(dirname "$manifest")" && pwd)
        mapfile -t packages < <(grep -E '\.deb$' "$manifest")
        if [ "${#packages[@]}" -ne 1 ]; then
            echo "Error: $suite manifest must name exactly one .deb payload: $manifest" >&2
            exit 1
        fi
        deb="$manifest_dir/${packages[0]}"
        version=$(dpkg-deb -f "$deb" Version)
        case "$suite" in
            trixie) expected_suffix='~deb13u1' ;;
            resolute) expected_suffix='~ubuntu26.04.1' ;;
        esac
        case "$version" in
            *"$expected_suffix") ;;
            *)
                echo "Error: $deb version '$version' does not match stable APT suite '$suite' ($expected_suffix)" >&2
                exit 1
                ;;
        esac
        reprepro -b "${REPO_DIR}" includedeb "$suite" "$deb"
    done

    echo ""
    echo "=== APT repo structure ==="
    find "${REPO_DIR}" -type f -not -path '*/db/*' -not -path '*/conf/*' | sort

    # Validate expected structure
    for SUITE in trixie resolute; do
        [ -f "${REPO_DIR}/dists/${SUITE}/Release" ] || { echo "MISSING: dists/${SUITE}/Release" >&2; exit 1; }
        echo "OK: dists/${SUITE}/Release"
        [ -d "${REPO_DIR}/dists/${SUITE}/facelock/binary-amd64" ] || { echo "MISSING: dists/${SUITE}/facelock/binary-amd64/" >&2; exit 1; }
        echo "OK: dists/${SUITE}/facelock/binary-amd64/"
    done
    [ -d "${REPO_DIR}/pool/facelock" ] || { echo "MISSING: pool/facelock/" >&2; exit 1; }
    echo "OK: pool/facelock/"

    echo ""
    echo "APT repo generation: OK"

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

    if command -v packit >/dev/null 2>&1; then
        packit config validate --offline -c .packit.yaml || failed=1
    else
        echo "SKIP: packit CLI not installed; test-copr runs the required schema gate"
    fi

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
    if [ "$failed" -ne 0 ]; then
        echo "Release preflight: FAILED"
        exit 1
    fi

    echo "Release preflight: OK"
    echo "Next: run 'just check', 'just test-release-matrix', 'just test-arch-pam', 'just test-rpm', and 'just test-deb' before tagging."

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
