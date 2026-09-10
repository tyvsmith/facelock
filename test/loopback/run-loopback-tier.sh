#!/bin/bash
# Run the two camera-required E2E tiers against a synthetic camera.
#
# The camera is a v4l2loopback node fed by ffmpeg with the procedurally
# rendered face sequence from crates/facelock-test-support (see NOTICE.md
# beside this script). The node enumerates GREY only while it is fed, so
# facelock classifies it as IR by format evidence and the run enrolls and
# authenticates with `require_ir = true` and `require_frame_variance = true`,
# the product defaults the container config relaxes for a real RGB webcam.
# An optional second node fed YUYV is the non-IR camera the `require_ir`
# refusal assertions in test/run-oneshot-tests.sh otherwise skip.
#
# Usage: test/loopback/run-loopback-tier.sh [IR_NODE [RGB_NODE|none]]
#
#   FACELOCK_LOOPBACK_IR    IR node, default /dev/video20
#   FACELOCK_LOOPBACK_RGB   RGB twin, default /dev/video21; "none" to skip it
#   FACELOCK_TEST_IMAGE     container image, default facelock-pam-test
#   FACELOCK_LIVE_TIMEOUT   forwarded to the scripts (a timeout(1) duration)
#   FACELOCK_SYNTH_FRAMES   directory already holding ir.y8 and rgb.yuyv;
#                           otherwise they are rendered with cargo
#
# Only the loopback nodes are passed into the container, so a real camera on
# the host is never opened and nobody has to sit in front of it. Exit 2 means
# the host is not set up (no node, wrong node, missing tool) and says what
# to run; exit 1 means a tier failed.
set -euo pipefail

IR_NODE="${1:-${FACELOCK_LOOPBACK_IR:-/dev/video20}}"
RGB_NODE="${2:-${FACELOCK_LOOPBACK_RGB:-/dev/video21}}"
IMAGE="${FACELOCK_TEST_IMAGE:-facelock-pam-test}"
FRAMES_DIR="${FACELOCK_SYNTH_FRAMES:-}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"

WIDTH=640
HEIGHT=480
FPS=15

setup_hint() {
    cat >&2 <<EOF
       The tier needs two v4l2loopback nodes nothing else is feeding. On a
       host where the module is not loaded:
         sudo modprobe v4l2loopback devices=2 video_nr=20,21 \\
             card_label=facelock-synth-ir,facelock-synth-rgb exclusive_caps=1
       If v4l2loopback is already loaded for something else, add nodes
       without unloading it (v4l2loopback-utils):
         sudo v4l2loopback-ctl add -x 1 -n facelock-synth-ir /dev/video20
         sudo v4l2loopback-ctl add -x 1 -n facelock-synth-rgb /dev/video21
       Then make them writable by your user (they are root:video by default):
         sudo chmod a+rw /dev/video20 /dev/video21
       Different node numbers: FACELOCK_LOOPBACK_IR=/dev/videoN, and
       FACELOCK_LOOPBACK_RGB=/dev/videoM or none.
EOF
}

# A node this script may feed: a v4l2loopback device, idle, writable. A real
# camera has a parent device in sysfs (its USB or PCI function); a loopback
# node is a virtual device and has none, which is what keeps this script
# from ever pointing ffmpeg at the host's webcam.
check_node() {
    local node="$1" role="$2"
    local sys="/sys/class/video4linux/$(basename "$node")"
    if [ ! -c "$node" ]; then
        echo "error: $role node $node does not exist" >&2
        setup_hint
        exit 2
    fi
    if [ -e "$sys/device" ]; then
        echo "error: $node is a real camera ($(cat "$sys/name" 2>/dev/null || echo '?')), not a v4l2loopback node; refusing to feed it" >&2
        setup_hint
        exit 2
    fi
    if [ ! -w "$node" ]; then
        echo "error: $node is not writable by $(id -un) (mode $(stat -c %A "$node"))" >&2
        setup_hint
        exit 2
    fi
    # v4l2loopback reports "capture" once a producer has attached.
    if [ -r "$sys/state" ] && [ "$(cat "$sys/state")" = "capture" ]; then
        echo "error: $node ($(cat "$sys/name" 2>/dev/null || echo '?')) is already being fed by another process" >&2
        exit 2
    fi
}

