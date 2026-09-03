# Security Model

## Threat Model

facelock is a **local biometric authentication system**. The threat model assumes:

- **Attacker has physical access** to the machine (the entire point of face auth is physical-presence scenarios like unlocking a laptop)
- **Attacker may have a photo or video** of the enrolled user
- **Attacker does not have root** (if they do, game over regardless)
- **Attacker cannot modify files** in `/etc/facelock/`, `/var/lib/facelock/`, or `/lib/security/`

## Privacy Guarantees

Facelock is designed to keep biometric data under the user's exclusive control:

- **Local-only inference**: All face detection and recognition runs on-device via ONNX Runtime. No images, embeddings, or metadata are ever transmitted over the network.
- **No telemetry**: Facelock contains zero analytics, tracking, or phone-home code. After the one-time model download during `facelock setup`, it never contacts any server.
- **No cloud dependencies**: Authentication works fully offline. No account registration, no API keys, no external services.
- **Data stays on disk**: Face embeddings are stored in a local SQLite database (`/var/lib/facelock/facelock.db`) with restrictive permissions (600, root:root, inside a 711 root:root directory that nobody but root can list). Optional AES-256-GCM encryption with TPM-sealed keys provides defense in depth.
- **Open source**: All code is MIT/Apache-2.0 licensed. No proprietary blobs or obfuscated network calls. Privacy claims are verifiable by reading the source.

## Attack Vectors & Mitigations

### 1. Photo/Video Spoofing (CRITICAL)

**Attack**: Hold a photo or video of the enrolled user in front of the camera.

**Why this matters**: This is the #1 attack against face authentication. Without mitigation, anyone with a Facebook photo can unlock the machine.

**Mitigations** (layered, implement all):

#### A. IR Camera Enforcement (Required)

Add `security.require_ir` config flag, **default true**:

```toml
[security]
require_ir = true  # Refuse to authenticate on RGB-only cameras
```

Implementation (`facelock-camera/src/device.rs`, `ir_source_with_quirks`):

```rust
// IR classification is DERIVED from queried device evidence, surfaced as IrSource:
//   Quirk  – hardware quirks DB force_ir, corroborated (authoritative, both directions)
//   Format – the device's OWN queried capture formats are mono-only/IR-typical
//            (GREY/Y8/Y10/Y12/Y16, with NO color format mixed in)
//   None   – not classified as IR
//
// The free-text device name is NEVER, by itself, sufficient to classify a
// device as IR (#98): a crafted CARD_LABEL ("Fake IR Camera") on a
// v4l2loopback node exposing only YUYV/MJPG does not classify as IR. Per-node:
// 1. a quirks DB force_ir match is authoritative — by USB vendor:product ID
//    unconditionally; by device NAME ONLY when corroborated by a real USB
//    identity or the device's own mono-format evidence (#98 Task 3);
// 2. otherwise IR-ness is derived SOLELY from the device's own queried
//    formats: a mono-ONLY format set is IR, any set with a color format is not;
// 3. the device name is consulted only as an auto-detection tiebreak hint,
//    among nodes that ALREADY qualify by evidence — never to classify a node.
pub fn ir_source_with_quirks(device, quirks) -> IrSource { ... }

// Node-level disambiguation for multi-node USB devices: force_ir means "this
// USB device HAS an IR sensor", not "every capture node of it is IR". One
// physical camera can expose several V4L2 nodes under one VID:PID (Logitech
// BRIO 046d:085e: /dev/video0 = RGB YUYV/MJPG, /dev/video2 = IR native GREY).
// When multiple nodes share a quirk-matched USB identity AND at least one has
// an IR-typical format (GREY/Y8/Y10/Y12/Y16), only the node(s) with that
// format classify IR; siblings fall back to the quirk-free heuristic. A
// quirk's format_preference counts as this evidence only when it is itself
// IR-typical and the node actually advertises it. If NO node has an IR-like
// format, force_ir is trusted for all (some quirk entries exist precisely
// because the camera advertises no IR-like format). Anything gating require_ir
// uses these sibling-aware forms:
pub fn classify_ir_sources(devices, quirks) -> Vec<IrSource> { ... }
pub fn ir_source_resolved(device, quirks) -> IrSource { ... } // enumerates siblings

// In the auth flow (daemon pre_check and oneshot), before recognition:
if config.security.require_ir && !device_is_ir {
    return DaemonResponse::Error {
        message: "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED).".into()
    };
}
```

**Rationale**: Phone screens and printed photos do not emit infrared light correctly. An IR camera sees a flat, textureless surface where a real face would have depth and skin texture in IR. This single check eliminates the vast majority of spoofing attacks.

**Why the name alone is not enough, and what IS the evidence (H1, #98/#99)**: IR classification is derived **solely from queried device evidence** — the pixel formats the device actually enumerates — never from its free-text name. A node qualifies as IR only when it enumerates *exclusively* IR-typical mono formats (GREY/Y8/Y10/Y12/Y16) with **no color format mixed in**. Many ordinary RGB UVC webcams enumerate a GREY format *alongside* YUYV/MJPG; those do **not** qualify (this is what the old "H1" concern was about, now resolved by the mono-**only** requirement rather than by name corroboration). The previous heuristic (`contains("ir")` OR any GREY/Y16 format) misclassified plain webcams as IR and matched the substring "ir" inside unrelated names ("Sirius", "AIR-Cam"), silently defeating `require_ir = true`. Now a crafted `CARD_LABEL` on a color-only v4l2loopback device does not classify as IR no matter what it is named (#98), and the quirk `name_pattern` matcher is **anchored** (it matches the whole device name, not a substring) so it no longer fires on the "ir" inside those unrelated names (#99). The name is used only as a tiebreak hint when auto-detection chooses among nodes that *already* qualify by evidence, and as one corroboration path for a name-only `force_ir` quirk — never as a standalone classification signal. This is why `require_ir` is now load-bearing rather than trivially bypassable.

**Quirk `force_ir` corroboration (#98 Task 3)**: a quirks `force_ir = true` entry that matched by **USB vendor:product ID** is authoritative on its own — a software-only virtual device (v4l2loopback) has no real USB node, so it can never win a USB-ID match. A `force_ir = true` entry that matched only by device **name**, however, requires corroboration before it is trusted: either the device has a real (even if DB-unlisted) USB identity, or its own queried formats independently support IR. Without either, a crafted name that happens to match a shipped pattern falls through to the evidence-only heuristic instead of being trusted. (`force_ir = false` remains authoritative unconditionally — the conservative "not IR" direction is always honored.)

**Honest residual — format evidence is not unforgeable**: deriving IR-ness from queried formats raises the attacker's cost from "set a free-text `CARD_LABEL` string" to "also negotiate a mono-**only** pixel format", and removes the old path where a bare name token (or a name-only quirk) could escalate a device to IR. It does **not** make the evidence unforgeable. A `v4l2loopback` device (loading the module requires **root**) or a programmable USB gadget can present a mono-only (GREY/Y16/…) format set and **will** classify as IR — the format check cannot distinguish a genuine IR sensor from a device that merely advertises IR-typical formats. The remaining backstops against a fabricated IR device are the **liveness / frame-variance checks** (§B) and the **privilege required to create such a device** in the first place (root to load `v4l2loopback`, or physical access to attach USB-gadget hardware). `require_ir` is one layer of a layered defense, not a standalone attestation of a real IR sensor.

**Why `force_ir` is device-level, not node-level (hardware-verified regression)**: on a real Logitech BRIO, treating every quirk-matched node as IR made *both* `/dev/video0` (the RGB sensor) and `/dev/video2` (the IR sensor) classify IR — so setup stopped auto-selecting and auto-detect captured from the RGB sensor (white LED) instead of the IR sensor. The sibling-format disambiguation above restores per-node honesty: exactly one BRIO node is `[IR]`, and auto-detection prefers the format-corroborated IR node. A quirk preference for an RGB format such as MJPG may still guide capture negotiation, but it is not IR evidence and cannot exempt an RGB sibling from demotion.

**Limitation**: classification is capability-based, not a hardware allow-list. A genuine IR camera that exposes its IR and color streams on a *single* V4L2 node (so its format set is not mono-only) and is not covered by a shipped quirk will not auto-classify as IR. Add a quirks `force_ir` entry keyed by **USB vendor:product ID** (`/etc/facelock/quirks.d/`) for such hardware — a VID:PID match is authoritative — and set `format_preference` to the IR node's native format (e.g. `"GREY"`) when the camera exposes multiple capture nodes. Prefer a USB-ID quirk over a name-only one: a name-only `force_ir` is trusted only when corroborated by the device's own mono-format evidence or a real USB identity, so it is not a reliable override on its own. The `facelock devices` command displays whether each camera is detected as IR. Device *identity* pinning (rather than capability heuristics) is implemented as its robust successor — see §1.D Device Coupling (Plan 02).

#### B. Frame Variance Check (Required)

Require minimum variance across consecutive matched frames during authentication.
The check is evaluated over a **sliding window** of the most recent
`min_auth_frames` matched-frame embeddings (`FrameVarianceWindow` in
`facelock-core/src/types.rs`): the gate passes only when the window is full AND
every consecutive pair inside it has cosine similarity at or below the cutoff
(`security.frame_variance_max_similarity`, default **0.985**,
`DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY`).

**Why a sliding window**: an earlier version accumulated every matched frame for
the whole session and required *all* consecutive pairs to drift. One too-still
pair at any moment then made success permanently unreachable — a user who
started still and then moved could never recover (hardware-verified lockout).
The window forgets old frames: once it fills with moving frames the gate passes.
The anti-photo property is preserved because a truly static input keeps *every*
pair above the cutoff in *every* window, so no window ever passes, regardless of
session length. Embeddings evicted from the window are zeroized at eviction.

**Field-measured consecutive-pair similarity** (Logitech BRIO IR node, real user):

| Input | Consecutive-pair cosine similarity |
|-------|-----------------------------------|
| Truly static (photo on a stand, paused replay) | ≳ 0.999 |
| Frozen, non-blinking live human | 0.98 – 0.995 |
| Naturally moving live human | well below 0.98 |

The default cutoff (0.985) sits *inside* the frozen-human band — deliberately
stricter than the top of the band (0.995) for extra margin against static
replays that carry sensor-noise-level drift. The honest tradeoff: a fully
frozen, non-blinking user may not pass at 0.985, but because the gate is a
sliding window it recovers the moment they move slightly — the worst case is a
brief delay, never a lockout, and the PAM stack falls through to password
regardless. (An earlier default of 0.97 assumed a 0.02–0.10 live-drift range
that is empirically wrong for a still user — it caused hard false-reject
lockups where auth reported "no match" despite 0.91–0.98 recognition
similarity. The window-less design of that era turned stillness into a
permanent failure; the sliding window is what makes the stricter 0.985 default
safe to ship.)

**Honest scope — this does NOT stop video replay.** Frame-variance only rules out a
*static* image (printed photo, single frozen frame). A recorded video of the enrolled
user contains genuine inter-frame motion and will pass this check. Frame-variance is a
cheap passive defense-in-depth filter whose honest job is rejecting perfectly-static
input; **IR enforcement plus the raw-frame texture check (§A, §C) are the load-bearing
anti-spoof defenses**, and active liveness (opt-in landmark/blink) is the answer to
video replay.

**False-reject tradeoff**: `frame_variance_max_similarity` is the user-tunable
knob and stays purely passive either way. Loosen toward 0.995 (top of the
frozen-human band) if a very still user finds the delay annoying; tighten toward
0.97 for paranoia, accepting that holding still delays auth until you move.
Raising it above ~0.998 starts admitting sensor-noise-level drift and defeats
the check. When the timeout expires with matching frames but an unsatisfied variance gate,
`facelock test` says so explicitly, and per-window min/max pair similarities are
logged at debug level (values only, never embeddings) for tuning.

Config:
```toml
[security]
require_frame_variance = true         # Reject static images (photo attack defense)
frame_variance_max_similarity = 0.985 # Max consecutive-frame similarity in the window
min_auth_frames = 3                   # Matched frames required = variance window size
```

#### C. Dark Frame / IR Texture Validation (Recommended)

In IR mode, verify that the face region has expected IR texture characteristics:
- Real skin has micro-texture visible in IR
- Photos/screens appear as flat, uniform surfaces in IR
- Compute standard deviation of pixel intensity within the face bounding box
- Reject faces with abnormally low texture variance

```rust
pub fn check_ir_texture(gray: &[u8], bbox: &BoundingBox, width: u32, min_stddev: f32) -> bool {
    let face_pixels = extract_bbox_region(gray, bbox, width);
    if face_pixels.is_empty() { return false; }
    let mean: f32 = face_pixels.iter().map(|&p| p as f32).sum::<f32>() / face_pixels.len() as f32;
    let variance: f32 = face_pixels.iter().map(|&p| (p as f32 - mean).powi(2)).sum::<f32>() / face_pixels.len() as f32;
    variance.sqrt() > min_stddev
}
```

**Run on the RAW frame, not CLAHE (H3)**: this check MUST see the raw grayscale frame.
The auth loop previously fed a **CLAHE**-equalized frame into `check_ir_texture`. CLAHE
(Contrast-Limited Adaptive Histogram Equalization) stretches local contrast, which
*inflates* the std_dev of an otherwise flat photo/screen and pushes it above the cutoff —
i.e. CLAHE was masking exactly the spoof this check exists to catch. CLAHE now belongs
only to the recognition/embedding path; texture measurement uses `frame.gray` directly.

**Verified Y16 scale (conservative alpha rule)**: the absolute
`ir_texture_min_stddev` threshold is meaningful only after Y16 samples are mapped onto a
known 8-bit scale. Authentication therefore accepts an actual selected/negotiated Y16
format only when its matched quirk declares a valid, hardware-verified `y16_bit_depth` in
8..=16. The session shift is then `bit_depth - 8`, fixed before any auth frame is captured
and never recomputed per frame. Missing or invalid depth has typed provenance
`UnverifiedY16`; it never reaches the texture comparison.

Facelock derives this state twice. Interrogation applies the real format preference and
normal negotiation priority to the enumerated normalized FourCCs, so a known unverified
Y16 choice is rejected before opening the camera. After `VIDIOC_S_FMT`, the opened camera
recomputes it from the actual normalized FourCC. If a driver negotiated Y16 after a
different format was predicted, authentication skips warmup/calibration capture and
returns the same rejection before the auth loop can succeed. An actual GREY stream is
`NotY16` and keeps the existing 8-bit behavior, even when the same device also advertises
Y16.

The stable recoverable message is `Y16 IR texture scale is unverified; authentication
requires a verified y16_bit_depth (8..=16) quirk`. The daemon returns its ordinary `-2`
error sentinel; oneshot returns exit 2; PAM maps either path to `PAM_IGNORE`, preserving
password fallback. The gate applies even when `security.require_ir = false`: disabling IR
classification enforcement does not make an unknown pixel scale trustworthy. It neither
reclassifies/downgrades the device to RGB nor silently skips the texture check.

Scene calibration remains available for non-auth Y16 conversion such as preview,
enrollment, and benchmarking. It runs lazily on their first capture and pins one shift for
that camera session, but it never upgrades `UnverifiedY16` or authorizes authentication.
This separation closes the previous fail-open direction: a dim calibration scene could
pick a shift that inflated a flat spoof's measured standard deviation above the absolute
threshold. A scale-invariant texture metric is intentionally not introduced in this
alpha.

Hardware evidence is deliberately asymmetric. The Logitech BRIO 046d:085e IR node was
manually exercised in native GREY for #162 (33/33 integration and 27/27 oneshot
scenarios), so this rule preserves an observed path. Facelock has no Y16 hardware or
fixture validation record; the texture bands below have never been validated through
Y16. The shipped Intel RealSense Y16 quirks therefore do not declare a depth and reject
authentication recoverably until an evidence-backed local or shipped quirk does. Do not
infer effective sensor depth from the 16-bit V4L2 container.

**Raw-frame calibration**: on the raw frame, flat surfaces (photos/screens in IR) score
std_dev **< 5**, real IR skin scores **> 15**. The cutoff `security.ir_texture_min_stddev`
defaults to **10.0** (between the two bands). Lower it if real faces are being rejected;
raise it toward 15 to be stricter. Applied on IR devices only (RGB texture is too variable).

#### D. Device Coupling — Template↔Camera Binding (Plan 02, default on)

**Attack it addresses**: the realistic attacker unplugs the enrolled IR camera and plugs
in *their own* camera — a commodity RGB webcam or a different-model unit — to feed a spoof
frame into the recognition path. The IR heuristics above (§A–C) are capability checks on
*whatever camera is currently attached*; they do not verify it is the *same* camera the
user enrolled on. Device coupling closes that gap by **identity** rather than capability.

Each enrolled template records a fingerprint of the camera that captured it — the canonical
string `"vid:pid:serial"` read from sysfs (`idVendor`/`idProduct`/`serial`), stored in
`face_models.device_id` (schema V6). At auth, the live camera is fingerprinted once and every
candidate template whose fingerprint does not match at the configured granularity is
**skipped before its embeddings are ever compared**. A skipped template can only produce
"no match", so a swapped-in camera **degrades to the password fallback** — it can never
reach a success, and it never causes a lockout (fail SOFT, matching the biometric-is-
`sufficient`-never-`required` contract).

Config (`[security]`):

```toml
bind_templates_to_device = true      # default; skip templates from a non-matching camera
device_match_granularity = "model"   # "model" = VID:PID (default), "unit" = VID:PID:serial
bind_legacy_templates    = true      # default; allow-with-warn for pre-V6 / NULL-device rows
```

- **`model`** (default) compares VID:PID only. Invisible to single-camera users; blocks a
  cross-model swap. Identical same-model cameras are accepted (documented behavior).
- **`unit`** additionally requires a matching serial. Blocks even a same-model swap, but only
  works on cameras that expose a stable serial — enrollment at `unit` on a camera without a
  serial, or without a full vendor:product identity, is refused with an explanatory error
  rather than silently downgrading (a NULL row would bind to nothing). The check runs once
  the camera is open and before the first model write (#309), so a refusal leaves no template
  behind; the error names `security.device_match_granularity` and the way out. It judges the
  id that would be stored, not whether `bind_templates_to_device` is on today, so a template
  enrolled while coupling is off still matches once coupling is turned on.
- **Legacy / unidentifiable cameras**: a pre-V6 template (NULL `device_id`) or one enrolled
  on a camera exposing no USB identity is stored NULL and governed by `bind_legacy_templates`.
  A vendor id without a product id (or the reverse) is no identity: the matcher needs both.
  At `model` such a camera is accepted as before, but what is stored changed (#309): NULL,
  where it used to be an id that could never match. At `unit` it is refused.
  Default allow-with-warn so an upgrade never breaks existing enrollments; a one-line log
  nudges re-enrollment. Set it `false` to require every template to carry a matching id;
  enrollment then refuses a camera with no usable identity (while coupling is on), since
  the NULL row it would store could never authenticate. The error names
  `bind_legacy_templates`.
- **Enroll per camera**: enrolling the same user on a second camera creates a *second* template
  with its own `device_id` (the store already allows multiple models per user). Each template
  then authenticates only on its own camera. `facelock list` shows each template's camera.

**Honest threat framing — this is advisory defense-in-depth, NOT attestation.** `VID:PID` is
**model granularity**: every unit of the same model shares it. `serial` is unit-unique *when
present*, but vendors frequently omit or duplicate it. Most importantly, a **programmable USB
device can forge any of these fields** — consumer UVC cameras have no signed-frame
attestation, so there is no cryptographic root of trust to bind to. Device coupling raises the
bar against the attacker who buys/plugs a *commodity* camera; it does not stop an attacker who
builds a USB device that impersonates the enrolled camera's descriptors. It is one layer, not
a guarantee.

**Plan 04 seam — now wired (opt-in)**: `facelock_core::types::device_binding_aad()` defines
the AAD bytes for folding `device_id` into the AES-GCM Additional Authenticated Data. Plan 04
wires this through enroll and the decrypt path behind `security.bind_device_aad` (default
false): when enabled, each encrypted template is bound to its enrolling camera's id and cannot
be decrypted under a different one. It stays **opt-in** because hard binding fails closed on
the unstable/absent ids described above: enrollment on a camera with no usable identity is
refused outright (#312), and an id that changes between enrollment and authentication fails
decryption, so on such hardware every auth would fall through to password. Default-off keeps
the never-lockout guarantee for everyone else. What enabling it does to an existing store is
in §3.C Encryption at Rest below. Integrated MIPI/CSI cameras (Intel IPU6/IPU7, the Windows
Hello class) expose no USB vendor or product identity at all, so neither
`device_match_granularity = "unit"` nor `bind_device_aad` can be used with them; `model` with
`bind_legacy_templates = true` is the only coupling setting that enrolls there.

### 2. Model Tampering

**Attack**: Replace ONNX model files with adversarial models that always match (or match specific attackers).

**Mitigations**:

#### A. SHA256 Verification at Load Time (Required)

Verify model integrity not just at download, but every time the daemon loads models:

```rust
impl FaceEngine {
    pub fn load(config: &RecognitionConfig, model_dir: &Path) -> Result<Self> {
        let manifest = load_manifest();

        for model in &manifest.default_models() {
            let path = model_dir.join(&model.filename);
            if !verify_model(&path, &model.sha256)? {
                return Err(FacelockError::Detection(format!(
                    "Model integrity check failed for {}. Expected SHA256: {}. \
                     Re-run `facelock setup` to re-download.",
                    model.filename, model.sha256
                )));
            }
        }
        // ... load models
    }
}
```

#### B. File Permissions on Model Directory (Required)

```bash
# Models owned by root, not writable by others
chown -R root:root /var/lib/facelock/models
chmod 755 /var/lib/facelock/models
chmod 644 /var/lib/facelock/models/*.onnx
```

The model files are public, SHA-256-verified downloads, so their own modes are
permissive; see `docs/contracts.md` § *Traversal for everyone, listing for
nobody*. What the modes here must guarantee is only that nobody but root can
**write** them.

#### C. ONNX Runtime Loader Trust (Required)

ONNX Runtime is executable code loaded inside the daemon, PAM one-shot helper,
and other privileged entry points. The loader therefore validates a candidate
before mapping it. Privileged contexts include a zero real or effective UID or
GID, set-id ID mismatches, the kernel's `AT_SECURE` mode, and any inheritable,
permitted, effective, or ambient Linux capability on the calling thread. The
capability check reads `/proc/thread-self/status`; unreadable, missing,
duplicate, empty, or malformed capability state fails closed as privileged.
Every privileged context ignores `ORT_DYLIB_PATH` completely and never searches
`/usr/local`.

The deterministic order is: an explicit override only for an unprivileged
process; the trusted system locations for the configured GPU provider; the
package manager's `/usr/lib64/libonnxruntime.so.1` and
`/usr/lib/libonnxruntime.so.1`; then Facelock's package-owned bundle. The Fedora
bundle uses the stable SONAME filename. Existing Debian bundles retain a
package-owned unversioned compatibility name at the end of the same category.

For every privileged system or bundle candidate, the loader first resolves a
symlinked approved root by path — merged-/usr distributions ship `/usr/lib64`
as a link to `lib` — under the same rules as SONAME links: a root-owned,
single-link chain of confined relative targets, every traversed directory
root-owned and not group- or world-writable. Absolute and escaping targets are
rejected even when they name an approved location. The loader then opens the
resolved real directory without following a link and walks each relative
component by held descriptor with `O_NOFOLLOW|O_NONBLOCK`. Directory links
beneath the root, absolute link targets, `..`, escape-and-return targets, and
untrusted link owners are rejected. A package SONAME symlink is allowed only as
a root-owned, single-link, relative chain beneath the held root; every
traversed directory must be root-owned and not group- or world-writable. One
descriptor walker is used on kernels both with and without `openat2`, so an
`ENOSYS` path cannot receive weaker checks.

Before `dlopen`, the final descriptor must name a bounded regular file with
exactly one hard link, root ownership, no group/world write bits, no
set-user-ID/set-group-ID bits, and no `security.capability` xattr. Device,
inode, link count, size, ownership, mode, and modification/change timestamps
must remain stable before and after the bounded read. The bytes must be a
64-bit ELF for the running architecture, with SONAME `libonnxruntime.so.1` and
no RPATH/RUNPATH other than exactly `$ORIGIN` (or `${ORIGIN}`). Mapping uses
that already validated descriptor, not a bare pathname followed by post-map
checks. Corrupt, escaping, mutable-parent, wrong-architecture, wrong-SONAME,
and unsafe search-path candidates are rejected and resolution continues
fail-closed.

Authentication never downloads a runtime or model. Release CI fetches a pinned
ORT archive in a separate network stage, verifies its SHA-256 before extraction,
and carries the license, third-party notices, version, commit, provenance,
checksums, and manifest into package assembly. Direct RPM assembly itself only
accepts that prepared artifact and runs entirely beneath a fail-closed seccomp
sandbox that denies socket creation, connection/message syscalls, and
`io_uring_setup`, closes inherited non-stdio descriptors, and proves the denial
with an `ENOSYS` network probe before invoking `rpmbuild`. Cargo offline mode is
additional defense; it is not the network boundary.

#### Debian source-build boundary

Debian-family release support is exactly Debian 13 (Trixie) and Ubuntu 26.04
LTS (Resolute). Both suites ship the single `facelock` package with TPM enabled;
Bookworm and Noble artifacts may remain in historical releases, but those
suites are unsupported and receive no new packages.

The Debian source package carries four independently classified inputs: the
exact tagged main archive, the reviewed ORT component and its legal/provenance
set, the deterministic Cargo-vendor component bound to `Cargo.lock`, and the
Debian quilt delta. Trixie uses official Backports Rust/Cargo and Resolute uses
its native distro toolchain. No rustup toolchain is trusted as a build
dependency. Package assembly and a clean `.dsc` rebuild both run through the
same fail-closed syscall sandbox with network denied and empty Cargo/Rustup
caches. Cargo's locked/offline mode is additional enforcement; it never
substitutes for the network boundary.

The release and clean rebuild must agree on binary-package identity, resolved
dependencies, installed paths, and installed file hashes. Stable APT
publication is all-or-nothing across exactly two suite manifests. This prevents
an undeclared toolchain/cache fetch, a component omission, or a partial suite
publication from being mistaken for a reproducible Debian source build.

### 3. Embedding / Database Security

**Attack**: Read or modify the SQLite database to extract biometric data or inject fake embeddings.

**Mitigations**:

#### A. Database File Permissions (Required)

```bash
# Database owned by root, readable by root only. User-run PAM stacks request
# authentication through the daemon; nothing but root reads templates.
chown root:root /var/lib/facelock/facelock.db
chmod 600 /var/lib/facelock/facelock.db
```

Runtime note:
- The daemon/setup paths must also secure SQLite `-wal` and `-shm` sidecar files to `0600`
- Audit logs and snapshots must be created with explicit restrictive modes instead of relying on ambient umask
- The systemd service should set `UMask=0027` as a baseline defense-in-depth default

#### A2. Traversal for Everyone, Listing for Nobody (`/var/lib/facelock` is 0711)

```
/var/lib/facelock/            0711 root:root       traverse-only, NOT listable
  facelock.db                 0600 root:root
  facelock.db-wal / -shm      0600 root:root
  models/                     0755 root:root       public, SHA-256 verified
  enrolled/                   0711 root:root       markers only
    <user>                    0600 <user>:<user>

/var/log/facelock/            0700 root:root
  audit.jsonl                 0600 root:root
  snapshots/                  0700 root:root
```

The state directory grants every local user traversal (`--x`) and nobody but
root listing (no `r` for group or other). Anyone can `open()` a path it
already knows by name — its own `enrolled/<user>` marker, a model file — and
nobody can `readdir` the directory, read the `0600 root:root` database, or
reach the audit log or snapshots. Every secret is protected by its own mode;
the directory protects only *what is there* from enumeration. There is no
group (ADR 010): nothing here is group-owned.

Two consequences worth stating explicitly:

- **D-Bus is required for user-run screen lockers** (hyprlock/swaylock) and
  the polkit agent. Their PAM stack runs as the user, and nothing makes the
  database or the `0600 root:root` encryption key readable to them, so the
  daemon is the only path — and the bus admits their `Authenticate` without a
  group (§ 4 A). Root-invoked PAM (`sudo`, `login`, `sshd`) additionally has
  the oneshot fallback, which reads the files directly as root.
- **Known residual**: any local user can `stat` a name it can guess —
  `facelock.db` (size, mtime), `enrolled/<user>` (existence) — because
  traversal permits exactly that. Closing it would mean denying the traversal
  that `is-enrolled` and model loading depend on. Accepted; before ADR 010 the
  same residual existed for group members.

**The enforcement mechanism is a guard test, not this document.** The test in
`crates/facelock-cli/src/state_layout.rs` walks every entry under the state
directory and asserts that no file carries any "other" bit and no directory
carries "other" read or write — traversal is the only thing granted — with
`models/` (public data) as the single allowed exception. A future change that
drops a world-readable file into the state directory fails that test with a
message that explains the rule.

#### A3. Enrollment Markers (`/var/lib/facelock/enrolled` is 0711)

`facelock is-enrolled` must not activate the daemon or open a camera — it runs
repeatedly on the lock screen. It answers from a marker file rather than from
the database, and *"enrolled"* means **"face auth is operational for me"**:
the caller opens its own `0600` marker by name through two `0711 root:root`
directories. No group is involved (ADR 010): the answer is
`enrolled` the moment enrollment writes the marker.

```
/var/lib/facelock/enrolled/          0711 root:root
/var/lib/facelock/enrolled/<user>    0600 <user>:<user>
```

- **`0711` on the directory** permits traversal to a known filename but not
  `readdir`, so which accounts have face auth enrolled is not listable (a
  guessed name can be probed for existence — the § A2 residual).
- **`0600` owned by the user** means "am I enrolled?" is answerable by that
  user and by nobody else — the same privacy property as
  `~/.ssh/authorized_keys`.
- `EACCES` and `ENOENT` are both reported as not-enrolled, never as an error.
  An indicator that fails to show is the safe way to be wrong.

Several places encode this layout and must stay in sync: `dist/facelock.tmpfiles`,
`dist/facelock.install`, `debian/postinst`, `dist/nix/module.nix`,
`dist/openrc/facelock-daemon`, the `install-files` recipe in `justfile`,
`secure_setup_paths()` in `crates/facelock-cli/src/commands/setup.rs`, the
default path constants in `crates/facelock-core/src/paths.rs`, and the typed
constants plus guard tests in `crates/facelock-cli/src/state_layout.rs`.

The `justfile` one is easy to forget and the most expensive to get wrong:
`test/Containerfile` builds the test image with `just install-files`, so if that
recipe drifts the container tests exercise a layout no user ever runs — and may
pass while doing it. `just test-arch-layout` asserts the shipped modes exactly.

**The marker is a hint, not authority.** It can drift from the database (an
out-of-band restore, for instance). That is acceptable because `is-enrolled`
only decides whether to show a UI affordance: a stale marker degrades gracefully —
the indicator appears, the PAM attempt fails, and the password context was
running in parallel the whole time. **PAM at auth time remains authoritative.**
Nothing in the authentication path may consult the marker.

**No marker write is reachable from a rejected attempt.** On the one-shot path
the marker convergence sits *below* `pre_check_audited`, deliberately: a write
means a temp file, a `chown`, a `rename` and possibly a `mkdir`, and putting
that above the gates would let an attempt rejected as disabled / SSH / lid /
rate-limited / non-IR drive filesystem work from the wrong side of the rate
limiter.

There is exactly one operation on the rejection side, and it is a **removal**:
when the database authoritatively reports zero models for the user, the
one-shot unlinks a marker that claims otherwise (it must, because a daemonless
install has no `reconcile_all` to prune with, so the marker would otherwise be
permanent). It creates nothing, `chown`s nothing, and touches one validated
path component under `enrolled/`. It is idempotent — a repeat attempt finds
`ENOENT` — and it fires only when the marker is already false, so it can delete
a stale marker and never a correct one. Note that the enrollment gate runs
*before* the rate-limit check, so this genuinely runs unmetered; the bound (one
`unlink` per attempt, nothing accumulating) and the "cannot delete a correct
marker" property are what make that acceptable, not the rate limiter.

#### B. Embedding Sensitivity Warning (Required)

Face embeddings are **biometric data**. Unlike passwords, they cannot be changed. Document this:
- The database contains irreversible biometric templates
- If compromised, the user's face embeddings cannot be "rotated" like a password
- Embeddings should be treated as sensitive personal data

#### C. Encryption at Rest (Implemented — encrypted by default, Plan 04)

Face templates are **encrypted at rest by default** (`encryption.method = "keyfile"`, finding
#8). Embeddings are AES-256-GCM encrypted with a 256-bit key held in a key file
(`keyfile`) or a TPM-sealed key (`tpm`). The TPM method seals the AES key at rest; it is
unsealed at daemon startup and held in memory.

**The keyfile is created for a database that holds no encrypted template, and only then**
(#231). "Encrypted" is decided by each stored blob's own version byte, not by the `sealed`
column, which cannot tell a TPM-sealed row from a keyfile-sealed one. A missing key over
encrypted rows is refused: those rows are unreadable either way, but writing a replacement
makes a later restore of the real key useless, and that is the one loss facelock can still
prevent. A store that *cannot be asked* counts as encrypted rows existing — "no rows" and
"no answer" are different facts, and only the first authorizes writing a key. Every writer
of that key goes through the same gate: the daemon, `facelock auth` and the other one-shot
commands, `facelock setup`'s automatic policy, and both branches of `facelock encrypt`
(`--generate-key` included, which replaces a live key rather than creating one).

**The check and the write are one exclusive transaction on the store**, so the question and
the act cannot be separated by an enrollment. Enrollment persists a template in a single
store transaction of its own, so it either commits before the check looks — where the
refusal sees it — or waits until the key is in place. Without that section a plain read
would not even observe the commit: under WAL it reads the snapshot it opened on. A store
that cannot be locked is a refusal on the same reasoning as one that cannot be queried, and
says which of the two happened.

Creation is atomic and never destructive. The 32 bytes are written to an `O_EXCL |
O_NOFOLLOW` temporary beside the key at mode `0600` in a single `open(2)` create-at-mode
(no create-then-`chmod` window — finding #11), flushed, then moved onto the key path with
`renameat2(RENAME_NOREPLACE)` and the parent directory flushed. Two daemons starting at
once therefore resolve to one key rather than each overwriting the other's, and no reader
ever observes the key path holding a partial file. A **symlink at the key path is its own
refusal** on every path, creating and reading alike: the reader opens `O_NOFOLLOW`, so a
planted link is never followed to whatever it names.

**What a refusal does.** Enrollment fails closed — facelock will not store a biometric
template as plaintext because encryption is unavailable. Authentication is unaffected and
keeps falling through to the password prompt, and a user whose own templates are plaintext
still authenticates normally even while another user's encrypted row holds the refusal
open. The auth path reports the missing key, never "the database is corrupt".

**Recovery** is to restore the key artifact for the method that wrote the rows —
`encryption.key_path` for `keyfile`, `encryption.sealed_key_path` for `tpm` — from backup.
The daemon re-reads the key on the next authentication or enrollment attempt, so a restore
lifts the refusal live without restarting anything. The destructive alternative, for an
operator who has no backup, is `facelock clear` followed by re-enrollment.

**Known limits.**

- The gate asks the database named by `storage.db_path`. A configuration that points at the
  wrong path finds an empty database, correctly concludes nothing would be orphaned, and
  mints a key. `facelock setup`'s interactive `--encryption` choices carry a stricter guard
  (`handle_orphan_models_before_keygen`, which refuses on *any* stored model and offers to
  clear); the automatic policy does not, because on a genuine fresh install there is
  nothing to distinguish it from.
- Under `ProtectSystem=strict` the shipped daemon unit cannot write `/etc/facelock` at all,
  so it cannot create `/etc/facelock/encryption.key`: on that unit the key is created by
  installation or by a privileged `facelock setup` / `facelock encrypt` run, and the daemon
  only ever reads it. A daemon that finds no key refuses enrollment as above rather than
  falling back to plaintext.

Plaintext storage (`method = "none"`) is an explicit opt-out: enrollment **refuses** to write
plaintext biometric templates unless `security.allow_plaintext = true`, and warns prominently
when it does. This never affects auth — a decrypt failure degrades to the password fallback,
never a lockout.

Enrollment persists a template only once every accepted embedding has passed the capture
gates and been sealed, in one store transaction that also replaces the previous same-label
model (#308); a cancelled or failed enrollment leaves the store untouched, so a partial
template written by this version can never authenticate and a failed re-enrollment keeps
the template it was replacing. Templates the old flow left behind are covered in
`docs/contracts.md`, "Enrollment atomicity".

**In memory**, the long-lived compare sets are held in a drop guard (`Wiped` in
`facelock-core`) that zeroizes them on every exit path, unwind included: the authentication
loop's caller buffer and device-filtered compare set, the daemon's preview compare set
(loaded once per preview session — not once per frame — and wiped when the camera closes,
the store changes through the daemon, or the user changes), and the sets held by the
`facelock bench` and `facelock-bench` benchmark paths, and the live-capture probe sets both
`calibrate` implementations accumulate from the camera. The transient copies in the load and
decrypt paths are guarded too (#293): the decrypt loop and the store's plaintext fast load
accumulate inside the guard, so a row failing mid-load (AAD mismatch, TPM unseal failure,
corrupt row) zeroizes the rows already collected, and buffer growth wipes the outgrown
allocation; the compare set is built through the guard at exact capacity; and `facelock tpm
decrypt` holds each row's unsealed bytes in `zeroize::Zeroizing`. Still unguarded: the
per-frame embeddings inference returns before they are consumed, the per-row blob buffers
SQLite hands back while a plaintext store loads, and the raw-row snapshot `facelock tpm
encrypt` reads before sealing one.

**Hard device binding (opt-in, `security.bind_device_aad`).** When enabled, the enrolling
camera's `device_id` is folded into the AES-GCM Additional Authenticated Data, so a template
sealed under one camera cannot be decrypted under another — the cryptographic complement to
the advisory device coupling in §1.D. Default off: hard binding fails closed on unstable or
absent device ids, so it is opt-in only.

*Fail closed at enrollment (#312).* With the flag on, enrollment on a camera with no usable
USB identity (no non-empty canonical `device_id`) is refused before any model or embedding is
written, in the daemon and the direct path alike; the error names `security.bind_device_aad`
and the way out (disable it, or use a camera that reports `idVendor` and `idProduct`). The
enrollment loop checks again on its own (`Config::require_device_aad`), so no caller can
seal an unbound template under the flag. Without the flag, templates are sealed with no AAD,
which the cipher treats as an empty AAD: that equivalence is the ordinary-encryption contract
and is pinned by a test in `facelock-tpm`. The flag needs an encryption method to act on:
under `encryption.method = "none"` it is inert (no refusal, no classification). The gate
checks that a device id exists, not that it is unique: a non-empty serial is not necessarily
unit-unique (§1.D), so hard binding is only as strong as the identity the camera reports.
Integrated MIPI/CSI cameras (IPU6/IPU7) report none, so enrollment under the flag is refused
on them.

*Legacy unbound rows, never a lockout.* A row with a NULL or empty `device_id` (enrolled
before the flag, or before device coupling existed) was sealed with no AAD, and decrypts with
none: it keeps authenticating after the flag is turned on, as long as every other row in the
user's store still decrypts (see *Mixed stores*). It is classified `LegacyUnbound`
rather than passed off as bound, and reported in three places: the daemon logs a warning
naming the model ids at each compare-set load, `facelock list` shows `unbound (re-enroll to
bind)` in the Camera column, and `facelock status` lists it as `#N: label, unbound (re-enroll
to bind)`. Re-enroll the user on the camera to bind the template. A row that *does* carry a
`device_id` but was sealed before the flag was enabled has no AAD to match the one now derived
from its id, so it fails to decrypt; the error says so and names the re-enrollment that fixes
it. Enabling the flag over an existing store therefore means re-enrolling every user on it.

*Mixed stores.* The first failing row fails the whole load (a partial compare set would
silently narrow authentication), so a user whose store mixes NULL rows with id-bearing rows
sealed before the flag loses both kinds until the id-bearing rows are re-enrolled. The unbound
diagnostic is logged before decryption, so that failed load still names the NULL rows.

*Turning it off.* Setting `bind_device_aad = false` (or switching `encryption.method` to
`"none"`) over a store sealed under the flag is the mirror image: every hard-bound template
stops decrypting, because the AAD it was sealed under is no longer supplied. `facelock list`
and `facelock status` keep reporting those rows as bound (they carry a device id). `facelock
tpm decrypt` does not support hard-bound templates: `run_decrypt` unseals without device AAD,
so it can never recover them, flag on or off. The way back for a hard-bound row is
re-enabling the flag (authentication resumes) or re-enrolling; a plaintext export of
hard-bound rows is not available (tracked as a follow-up). The refusal message's `= false`
remedy says the same, since it changes new enrollments only. `facelock tpm encrypt` refuses
to run while the flag is on: it re-seals rows from a query that carries no device id and
would manufacture id-bearing rows with no AAD.

**TPM PCR binding is implemented and enforced when enabled; it is opt-in by default
(`tpm.pcr_binding`, finding #5).** With `tpm.pcr_binding = true`, the sealed key object is
created with `userWithAuth = false` and its PCR selection is recorded in the sealed blob
(version byte `0x03`). Unseal starts a *real* policy session and replays `PolicyPCR` against
the current PCRs, so a firmware/kernel change to a bound PCR makes the key refuse to unseal
(face auth then falls through to password).

**Recovery is `sudo facelock tpm reseal`.** It re-seals the AES key under the current PCR
state, recovering the key by unsealing the existing blob when the PCRs still match, and
falling back to the plaintext `encryption.key` backup once they have moved. Because it prefers
the still-valid blob, it is safe to run proactively before a firmware or kernel update.
`facelock tpm unseal-check` reports which of the two recovery paths a machine is on.

**Why the default stays off.** The recommended setup keeps a plaintext `encryption.key`
backup, so a PCR change costs a reseal instead of a re-enrollment. While that backup exists,
PCR binding buys nothing against an attacker with disk access: the AES key sits beside the
sealed blob in `/etc/facelock`, protected only by that file's `0600` root-only mode. Binding
is worth enabling when the backup lives off the machine and the operator accepts
re-enrollment as the failure mode. See `docs/configuration.md` for the `[encryption]` and
`[tpm]` sections, and `docs/cli.md` for `facelock tpm reseal`.

#### D. Debian purge traversal (Implemented)

Package purge is a privileged deletion boundary, separate from ordinary
removal and from the explicit CLI data-purge workflow. The Debian `postrm`
enumerates only `/etc/facelock`, `/var/lib/facelock`, and
`/var/log/facelock`; it never grants deletion authority to configured paths.
Before dpkg removes the conffile, the `remove` phase reports any configured
model, database, key, sealed-key, audit, or snapshot path outside those roots.
The bounded classifier accepts canonical section assignments and dotted or
quoted key and table components while retaining their active TOML table scope.
It reports a controlled classification warning for valid representations
outside that grammar, including multiline strings. An unsafe configuration
object reported as retained is protected from the later generic walk. The
`purge` phase repeats the report when the conffile still exists but never opens
an external value.

PAM rollback state has a separate owner. If
`/var/lib/facelock/pam-backups` remains nonempty after the binary-backed PAM
cleanup, `postrm` treats the directory as an opaque subtree and retains it
whole. It does not reinterpret provenance or delete safe-looking children; an
empty trusted directory may be removed normally.

Every traversed root and directory must be a root-owned,
non-group/world-writable directory chain on one device. Every removable leaf
must be a single-link regular file with compatible ownership and modes. The
one narrow exception is a direct user-owned enrollment marker beneath
`/var/lib/facelock/enrolled`, whose owner-only mode and protected parent make
its provenance distinguishable from a wrong-owner file elsewhere. Symlinks,
other hard links, special files, unsafe ownership or modes, and inspection or
unlink failures are retained and named.

The helper opens every fixed-prefix and root component with
`O_DIRECTORY|O_NOFOLLOW`, keeps those descriptors pinned, and opens regular
candidates with `O_NONBLOCK|O_NOFOLLOW`. Mount IDs from `/proc/self/fdinfo` are
checked against `/proc/self/mountinfo`, which detects same-device bind mounts
that an `st_dev` comparison alone misses; each descendant's device is also
compared directly with the opened root. The fixed chain and opened descendant
chain are rechecked immediately before and after quarantine.

Safe regular files and empty descendant directories are moved within their
trusted parent with descriptor-anchored x86-64
`renameat2(RENAME_NOREPLACE)`, reopened at the admitted quarantine name, and
proved to retain the inspected identity before unlink or `rmdir`. Quarantine
collisions are preserved and reported, and recovery uses the same no-replace
primitive so a public replacement cannot be overwritten. The supported Debian
artifacts are `Architecture: amd64`; an unavailable syscall fails closed rather
than degrading to check-then-rename. Failed unlink or `rmdir` attempts restore
the proven object when possible. A still-open regular inode whose link count is
nonzero after quarantine unlink is reported as an external hard-link remnant.
Directories containing any refused child are never moved. The helper never
removes the three compiled root directories, so no helper deletion operation
is authorized in their out-of-bound parents. After the helper returns, native
conffile purge may remove an empty `/etc/facelock` directory owned by dpkg;
the state and log roots remain helper-owned anchors and may contain reported
opaque or refused remnants.

Root ownership and the prohibition on group/world-writable traversal parents
form the concurrency boundary as well as the provenance boundary. Linux has no
identity-conditional unlink or directory removal operation. After the final
quarantine proof, package purge therefore assumes no concurrent root-equivalent
writer substitutes that trusted name at the deletion syscall. The service is
stopped for the package transaction and unprivileged users cannot mutate those
parents; a deliberately concurrent same-authority writer is outside this
maintainer-script contract.

Traversal is iterative and bounded per compiled root to 64 descendant
directory levels and 10,000 inspected entries. A directory beyond the depth
limit is retained and reported while safe siblings remain eligible. Reaching
the node limit stops that root and reports the whole root subtree. The helper
otherwise cleans safe siblings around a refused child and returns success after
reporting remnants so an unsafe filesystem object cannot strand dpkg. This is
safe unlinking, not secure erasure: storage firmware, snapshots, backups,
journals, and remapped blocks remain outside its guarantee.

### 4. D-Bus IPC Security

**Attack**: Unauthorized user sends D-Bus messages to the daemon to trigger auth, enroll faces, or extract data.

**Mitigations**:

#### A. D-Bus System Bus Policy (Required)

Access to the daemon is governed by the D-Bus system bus policy in
`dbus/org.facelock.Daemon.conf`, installed to `/usr/share/dbus-1/system.d/`
and enforced by the bus itself (dbus-daemon or dbus-broker). Package
transactions never overwrite a legacy `/etc/dbus-1/system.d/` copy. Setup
removes one only when it is an exact reviewed historical Facelock file;
modified or linked copies are preserved and reported. D-Bus merges policy
fragments rather than selecting one winner, so setup also reports unrelated
local policy files without modifying them or claiming the package policy
overrides them. Two grants (ADR 010):

- **root**: may own the name, send anything on the interface, and receive the
  daemon's signals.
- **every local user** (`default` context): may send exactly one method,
  `org.facelock.Daemon.Authenticate`. Screen lockers and the polkit agent run
  their PAM stack as the user, so this is what lets face unlock work with no
  group and no re-login. Everything else stays denied at the bus.

There is no group policy: the `facelock` group is retired (ADR 010) —
packaging no longer creates it and `facelock setup` removes a leftover one.

Because the bus lets any local user reach `Authenticate`, the daemon's own
check — it verifies the caller UID via `GetConnectionUnixUser` on every method
call and applies method-level authorization — is the boundary for that method
rather than a second layer:
- `Authenticate`: root, or a non-root caller acting on their own username. This is the **only** user-scoped method, and it is architecture rather than policy: screen lockers run their PAM stack as the user, so a user must be able to request authentication for themselves.
- Everything else is **root only**: `TestAuthenticate`, `Enroll`, `ListModels`, `RemoveModel`, `ClearModels`, `PreviewFrame`, `PreviewDetectFrame`, `ListDevices`, `ReleaseCamera`, `Ping`, `Shutdown`.

What opening `Authenticate` exposes, and why it is acceptable: any local UID
may ask the daemon to authenticate **itself**. The daemon authorizes that
caller/target pair before refreshing its idle timer or touching handler reload,
SQLite, audit, or capture state. It then charges every authorized non-root
request to a server-local per-caller-UID token bucket before any of that shared
work: a burst of 10 requests, followed by one token restored per monotonic
second. The bucket is charged whether the request later finds no models, fails
another preflight gate, meets a busy camera, or reaches capture. UID 0 is
trusted and exempt; `TestAuthenticate` remains root-only and does not use this
budget.

The ingress bucket is an availability control, not the persistent biometric
guess limiter in § C. It is intentionally independent of
`security.rate_limit`: that SQLite-backed per-target-user limiter still charges
only a camera-backed failure where a face was detected, and daemon restart
still does not forgive those guesses. The ingress state is daemon-local and
monotonic, drops fully refilled stale buckets, and is capped at 1024 UID entries
with least-recently-seen eviction. The cap bounds memory; it is not a global
many-UID quota, so distinct local accounts retain independent buckets. Root is
exempt because one root bucket would couple concurrent PAM authentication for
different target users. If the bucket state mutex is poisoned, every later
non-root admission fails closed as the same recoverable in-band rate-limit
response until daemon restart; the daemon never resumes from possibly
half-mutated ingress state.

With `security.abort_if_ssh = true`, an admitted non-root `Authenticate` must
also carry a live process identity from the bus daemon's
`GetConnectionCredentials` `ProcessFD`. The daemon derives the PID only from
that pidfd's `/proc/self/fdinfo` metadata, checks pidfd liveness on both sides
of logind's `GetSessionByPID`, and accepts only a session whose `Remote`
property is false. It does not use the racy numeric `ProcessID` credential and
does not require a security label. Missing, malformed, dead, remote, and
unverifiable identities and expiry of the four-second provenance deadline
(covering credentials, ProcessFD validation, and logind) all produce the same
AccessDenied message on the wire; the detailed reason is
confined to the privileged daemon journal. Credentials and login1 are queried
asynchronously without retaining the handler mutex. Caller departure,
`ReleaseCamera`, suspend, and shutdown cancel a pending query, so a stalled
system-bus reply cannot pin later handler operations or daemon shutdown. A
caller that exits while logind is being queried cannot authorize a process
that later reuses its numeric PID because the original pidfd is rechecked
before the answer is used.

UID 0 bypasses only that remote-session provenance check; it still uses the
ordinary `Authenticate` authorization and the SQLite-backed biometric-guess
limiter. When `abort_if_ssh = false`, no ProcessFD, PID, logind, or session
lookup occurs. `TestAuthenticate` is separately root-only. The one-shot PAM
fallback retains its environment check, using only the explicitly forwarded
`SSH_CONNECTION` / `SSH_TTY` variables.

An unenrolled UID is answered by the audited `pre_check` from SQLite
(`has_models`) before the global capture slot is claimed. Saturated ingress and
busy-camera rejections happen before that audit writer, which bounds audit-file
pressure from one non-root UID to the bucket's refill rate after its initial
burst. No UID can name another user (`require_user_authorized`), learn another
user's enrollment, or see a similarity score (redacted for non-root). This is
the same shape as fprintd, whose bus policy admits every user and whose daemon
authorizes per call.

The remaining cost, stated plainly: any local UID can still bus-activate the
daemon (`StartServiceByName` is open to every context in `system.conf`) and can
make bounded, cheap self-authentication requests. Distinct local accounts can
combine their independent refill rates; the hard memory cap does not claim to
be aggregate admission control. Face unlock continues to fail closed to the
password. The first admitted request after a config-file mtime change may pay
a handler rebuild, but that mtime is claimed before the attempt: a failed
rebuild keeps the old handler and is not retried until the root-owned config
file changes again. A completed rebuild is generation-checked before
installation; if a newer mtime was claimed while it was building, the stale
handler is discarded rather than reactivating older security configuration.

The scope table's catch-all arm is root-only, so a method added later is closed until it is deliberately opened up. Two entries are spelled out explicitly rather than left to that catch-all, because their root-only scope is load-bearing rather than incidental:
- `PreviewDetectFrame` runs per-frame with neither `pre_check` nor the rate limiter. For any weaker caller it would be a continuous similarity feed at camera framerate; together with score redaction, denying non-root callers closes the hill-climbing oracle by construction (see A5 below).
- `TestAuthenticate` is the entry point that does *not* charge the rate limit, which is exactly why it is only safe to offer to root.

The policy also self-contains two explicit defaults rather than relying on system-wide bus defaults:
- `<deny own="org.facelock.Daemon"/>` in the default context (name-squatting protection; only root may own the name).
- `<deny receive_sender="org.facelock.Daemon" receive_type="signal"/>` in the default context; the only allow is in the root block (see below).

#### A2. PAM Peer-UID Verification (Required)

The trust check runs in both directions: before trusting an `Authenticate`
reply, the PAM module resolves the owner of `org.facelock.Daemon` via
`GetNameOwner`, verifies that owner's UID is 0 via `GetConnectionUnixUser`,
and pins the method call to the owner's *unique* bus name so the owner cannot
change between check and call. If the name is owned by a non-root process
(e.g. because the bus policy file was misconfigured or replaced), the module
refuses the reply and degrades — it never returns `PAM_SUCCESS` on the word
of an unverified peer. This removes the single point of failure on the
`org.facelock.Daemon.conf` policy file.

#### A3. In-Band Recoverable Error Encoding (Required)

Recoverable authentication errors (rate limited, IR required, camera or
storage failure) are returned in-band as `AuthResult { model_id: -2, label:
<message> }` rather than as D-Bus errors. A D-Bus error reply is
indistinguishable from "daemon broken", which would make the PAM module fall
back to a fresh root oneshot attempt — silently escalating past daemon-side
state such as the rate-limit window. With in-band encoding the PAM module
classifies the error itself: rate limited maps to `PAM_AUTH_ERR` (face-auth
budget exhausted, password modules still run), everything else to
`PAM_IGNORE`. D-Bus errors remain reserved for authorization failures and
transport-level problems, which do fall back to the oneshot path.

Daemon-side, the rejection's *class* is a type
(`facelock_daemon::auth::ErrorKind`) and the message is rendered from it; the
audit label and the oneshot exit code derive from the class, not from the text.
PAM still matches the message because it cannot link the daemon crate, so the
two strings it matches (`rate limited`, `IR camera required`) are frozen
protocol — see docs/contracts.md, "Rejection classes".

The oneshot transport carries the same classes in the exit code (3 = rate
limited, 4 = suppressed, 5 = all frames dark; docs/contracts.md, "facelock
auth Exit Codes"), and the module maps them to the same PAM consequences it
gives the daemon's replies. Daemon unavailability therefore no longer softens
a rate-limited rejection to `PAM_IGNORE` on the fallback path.

A non-match reply also states explicitly whether a face was detected
(`model_id == -4`). PAM used to infer that from `similarity == 0.0`, which is
wrong for any non-root caller because the score is redacted to `0.0` for all of
them; a user-run locker therefore abstained on genuine non-matches. The
face-detected bit is a detector signal, not a matcher signal — it says a face
was present, never how close it came — so it is not a hill-climbing oracle and
is not redacted.

#### A4. Auth-Attempt Signal Hygiene (Implemented)

**Attack**: Any local user adds a match rule (or runs `dbus-monitor`) and passively observes `AuthAttempted` broadcast signals to learn who authenticates when — and, if the payload carried the raw similarity score, uses it as a spoof-tuning oracle (iterate on a photo/mask until the score climbs).

**Mitigations**:
- The `AuthAttempted` signal payload is `(user: s, matched: b)` only. It **never** carries the similarity score; the raw biometric score is available only in the `Authenticate` method reply to a **root** caller; it is redacted to `0.0` for every non-root caller (ADR 010 opened `Authenticate` to every local user).
- The bus policy denies delivery of the daemon's signals in the default context; only root may receive them.

#### A5. Raw Frame Access Parity (Implemented — root-only)

**Attack**: `PreviewFrame` is root-only, but any local user pulls raw camera/IR frames through the weaker-gated `PreviewDetectFrame` "detect" variant instead — silently, with no user consent.

**Mitigation**: both methods are root-only. `PreviewDetectFrame` runs per-frame with neither `pre_check` nor the rate limiter, so for any weaker caller it would be a continuous similarity feed at camera framerate; `authorize_method` therefore denies every non-root caller with `AccessDenied` before the method reaches the camera or the capture slot.

**Fail closed**: on top of that denial the daemon strips `jpeg_data` from any non-root reply (`sanitize_preview_jpeg`), leaving detection/recognition metadata only. That strip is unreachable while the method stays root-only; it is kept deliberately, so a future regression in the authorization table cannot turn into raw camera/IR imagery on the wire.

**Residual — similarity in detect metadata (accepted, root-only).** The response returns per-face recognition metadata — bounding boxes, confidence, and the recognition *similarity* score — so the enroll/preview UI can give live quality feedback ("your face is recognized well, hold still to capture"). A raw similarity score is a spoof-tuning oracle in general (iterate a photo/mask until the number climbs), which is exactly why A4 removed it from the broadcast `AuthAttempted` signal. Here it reaches root only, and the score is redacted for any non-root caller regardless (defense in depth against the same regression). A future option is to bucket the score (`weak`/`good`/`strong`) if even the root-only exposure is ever deemed too precise.

#### A6. Capture Contention Guard (Implemented)

**Attack**: Local DoS — a caller loops `Authenticate`/`PreviewDetectFrame`, keeping the global handler mutex held so every other caller (including root) queues up to the 10-second handler-lock timeout per request. Under ADR 010 any local UID may call `Authenticate` for itself, so for that method the caller set the guard bounds is every local account, not root and the (pre-ADR 010) `facelock` group; `PreviewDetectFrame` remains root-only.

**Mitigation**: A cheap in-flight capture guard rejects a concurrent
`Authenticate`/`Enroll`/`PreviewFrame`/`PreviewDetectFrame` **immediately** with
a `daemon busy` error instead of queueing. Authentication also rechecks that
guard while waiting for the handler, closing the race between the initial
check and mutex acquisition. Its camera-independent, audited preflight then
runs while holding one handler generation, and only a request that passes may
claim the global capture slot; an unenrolled caller therefore never occupies
it. PAM treats a busy response like any daemon error (`PAM_IGNORE`) and falls
through to password — degraded, never locked out. The server-local ingress
bucket above charges every authorized non-root request before the busy check;
the persistent biometric-guess limiter remains unchanged and orthogonal.

#### B. D-Bus Message Size Limits (Enforced by Bus)

The D-Bus bus daemon enforces message size limits (typically 128MB by default, configurable in the bus configuration). This prevents oversized messages from consuming daemon memory without requiring application-level size checks.

#### C. Persistent Rate Limiting (Implemented)

Throttle authentication attempts to prevent brute-force:

```rust
let rate_limiter = RateLimiter::new(5, 60);
if !rate_limiter.check(&store, user)? {
    return Err("rate limited");
}

// ... authentication attempt ...

if auth_failed {
    rate_limiter.record_failure(&store, user)?;
}
```

Implementation note:
- Failed attempts are stored in the shared SQLite `rate_limit` table
- Daemon mode and oneshot mode use the same window and thresholds
- Restarting the daemon must not reset a user's lockout state

**An attempt where no face was detected is not charged** (ADR 008 §4). The
limiter exists to bound *guessing* — presenting material to the camera and
being told no — and an empty chair presents nothing. Charging it made the
budget consumable without any attacker input: a lock screen that starts face
auth on every wake, a laptop opened facing an empty desk, or an unattended
`sudo` prompt would each spend the five attempts on nobody, so the user's real
attempt met a lockout. The distinction is the detector's, not the caller's
(`face_detected` on the result, the `-4` versus `-1` wire sentinel), so it
cannot be asked for: an attacker who wants a free attempt has to keep their
face out of frame, which is also an attempt that could never have succeeded.
A detected face that fails any later gate — no match, IR texture, frame
variance, landmark liveness — is charged as it always was. A no-face attempt
also *ends* after `recognition.no_face_timeout_secs` (default 2) instead of
running the full `recognition.timeout_secs`, which turns the camera and its IR
emitter off that much sooner.

### 5. PAM Module Hardening

#### PAM service writer and backup provenance

`facelock pam add` and `remove` accept only a single service-name component
and resolve it again beneath the configured PAM roots using directory-relative
no-follow operations. Service entries must be regular files with one link.
Immediately before publishing a replacement, the writer compares the opened
file's device, inode, link count, and SHA-256 hash with the phase-one plan. An
existing override is published with an atomic exchange: the exact displaced
inode stays open and is checked again, an intervening administrator or package
replacement is exchanged back, and only the verified displaced inode is
unlinked. During that bounded check, the complete new PAM document may be
briefly visible, but a mismatch restores the complete intervening document;
neither side is partially written. The file and parent are fsynced, and PAM's
password fallback is unchanged. A vendor-only service is published into the
override root with a no-replace rename, so an administrator file that appears
after planning is preserved. Vendor bytes do not donate a SELinux xattr to the
local override: the override directory's create/type-transition label applies.

Rollback copies live in the root-only
`/var/lib/facelock/pam-backups` directory, not in a PAM configuration
directory. That path is a fixed PAM trust root and does not move with a custom
`storage.db_path`; it is descriptor-opened without following links and is
repaired and rechecked as `0700 root:root` before recovery or mutation trusts
its entries. Each `0600 root:root` backup has the exact name
`<service>.<seconds>-<nine-digit-nanoseconds>` and an adjacent strict,
versioned JSON record. Version 1 records contain `version`, a positive
monotonic `sequence`, `prepared` or `committed` state, a confined `service`,
the `backup` basename, and `original_sha256`/`installed_sha256`. Records are
limited to 16 KiB and backup reads to 1 MiB before allocation. The record never
contains a target path and is treated only as a hint: recovery scans the state
directory, validates regular single-link files and hashes, rejects duplicate
or overflowing sequence order, and re-resolves the service under the PAM write
root. Installed bytes promote a prepared record, unchanged original bytes
discard the unused Facelock pair, and any mismatch is preserved for manual
inspection.

Every multi-name mutation first publishes a strict, path-free durable intent.
The reserved roles are `prepare`, `commit`, `cleanup`, `pam-replace`,
`pam-remove`, and `vendor-create`, with names derived from a validated strict
transaction basename. The backup-pair roles bind that basename to the backup;
the last two roles use it only as a collision-resistant operation key and do
not create a rollback pair. Role validation also pins which record hash,
replacement-record hash, and original file identity fields must be present or
absent. Commit and existing-file PAM publication use exchanges; vendor
creation uses a no-replace rename; cleanup moves both state entries into
no-replace quarantine names before unlinking. One state-directory flock spans
recovery, sequence and name allocation, backup persistence, PAM publication,
and provenance commit, so recovery cannot discard a prepared pair from an add
that is still in progress. Recovery validates the intent's role, sequence,
derived names, bounded hashes, and recorded identity where applicable before
it resumes or removes anything. Hash-bearing state-write temp names are
likewise removed only when their exact destination role, owner, mode, link
count, and contents validate. Thus a crash at any prepare, PAM-directory temp,
exchange, no-replace publication, quarantine, or unlink boundary has a
deterministic next action, while an ambiguous lookalike is retained.
Default removal deletes only validated committed Facelock pairs and the exact
legacy `<service>.facelock-backup` name; unresolved prepared pairs, malformed
records, symlinks, hard links, and unrelated administrator backups are never
followed or removed.
For an unchanged Facelock-created vendor override, named removal first uses the
bound `pam_remove` exchange and retains the exact published identity. It
then moves that exact inode from its canonical service name to the derived
`.facelock-vendor-retire-<transaction>` quarantine with a no-replace rename
while the same state transaction lock and publication evidence remain live.
It rechecks the bounded quarantine identity, canonical absence, exact two-line
header, payload, owner/mode, exact single-rule emitted document (or exact
no-rule restart shape), and current regular single-link vendor file before
checked unlink. Current vendor resolution opens later roots in order and stops
at the first existing service; malformed, linked, unreadable or oversized
higher-priority entries block cleanup. The header is parsed only against that
resolved path and is never opened as a recorded path. If no current source
resolves, an exact header path derived from a normalized configured later-root
candidate is recognition-only: the local override is retained and the absent
source is reported. An arbitrary recorded path is not accepted.

If validation fails while the canonical name is absent, the exact quarantine
is restored by no-replace rename. A concurrent canonical entry, quarantine
collision, root-reopen failure, identity mismatch or durability uncertainty
preserves the names and intent/binding evidence. Recovery resumes quarantine,
restore and unlink boundaries. Any local or vendor drift preserves the local
copy after its Facelock rule is removed. The exact header-bearing no-rule
intermediate is recognized on a restart; merely similar or metadata-drifted
files are not deleted.

Intent filenames use
`.facelock-intent-<hyphenated-role>-<transaction>.json`; the JSON `role`
values for the three PAM mutation roles use serde's snake-case spelling
`pam_replace`, `pam_remove`, and `vendor_create`. Every intent requires
`version`, positive `sequence`, confined `service`, strict `backup` transaction
basename, original/installed hashes, nullable record/replacement-record hashes,
and nullable device/inode/link and mode/uid/gid identity triples. The role
predicate requires record hashes only for backup-pair operations, the
replacement-record hash only for `commit`, the complete stable identity for
existing-file `pam_replace`/`pam_remove`, and the expected destination
mode/uid/gid for `vendor_create`; irrelevant non-null fields invalidate the
intent. Stable identity comparisons bind device, inode, single-link count,
content hash, mode, uid, and gid, but deliberately exclude timestamps and other
mutable metadata. State recovery additionally rechecks `0600` and the fixed
expected state owner on every match; it never adopts the directory's observed
owner as its authority. A same-inode, same-content entry whose mode or
ownership changed is therefore ambiguous and is retained rather than finalized
or removed.
Publication additionally writes a strict, self-contained
`.facelock-publication-<role>-<transaction>.json` binding after the replacement
temp exists and before exchange or no-replace publication. It binds the base
intent hash, role, sequence, service, operation basename, and the replacement's
complete device/inode/link/hash/mode/uid/gid identity. The canonical name is
reopened and full-compared against that identity after publication and before
any displaced inode or intent is removed. A mismatch retains the canonical
name, displaced name, intent, and binding for recovery/manual inspection.
The replacement identity is first captured from the still-open created temp
after its metadata and contents are synced. Facelock then reopens the reserved
basename and full-compares it before writing the publication binding; a
mismatch is ambiguous and retains the intent and filesystem evidence. Error
cleanup at that creation boundary uses the same identity-checked unlink and
directory sync rather than unlinking the basename without revalidation.
If no-clobber binding publication fails, Facelock preserves the colliding state
entry and reopens and full-compares the still-unpublished replacement temp. It
removes the base intent only after that exact temp is unlinked and its directory
is synced. Every identity or cleanup ambiguity retains the base intent and the
colliding state evidence. The temp is also retained unless its exact,
identity-checked unlink succeeded and only the subsequent durability sync
failed, in which case the temp name may already be absent.
The same full-identity cleanup applies after the binding is durable if source
drift or an exchange/no-replace failure prevents PAM or vendor publication. A
substituted reserved temp makes that failure ambiguous and retains the base
intent, binding, and all remaining evidence.
Successful cleanup removes the base intent first and the self-contained
binding last, so a crash between those unlinks can still authenticate the
canonical inode. Recovery considers the binding orphaned only when the exact
derived base-intent name is definitely absent; an invalid-mode, invalid-owner,
malformed, mismatching, symlinked, or hard-linked exact entry preserves the
binding. An orphan binding is removed only after the canonical identity check.
PAM-directory temps are `.facelock-pam-replace-<transaction>`,
`.facelock-pam-remove-<transaction>`, or
`.facelock-vendor-create-<transaction>`; vendor retirement uses the exact
`.facelock-vendor-retire-<transaction>` quarantine. State quarantines are the exact
`commit`, `backup`, and `record` role names, and state publication temps bind
their destination basename and content hash in the filename. Backup and record
temp destinations additionally require a confined service component, so empty,
`.` and `..` services are never owned. A reserved name without its complete
role schema, root/state-directory ownership, `0600` mode, single-link identity,
and bounded content hash is not considered Facelock-owned.
If an atomic state temp-to-final rename succeeds but syncing the parent
directory fails, the operation is ambiguous rather than an ordinary create
failure. Every caller propagates that ambiguity before cleanup: prepare keeps
its intent and visible backup or record, commit keeps its intent and named
replacement, and every publication-binding role keeps its intent, replacement
temp, and visible binding. Recovery can therefore classify the complete set;
checked cleanup remains limited to definite failures before the rename.

Machine-wide `pam remove --all` adds one whole-set transaction over these
per-service primitives. It ignores `[pam] config_dirs` and scans only the
compiled `/etc/pam.d`, `/usr/lib/pam.d`, and detection-only `/etc/authselect`
roots by enumerating already-open directory descriptors. Facelock does not
follow generated links: it skips one only when the link's exact absolute target
is the same service beneath a later compiled root that is scanned
independently. Every other linked entry is a blocker, while a reference found
in the independently scanned generated root is an unmanaged external-root
reference. A conventional direct local reference is writable when every
Facelock rule has the exact pre-versioned emitted bytes. A dot-prefixed or
package/administrator artifact name is considered only when an exact strict
provenance basename exists and its current hash matches committed provenance,
or when the regular local file is an exact current Facelock vendor copy;
unowned `.pacsave`, `.rpmsave`, pam-auth-update `.pam-old`, `~` and similar
artifacts are preserved and ignored. Customized rules, corrupt provenance for
a candidate, linked or unreadable entries, and references in read-only roots
are unmanaged blockers;
preflight reports them without following or changing them. Directory contents
remain detection ground truth, and provenance is never a target path or an
instruction to mutate.

The `--dry-run` path validates an existing PAM backup directory read-only. It
requires the directory's trusted owner and mode without repairing or syncing
them, acquiring the write lock, or running recovery; a trust failure preserves
the directory metadata and entries and refuses the preview.

For a clear preflight, Facelock prepares a validated backup/provenance pair for
every target, then atomically publishes one strict, bounded `remove-all`
journal before the first PAM exchange. Version 2 journal and commit targets
contain only confined services, strict backup basenames, full original or
installed identities, installed hashes and a required `delete_override`
boolean. Version 1 recovery requires that boolean to be absent; null, mixed or
mismatching flags invalidate the state. One state-directory flock spans
journal recovery, the complete
preflight, all exchanges, final active-reference rescan, commit publication and
cleanup. A later identity failure or non-empty final rescan exchanges every
earlier displaced original inode back in reverse order. Recovery does the same
for a journal without its commit marker. A strict self-contained commit marker
binds the journal hash and every published full identity; once durable,
recovery completes validated publication/state cleanup instead of rolling the
PAM files back. Ambiguity preserves the journal and per-file evidence.
Only names whose extracted operation has the strict batch timestamp grammar
enter this recovery path; prefix-shaped per-service provenance remains
ordinary provenance. Duplicate services invalidate either journal or commit
before any recovery cleanup.
An intent-only PAM replacement is cleaned as unstarted only when the canonical
file still has the journal's complete original identity, the intent agrees
with the prepared pair, and both exact temp and binding names are absent.
After reverse exchange and identity-checked replacement-temp cleanup, rollback
removes the exact publication binding before delegating the base intent to
that exact intent-only recovery. Each boundary is restartable; normal forward
publication retains its existing intent-first cleanup order.
Rollback-pair cleanup resumes an exact cleanup intent across both quarantine
moves and unlinks; total absence is already clean, while partial, substituted
or conflicting state is preserved.

After a commit is durable, a flagged unchanged vendor override is removed only
if its full committed local identity and the bounded, identity-rechecked
journal backup still have an exact emitted one-rule or no-rule restart shape.
The line-removed backup hash must equal the journaled installed hash, and its
header, payload and metadata must match the first existing current vendor
service in ordered later roots. The shared quarantine protocol completes the
unlink and parent sync before batch evidence cleanup. A missing flagged target
is an idempotent completed unlink on recovery; any drift or unflagged absence
preserves the evidence and requires review.

The uninstall call path reaches this command before removing the CLI or PAM
module and does not parse config or touch the database, models, camera, daemon,
or ONNX Runtime. Debian `prerm` and RPM `%preun` failures abort their package
removals. Booted tests exercise direct `dpkg`/`rpm` plus `apt-get`, `apt` and
`dnf` wrapper abort retention and blocker-free success. Arch packages install
a Remove-only libalpm `PreTransaction` hook whose
`AbortOnFail` action runs the same command, with the package scriptlet retaining
an idempotent second call. Source and Omarchy uninstallers delegate to the same
path. The module is removed only after the compiled-root final scan succeeds.
The all-or-nothing guarantee covers the direct PAM edits owned and scanned by
this transaction, plus retaining the package/module when it fails. Debian's
packaged `pam-auth-update` profile is opt-in (`Default: no`), so fresh install
does not alter `common-auth`. Direct `pam add` and `setup --pam` inspect only
the fixed profile, selection-state and live `common-auth` paths with bounded,
owner/mode/link-checked, no-follow reads. An exact selected and live profile
refuses a duplicate direct edit; any saved-selection/live-graph disagreement,
linked evidence, or modified evidence fails closed. Neither case writes PAM or
backup state.

Debian removal performs that fixed-root profile probe and a read-only
`remove --all --dry-run` preflight before the journaled direct cleanup, and it
reaches generated service lifecycle handling only after cleanup commits.
Ordinary removal stops the daemon but preserves enabled state for reinstall;
the generated purge path alone retires that state. A selected or unsafe shared
profile aborts removal with the package, binary, module, PAM graph, direct edits
and service state retained. The administrator is told to disable
the profile through `pam-auth-update`, prove a real correct password succeeds
and a wrong password fails, then retry. No released predecessor recorded exact
package-auto-enable provenance, so Facelock never guesses that an existing
selection is package-owned and never silently disables it. Automatic legacy
profile migration is intentionally deferred until such provenance can support
an exact graph snapshot, managed regeneration, provenance-owned direct-edit
reapplication, real authentication validation, and provable restoration or
retained evidence. An unselected `Default: no` profile causes no managed-graph
transition when its package metadata is removed.
The RPM retirement guard reads only `/etc/authselect/authselect.conf` before
payload replacement. It requires fixed root ownership, mode, link count and a
16 KiB bound, compares the first line's raw bytes before shell interpretation,
and refuses a selected retired `facelock` profile or any malformed state. It
never invokes authselect, chooses a replacement profile, or edits generated
state. Independently, `pam remove --all` treats `/etc/authselect` as a
detection-only root and never edits it.

#### A0. Config File Trust (Required)

The PAM module runs in a root context, so `/etc/facelock/config.toml` is an
attack vector: a writable config could disable anti-spoofing knobs or change
the daemon mode. (The oneshot binary path itself is fixed to
`/usr/bin/facelock` and is not caller-influenced.) Before parsing, the module
verifies that the config file **and every parent directory** are root-owned
and not group- or world-writable. The file check uses `fstat` on the opened
descriptor so the validated inode is exactly the one read (no TOCTOU). An
untrusted config is treated like a missing config: the module logs the
reason and returns `PAM_IGNORE` (fail closed, fall through to password).

#### A1. Sanitized Oneshot Child Environment (Required)

The oneshot fallback spawns `facelock auth` as root. When the PAM caller is
fully root (euid == uid, no `AT_SECURE`), the dynamic linker honors
`LD_PRELOAD`/`LD_*` from the inherited environment — allowing arbitrary code
injection into a root process. The module therefore spawns the child with
`env_clear()` and an allow-list:

- `SSH_CONNECTION` / `SSH_TTY` — forwarded so the child's own SSH-abort
  check keeps working
- `PATH=/usr/bin:/bin` — pinned, never inherited

Everything else (`LD_*`, `XDG_*`, `DBUS_*`, ...) is dropped. The desktop
notification path constructs its own session-bus address from the target UID
and does not need inherited XDG variables. The child's stdin is `/dev/null`.

#### A. Audit Logging (Required)

Log all authentication attempts with outcomes:

```rust
fn identify(pamh: *mut libc::c_void) -> libc::c_int {
    let user = pam_get_user(pamh);
    let service = pam_get_service(pamh);  // "sudo", "login", etc.
    let result = do_auth(user, service);

    // Log to syslog (PAM convention)
    // Format: pam_facelock(service): auth result for user
    syslog(LOG_AUTH | severity, "pam_facelock({}): {} for user {}",
           service, result_str, user);

    result
}
```

This creates an audit trail in `/var/log/auth.log` or journald.

#### B. Service-Specific Policy (Recommended)

Allow different PAM services to have different security levels:

```toml
[security.pam_policy]
# Only allow face auth for these PAM services
allowed_services = ["sudo", "polkit-1"]
# Never allow face auth for these (always fall through to password)
denied_services = ["login", "sshd", "su"]
```

### 6. Daemon Process Hardening

#### A. Capability Dropping (Implemented — verified, and fatal if it did not happen)

As soon as the two startup steps that need `CAP_CHOWN` are done — the state layout and the
enrollment-marker reconcile — and **before the daemon creates its first thread**, it narrows its
own capability set to [`retained_capability_mask()`](../crates/facelock-daemon/src/server.rs):
`CAP_SETUID` + `CAP_SETGID` and nothing else (`capset` on all three of
effective/permitted/inheritable, plus `PR_SET_NO_NEW_PRIVS`). Those two are the notification
privilege-drop's, and only its; see Phase 3 in §6.B for why they cannot be given up.

**Why that timing is the whole of the guarantee.** Linux capabilities and `PR_SET_NO_NEW_PRIVS`
are per-*thread* attributes: `capset(2)`/`prctl(2)` with `pid = 0` change the calling thread and
nothing else, and a thread that already exists keeps what it had for as long as it lives. (This
is why libcap ships `libpsx` to broadcast a capability change across a process's threads.) The
drop therefore has to happen while the daemon is still single-threaded — before
`FaceEngine::load` brings up ONNX Runtime's intra-op pools, and before the tokio runtime spawns
the workers and blocking threads that every D-Bus method body does its real work on. Narrowing
any later reaches none of them, and `capget` cannot report the mistake either: it is per-thread
too, so it would inspect the one thread that *did* drop and confirm itself. `server::run` takes
a `CapabilitiesDropped` token it cannot construct, so the ordering is a compile-time
requirement rather than a comment.

The narrowing asks only for capabilities the process already holds
(`capabilities_to_keep(permitted)`). `capset` requires the new permitted set to be a subset of
the old and rejects a non-conforming request *wholesale* — dropping nothing at all — so a
daemon started under, say, `CapabilityBoundingSet=CAP_CHOWN` with no ambient caps would
otherwise fail the drop entirely and land in the fatal branch below. Intersecting first means
the call only ever removes, and `CAP_CHOWN` is never in the result.

**The drop is then read back and checked.** `capget` reports what the kernel actually left
behind, and `drop_capabilities_or_refuse()` compares it against the retained mask:

| Outcome | What the daemon does |
|---------|----------------------|
| Nothing beyond the retained set is held | Serve. This is the steady state. |
| Anything beyond it survived (including a `capset` that failed outright) | **Refuse to serve** — exit non-zero before claiming the bus name, so no client ever sees a half-privileged daemon |
| Held set is *narrower* than the retained mask | Warn and serve — desktop notifications may not work, nothing the security model promised is violated |
| `capget` itself failed | **Refuse to serve** — an unverifiable guarantee is not a guarantee |

The check is deliberately one-sided ("is anything *extra* still here?"). A daemon started under
a narrower bounding set than the shipped unit grants simply keeps less than the retained mask;
that costs notifications and must not stop it from authenticating anyone. The ambient set needs
no separate check — the kernel clears a capability from ambient the moment it leaves permitted
or inheritable.

**Why fatal.** This used to warn and continue, which was defensible while the dropped set held
nothing the security model had promised to remove. With `CAP_CHOWN` in the bounding set for
startup (§6.B, #137), a failed drop would leave the daemon serving every authentication with
`chown(2)` in reach and only a journal line to say so. Refusing is not a lockout: PAM degrades to
the password exactly as it does when the daemon is not running at all — the same trade
`ensure_state_layout` already makes earlier in startup, and the same fail-closed convention as
the model SHA-256 check. `Restart=on-failure` will retry it every `RestartSec=3`, which is the
same restart loop a failed `ensure_state_layout` already produces (the unit sets no
`StartLimit*`, so systemd's defaults may not trip on a 3-second interval). Each attempt logs the
capability mask that survived, so the journal says exactly what is wrong rather than only that
something is. Reaching that loop takes a genuine kernel-level refusal to narrow: an operator
running the daemon with fewer capabilities than the shipped unit grants is *not* a failure here,
because the drop only ever asks for what is already held.

**Regression coverage.** `test/pkg-validate.sh` reads `CapPrm`/`CapEff` from
`/proc/<pid>/task/*/status` on the running daemon and asserts `CAP_CHOWN` is clear on **every**
thread. That walk is the point: `/proc/<pid>/status` reports the main thread, which is the one
thread that always drops, so it is exactly the blind spot a per-thread bug hides in. The unit
tests cover the mask arithmetic, and
`server::tests::only_threads_created_after_the_narrowing_inherit_it` pins the ordering itself by
running both orders against a real tokio runtime.

That walk only happens if the daemon actually starts, which needs `models/*.onnx` in the
checkout (they are gitignored). The Debian suite package gates and `just test-rpm-pkg` refuse to run without
them, and `pkg-validate.sh` fails rather than skipping — set
`FACELOCK_ALLOW_MISSING_MODELS=1` to accept a partial run, which then reports the missing
model-dependent assertions in its `N skipped` count instead of passing silently.
Any other skipped assertion fails package validation. The opt-out also names the active-service Debian
upgrade cases it cannot run; inactive versioned upgrades and the clean-base dependency proof
remain mandatory.

#### B. systemd Hardening (Implemented)

The systemd unit (`systemd/facelock-daemon.service`) includes layered hardening:

**Phase 1 (shipped):** `ProtectSystem=strict`, `InaccessiblePaths=/home /root`, `ReadWritePaths=/var/lib/facelock /var/log/facelock`, `PrivateTmp=yes`, `NoNewPrivileges=yes`, `UMask=0027`

**Phase 2 (shipped):** `ProtectKernelTunables/Modules/ControlGroups=yes`, `RestrictNamespaces=yes`, `LockPersonality=yes`, `RestrictRealtime=yes`, `RestrictSUIDSGID=yes`

**Phase 3 (shipped — capabilities, seccomp, network):**

- `CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_CHOWN` /
  `AmbientCapabilities=CAP_SETUID CAP_SETGID` —
  the daemon retains **exactly** the two ambient capabilities while it is serving
  authentications, plus `CAP_CHOWN` for the duration of startup only. Device access needs no
  caps (`/dev/video*` and `/dev/tpmrm0` are root-owned and opened via standard file
  permissions), but the desktop-notification path execs `runuser -u <user> -- notify-send` to
  drop into the user's session bus, and `runuser` calls `setgroups()`/`setuid()`, which require
  `CAP_SETGID` + `CAP_SETUID`. They are declared **Ambient** (not merely in the bounding set) so
  the caps survive the exec into the non-setuid `runuser` under `NoNewPrivileges=yes`. The daemon
  also narrows its in-process capability set to exactly these two once the startup chowns are
  done and while it is still single-threaded (`drop_capabilities_or_refuse()` in
  `facelock-daemon/src/server.rs`, holding them in effective/permitted/inheritable); everything
  else is dropped.
  - **`CAP_CHOWN` is startup-only, and that is enforced rather than attempted.** It is in the
    bounding set but deliberately **not** ambient, and the in-process drop clears it as soon as
    the two startup chowns are done and before the daemon spawns a single thread — so no thread
    of the process holds it while anyone is being authenticated, and no exec'd child can inherit
    it. Root without `CAP_CHOWN` cannot `chown(2)` at all, and two
    startup steps need it on an install that is
    being *upgraded* rather than freshly packaged: `state_layout::ensure_state_layout`, which
    chowns `/var/lib/facelock` and the files under it to `root:root` (a failure there is
    fatal — the daemon exits 1), and the enrollment-marker reconcile, which chowns each marker
    to the user it describes (#137). Both skip paths that are already correct, so a steady-state
    install never chowns. The reachable blast radius is small: `ProtectSystem=strict` leaves only
    `ReadWritePaths=/var/lib/facelock /var/log/facelock` and the private `/tmp` writable, and
    `chown` on a read-only mount fails with `EROFS`. **The drop is verified with `capget` and the
    daemon refuses to serve if anything beyond the retained set survived it** — see §6.A. Until
    #137 the drop was best-effort (`warn!("failed to drop capabilities (continuing)")`), which
    was tolerable only because the dropped set held nothing the security model had promised to
    remove. It does now.
  - **This was empirically required.** An earlier revision set both directives **empty** on the
    theory that the daemon needs no capabilities. That was wrong: on real hardware it broke
    notifications with `runuser: cannot set groups: Operation not permitted`.
  - **Direct-D-Bus-as-root is NOT a viable alternative.** Having root connect straight to the
    user's session bus (skipping setuid entirely) does not work under `dbus-broker`, which rejects
    UID 0 on a user session bus — `sudo DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
    notify-send test` fails with `Error sending data: Broken pipe`. The setuid-via-`runuser`
    path — and therefore these two capabilities — is required for notification delivery.
  - Notifications remain best-effort/fire-and-forget: they never block or fail the auth path, so
    even if delivery fails the biometric result and PAM fall-through are unaffected.
  - **End-to-end delivery is validated only on the maintainer's real hardware under systemd.**
    The unit tests assert the retained capability mask and `systemctl show` asserts the directive
    set; neither proves a notification actually pops.
- `RestrictAddressFamilies=AF_UNIX AF_NETLINK` + `IPAddressDeny=any` — the daemon only talks
  local sockets (system D-Bus, per-user session bus for notifications, kernel netlink). All
  inference is local; a compromised daemon cannot open TCP/IP sockets or exfiltrate over the
  network.
- `SystemCallFilter=@system-service` + `SystemCallErrorNumber=EPERM` +
  `SystemCallArchitectures=native` — allowlist seccomp. `@system-service` includes `ioctl`
  (V4L2), `capget`/`capset` (in-process drop), and the memory-management syscalls ONNX Runtime
  needs. Blocked syscalls return `EPERM` instead of killing the process, so an unexpected
  syscall degrades to a normal auth error (PAM falls through to password) rather than a crash
  loop — never a lockout.
- `ProtectProc=invisible` + `ProcSubset=pid` — other processes and non-PID `/proc` contents are
  hidden from the daemon.
- `ProtectHostname=yes`.

**Intentionally omitted directives (and why):**

- `ProtectClock=yes` — implies `DeviceAllow=char-rtc`, which switches the unit to a
  device-cgroup allowlist and breaks `/dev/video*` camera access (see below). `clock_settime`
  and related syscalls are already denied with `EPERM` by `SystemCallFilter=@system-service`.
- `DevicePolicy`/`DeviceAllow` — cgroup device ACLs interfered with camera auto-detection.
  Standard Unix permissions still restrict `/dev/video*` and `/dev/tpmrm0`.
- `MemoryDenyWriteExecute=yes` — breaks ONNX Runtime JIT paths such as CUDA and TensorRT.
- `User=` — the daemon must open the camera/TPM as root; non-root operation has not been
  validated on real hardware.

**Exposure score:** `systemd-analyze security --offline=true` reports **2.8 (OK)** for the
Phase 1–3 unit, down from 7.1 (MEDIUM) with Phase 1–2 only. (The score rose from 2.2 to 2.6
when the empty capability sets were corrected to `CAP_SETUID CAP_SETGID` — the two caps the
notification privilege-drop genuinely needs; the small increase is the honest cost of a working
notification path. It rose again from 2.6 to 2.8 with the startup-only `CAP_CHOWN` above:
`systemd-analyze` scores the bounding set, so it cannot see that the capability is dropped
in-process before the first authentication.) Verify with:
```bash
systemd-analyze security facelock-daemon.service
```

**Regression coverage:** both Debian suite package gates and `just test-rpm-pkg` boot the package container
with systemd as PID 1 (`test/run-pkg-validate-systemd.sh`) and assert via `systemctl show`
that the installed unit carries the Phase 3 directives, that the daemon starts and answers on
D-Bus inside the sandbox, and that an `AF_INET` socket cannot be created under the same
directive set (outbound TCP blocked).

#### C. Source-install activation barrier (Required)

Replacing a source-installed binary or activation file while D-Bus/systemd can
launch the daemon creates a time-of-check/time-of-use window. The privileged
entrypoint uses absolute program paths and a fixed trusted `PATH`; the recipe
uses privileged Bash mode, rejects startup-hook variables, and holds the
canonical `/run/facelock/lifecycle.lock` for its complete protected interval.
The helper safely creates or validates the `root:root` mode `0755` parent and
captures the never-unlinked lock as a `root:root` mode `0600`, zero-byte,
single-link regular file through a no-follow descriptor proof.

The installer snapshots service state plus persistent/runtime administrator
unit identities, then creates and opens an exact temporary systemd control-tier
mask. It proves the barrier manager-effective before stop and requires the unit
inactive, the bus name unowned, and every ordinary snapshot unchanged before
the first write. The system bus executable, unit, configuration, supported
policy includes, service directories, and selected Facelock definition require
trusted parents and bounded non-writable identities. Only standard system
service-directory topology and D-Bus activation delegated to
`facelock-daemon.service` are admitted; direct activation and custom/unreadable
topology fail closed.

Canonical assets are written only under `/usr`. Before any install write, the
lifecycle records exact identities for all three historical `/etc`
public/quarantine pairs. After canonical writes it stages only digest-allowlisted
historical copies to fixed same-parent quarantines without replacement.
Trusted modified administrator files and systemd masks remain unchanged and
are revalidated throughout. The quarantines remain available for reverse-order
rollback across all later writes and signal/failure cleanup. Parent signal
deferral spans both the staging child and the exact identity record. The child
traps exit and caught signals to reverse its local prefix, while parent cleanup
independently reconciles every preplanned pair if staging ended before that
record. Recovery uses only exact unchanged/staged identities and no-replace
moves; a collision preserves both names and the activation barrier.

On normal completion, systemd and D-Bus reload and prove the canonical winners
while activation remains barred. Only then may exact staged quarantines be
deleted; barrier removal and an initially active daemon's restart follow a
second complete proof. A pre-publication failure restores the original public
or interrupted-quarantine state before manager/D-Bus restoration. A commit
failure rolls back and reloads/proves the original winners while still barred.
Incomplete rollback, a collision, changing identity, partial publication, or
an unprovable reload retains the safest barrier and suppresses restart.

Barrier quarantine/removal uses held descriptors, no-clobber recovery, bounded
retries, and repeated disk, manager, D-Bus definition/configuration, and owner
checks. Linux provides no inode-conditional `unlink`/`rmdir`; the final syscall
therefore excludes an actor with root-equivalent write/search authority over
the trusted parent. Such an actor could already replace the barrier directly.
The protocol covers pre-existing hostile objects and unprivileged races and
never claims isolation from a second hostile root process.

Without systemd the source install aborts before mutation. Only the exact
checked-in offline container-build marker may select the offline path, which
authenticates its copied helpers/manifest, takes the same canonical lock, and
proves the absence of manager, bus, activation, installed-Facelock, and running
Facelock surfaces. Static/mocked and booted PID-1 regressions are respectively
`just test-source-install-daemon-lifecycle` and
`just test-source-install-daemon-lifecycle-systemd`.

### 7. Polkit / sudo Face Auth (Implemented)

Face auth can satisfy polkit and `sudo` authorization through **two independent
deployment models**. They have different scoping semantics, so it matters which
one a given host uses:

| Model | How it's wired | Scoping | Fallback |
|-------|----------------|---------|----------|
| **Agent model** | `facelock-polkit-agent` registers as the session's polkit authentication agent | `polkit.face_eligible_actions` allowlist (below) | Agent declines non-eligible actions |
| **PAM model** (Howdy-style) | `pam_facelock.so` added as `auth sufficient` in `/etc/pam.d/{sudo,polkit-1,…}` | **None** — face is attempted for *every* action under that PAM stack | Password, always (see below) |

Most real installs (including the Omarchy/hyprlock setup this project targets)
use the **PAM model**, because it is the only one that also covers `sudo` and
login. On those hosts the agent allowlist does **not** apply — it is an
agent-only control and `pam_facelock.so` never consults it.

#### 7a. PAM model — accepted posture (unscoped, password-backed)

When `pam_facelock.so` is placed as `auth sufficient` in a PAM stack, **any**
action routed through that stack (every `pkexec`/polkit prompt, every `sudo`)
will attempt a face match first. This is the same posture as a fingerprint
reader or Howdy: the biometric is a convenience factor across the board, not a
per-action capability.

This is **accepted by design**, and safe, because of two invariants the module
guarantees:

- **`sufficient`, never `required`.** A failed or unavailable face match (camera
  busy, no IR, timeout, spoof rejection, rate-limited) falls through to the next
  line in the stack — normally `pam_unix.so` / the password prompt. Face auth can
  only ever *add* a way in, never *remove* the password. Verified on this host:
  `pkexec echo hello` face-authorized; covering the camera fell back to the
  password dialog.
- **All the anti-spoof and trust-boundary defenses still apply** on every one of
  those attempts — `require_ir`, frame-variance liveness, rate limiting, SSH/lid
  abort, and the PAM service allowlist (`security.pam_policy.allowed_services`,
  which *does* gate the PAM path). So "unscoped across actions" does not mean
  "unscoped across defenses."

Operators who want face auth for only some actions under the PAM model should
control it at the PAM layer (which service files include `pam_facelock.so`), not
via `polkit.face_eligible_actions` (which the PAM path ignores). Per-action
scoping *inside* a single PAM stack is not offered; if you need it, use the agent
model instead, or omit `pam_facelock.so` from the stacks you want password-only.

#### 7b. Agent model — action allowlist

The `facelock-polkit-agent` lets a face match satisfy polkit authorization
requests. Two hardening rules keep this from becoming a universal root key:

**Action allowlist.** Face auth is offered only for polkit `action_id`s in
`polkit.face_eligible_actions`. The default is a single low-risk action
(`org.freedesktop.login1.lock-sessions`). High-risk actions — pkexec
(`org.freedesktop.policykit.exec`), PackageKit install/remove, udisks mount, and
accounts-service user administration — are **excluded by default**, so a single
face match cannot authorize arbitrary privileged operations. Users may extend the
list deliberately (like widening a fingerprint reader's reach); an empty list
disables face for all actions.

When an action is not eligible, the agent **declines** (returns a D-Bus
`Failed` error).

> **NOTE (agent model only):** polkit registers a single authentication agent
> per session and does not chain agents. When this agent declines a
> non-allowlisted action it returns an error, which — depending on the desktop's
> agent registration — may present as an authorization denial rather than a
> fallthrough to a password dialog. The intended UX (non-eligible actions handled
> by the desktop's normal password agent) is unverified pending live-desktop
> testing. Behavior is fail-closed: a non-eligible action is never
> face-authorized. This caveat does **not** apply to the PAM model (§7a), which
> always falls through to the password prompt. If the fallthrough UX matters to
> you and is unverified on your desktop, prefer the PAM model.

**Fail closed on unresolved user.** When responding to the polkit authority, the
agent resolves the target username to a uid. If the name does not resolve, the
agent refuses to respond. It never substitutes UID 0 — the previous
`unwrap_or(0)` behavior would have authenticated an unresolvable name as root.

### 8. CI Trust Boundary

The workflows anyone can trigger carry one external credential,
`CLAUDE_CODE_OAUTH_TOKEN`, in two workflows. `release.yml` holds the
publishing credentials and runs only on `v*` tag pushes.
`claude-code-review.yml` runs on `pull_request`, where GitHub withholds secrets
from fork heads, and skips any PR whose head is not this repository.

`claude.yml` answers `@claude` on issues, comments and reviews. Anyone can
raise those events on a public repository, so text alone never selects its
job. The job's `if:` requires, per event, that the author's
`author_association` is `OWNER` or `COLLABORATOR`, read from the payload field
that carries it (`comment`, `review` or `issue`); a payload without the field
fails closed, because `contains(array, null)` is false. `issues: assigned`
gates on the issue author, not the assigner (assigning needs triage or above).
`COLLABORATOR` includes read-only and triage outside collaborators; the action
then requires write access before it does anything. `MEMBER` is not accepted:
it never occurs on a user-owned repository, and after a transfer to an
organization it would admit every member.

The gate decides who starts the job, not what reaches the model. A stranger's
issue body is still the prompt once a maintainer answers it with `@claude`;
the action strips hidden markdown, but read the raw text first.

The two review events (`pull_request_review`, `pull_request_review_comment`)
also require the head repository to be this one, defense in depth on the two
payloads that carry it. `issue_comment` carries no head repository, only the
issue number, so the job-level `if:` cannot see a fork PR there. A guard step
ahead of the action step closes that: when the comment is on a pull request,
it reads the PR with the job's read-only `github.token` and exits non-zero
unless the head repository is this one. The action step, and with it the
OAuth token and the App-token exchange, never runs for a fork; the job that
does start holds nothing but `github.token`. The action refuses actors
without write access on its own, but only after the job has started with the
secret in its environment; the workflow gate keeps the job from being
scheduled at all.

The job's `permissions:` scope `github.token` only: `contents`,
`pull-requests`, `issues` and `actions` as `read`, plus `id-token: write`.
The action does not write through `github.token`. It exchanges the job's OIDC
token for a short-lived Claude GitHub App installation token that requests
`contents`, `pull-requests` and `issues` as `write`, plus whatever the
workflow adds through `additional_permissions` (here `actions: read`), and
hands that token to Claude as `GITHUB_TOKEN`; `claude_code_oauth_token`
covers only the Claude API. `id-token: write` is what makes that exchange
possible, and it is the scope's cost: anything running in the job can mint
OIDC tokens for any audience, including the one that yields the App token.

No workflow uses `pull_request_target`. `test/check-workflow-policy.py` pins,
for every `.yml` and `.yaml` workflow: the per-event author gate and the
read-only `github.token` ceiling on every job of a workflow that subscribes to
an actor-authored event, the accepted set, `issues: types` as exactly
`[opened, assigned]`, the same-repo guard on the two review events, the
fork-head guard step (its position before the action step and its `if:`) in
every `issue_comment` job that runs the action, the same-repo guard and the
same ceiling on every secret-bearing `pull_request` job, a 40-hex pin on
`anthropics/claude-code-action` with its bypass inputs
(`allowed_non_write_users`, `allowed_bots`, `github_token`) refused, and the
`pull_request_target` ban. It also refuses a fork-head guard step that carries
`continue-on-error` and an action step whose `if:` (`always()`, `failure()`,
`cancelled()`, a negated `success()`) or whose job's `continue-on-error` would
run it past a failed guard. It runs in `just check` and in CI.

## Security Configuration Reference

```toml
[security]
disabled = false
abort_if_ssh = true          # Refuse face auth over SSH
abort_if_lid_closed = true   # Refuse if laptop lid closed
require_ir = true            # CRITICAL: refuse non-IR cameras (anti-spoof, load-bearing)
require_frame_variance = true # Reject static images (photo defense; NOT video replay)
frame_variance_max_similarity = 0.985 # Max pair similarity in the sliding window (static >= ~0.999)
ir_texture_min_stddev = 10.0 # Min raw-frame face std_dev for IR texture (flat < 5, real > 15)
require_landmark_liveness = false # Require landmark movement between frames (off by default)
min_auth_frames = 3          # Minimum frames before accepting (variance check)
suppress_unknown = false     # Log unknown faces (true = suppress unknown-face log entries)

[notification]
mode = "terminal"            # Show "Identifying face..." on login screen

[security.pam_policy]
allowed_services = ["sudo", "polkit-1"]
denied_services = ["login", "sshd"]

[security.rate_limit]
max_attempts = 5             # Max auth attempts per user
window_secs = 60             # Rate limit window

[polkit]
# Polkit actions eligible for face auth *under the agent model only*. Non-listed
# actions are declined by the agent. High-risk actions excluded by default;
# empty = face off. NOTE: this list is ignored under the PAM model (pam_facelock.so
# in /etc/pam.d/*), where face is attempted for every action with password fallback.
# See "Polkit / sudo Face Auth" §7a/§7b above for the two models.
face_eligible_actions = ["org.freedesktop.login1.lock-sessions"]
```

## Summary: Security Implementation Priority

| Priority | Mitigation | Spec |
|----------|-----------|------|
| **P0** | IR camera enforcement (`require_ir`) | 02-camera, 05-daemon |
| **P0** | Frame variance check (anti-photo) | 05-daemon |
| **P0** | Model SHA256 at load time | 03-face-engine |
| **P0** | D-Bus system bus policy | 05-daemon |
| **P0** | D-Bus message size limits (bus-enforced) | 01-core-types |
| **P0** | PAM audit logging | 06-pam-module |
| **P0** | Database file permissions | 10-build-install |
| **P1** | IR texture validation | 02-camera, 05-daemon |
| **P1** | Rate limiting | 05-daemon |
| **P1** | systemd hardening | 10-build-install |
| **P1** | Capability dropping | 05-daemon |
| **P1** | Service-specific PAM policy | 06-pam-module |
| **P2** | Embedding encryption at rest | 04-face-store |
| **P2** | Memory zeroing on drop | 01-core-types |
| **P2** | Constant-time similarity comparison | 01-core-types |