# Wait until v4l2loopback has taken the producer's format, so the container
# enumerates the fed format and not the whole loopback format list.
wait_fed() {
    local node="$1" want="$2"
    local sys="/sys/class/video4linux/$(basename "$node")"
    local deadline=$((SECONDS + 10))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ -r "$sys/state" ] && [ "$(cat "$sys/state")" = "capture" ]; then
            local fmt
            fmt="$(cat "$sys/format" 2>/dev/null || echo '?')"
            case "$fmt" in
                "$want:${WIDTH}x${HEIGHT}"*) echo "$node: $fmt"; return 0 ;;
                *) echo "error: $node negotiated '$fmt', expected $want ${WIDTH}x${HEIGHT}" >&2; return 1 ;;
            esac
        fi
        sleep 0.2
    done
    echo "error: $node never reached the capture state; ffmpeg output:" >&2
    return 1
}

for tool in ffmpeg podman; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: $tool is not installed" >&2
        exit 2
    fi
done

[ "$RGB_NODE" = "none" ] && RGB_NODE=""
check_node "$IR_NODE" "IR"
if [ -n "$RGB_NODE" ]; then
    if [ "$RGB_NODE" = "$IR_NODE" ]; then
        echo "error: the IR and RGB nodes are the same device" >&2
        exit 2
    fi
    check_node "$RGB_NODE" "RGB"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/facelock-loopback.XXXXXX")"
FEEDERS=()
cleanup() {
    local pid
    for pid in "${FEEDERS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf -- "$WORK"
}
trap cleanup EXIT

if [ -z "$FRAMES_DIR" ]; then
    FRAMES_DIR="$WORK/frames"
    echo "rendering the synthetic face sequence"
    (cd "$REPO" && cargo run -q -p facelock-test-support --bin facelock-synth-face -- "$FRAMES_DIR")
fi
for f in ir.y8 rgb.yuyv; do
    if [ ! -s "$FRAMES_DIR/$f" ]; then
        echo "error: $FRAMES_DIR/$f is missing or empty" >&2
        exit 2
    fi
done

# A rawvideo loop, played at wall-clock rate (-re) so a capture blocks for a
# frame the way it does on a sensor, forever (-stream_loop -1). The sequence
# is one closed sweep, so the loop seam is one more small step.
feed() {
    local file="$1" in_fmt="$2" out_fmt="$3" node="$4" log="$5"
    ffmpeg -nostdin -loglevel error -re -stream_loop -1 \
        -f rawvideo -pix_fmt "$in_fmt" -video_size "${WIDTH}x${HEIGHT}" -framerate "$FPS" \
        -i "$file" -f v4l2 -pix_fmt "$out_fmt" "$node" > "$log" 2>&1 &
    FEEDERS+=("$!")
}

feed "$FRAMES_DIR/ir.y8" gray gray "$IR_NODE" "$WORK/ffmpeg-ir.log"
wait_fed "$IR_NODE" GREY || { cat "$WORK/ffmpeg-ir.log" >&2; exit 2; }
DEVICES=(--device "$IR_NODE")
if [ -n "$RGB_NODE" ]; then
    feed "$FRAMES_DIR/rgb.yuyv" yuyv422 yuyv422 "$RGB_NODE" "$WORK/ffmpeg-rgb.log"
    wait_fed "$RGB_NODE" YUYV || { cat "$WORK/ffmpeg-rgb.log" >&2; exit 2; }
    DEVICES+=(--device "$RGB_NODE")
else
    echo "no RGB twin: the require_ir refusal assertions will report SKIP"
fi

ENV_ARGS=(-e FACELOCK_E2E_STRICT_SECURITY=1)
if [ -n "${FACELOCK_LIVE_TIMEOUT:-}" ]; then
    ENV_ARGS+=(-e "FACELOCK_LIVE_TIMEOUT=$FACELOCK_LIVE_TIMEOUT")
fi

failed=0
for script in /run-integration-tests.sh /run-oneshot-tests.sh; do
    echo ""
    echo "== $script against $IR_NODE${RGB_NODE:+ and $RGB_NODE} =="
    if ! podman run --rm "${DEVICES[@]}" "${ENV_ARGS[@]}" "$IMAGE" "$script"; then
        failed=1
    fi
    for pid in "${FEEDERS[@]}"; do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "error: a feeder exited during the run" >&2
            cat "$WORK"/ffmpeg-*.log >&2
            exit 1
        fi
    done
done

exit "$failed"
