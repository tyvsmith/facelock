use facelock_core::error::{FacelockError, Result};
use v4l::Device;
use v4l::capability::Flags;
use v4l::framesize::FrameSizeEnum;
use v4l::video::Capture;

/// Information about a V4L2 video device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: String,
    pub name: String,
    pub driver: String,
    pub capabilities: Vec<String>,
    pub formats: Vec<FormatInfo>,
}

/// A supported pixel format with its available sizes. Defined in
/// `facelock-core` so `CameraCaps` can carry it; re-exported here where the
/// enumeration actually happens. [`query_device`] normalizes each `fourcc`
/// (V4L2's trailing-space padding stripped: "Y16", not "Y16 ").
pub use facelock_core::types::FormatInfo;

/// List all V4L2 video capture devices.
/// Returns an empty vec if no devices are found (does not error).
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut devices = Vec::new();

    for i in 0..64 {
        let path = format!("/dev/video{i}");
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match query_device(&path) {
            Ok(info) => devices.push(info),
            Err(e) => {
                tracing::debug!("skipping {path}: {e}");
                continue;
            }
        }
    }

    Ok(devices)
}

/// Validate that a specific device path is a usable video capture device.
pub fn validate_device(path: &str) -> Result<DeviceInfo> {
    query_device(path)
}

/// Provenance of an IR classification decision, for logging and honesty.
///
/// Ordered by authoritativeness: a `Quirk` hit is definitive (a USB-ID match
/// always; a name-only match only when corroborated — see
/// [`ir_source_with_quirks`]); `Format` means the device's OWN queried
/// capture formats support IR; `None` means not classified as IR.
///
/// The device's free-text name is NEVER, by itself, sufficient to classify a
/// device as IR (#98) — IR-ness is derived from queried evidence. The name is
/// still consulted, but only as a tiebreak hint during auto-detection
/// selection (see [`pick_auto_device`]) and as part of quirk-match
/// corroboration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrSource {
    /// Hardware quirks DB `force_ir = true`, corroborated — authoritative.
    Quirk,
    /// The device's own queried capture formats support IR (mono-only
    /// format enumeration — see [`has_mono_only_formats`]).
    Format,
    /// Not classified as IR.
    None,
}

/// True if the device name contains a whole `ir` or `infrared` token.
///
/// Tokenizes on non-alphanumeric boundaries so that substrings like the "ir" in
/// "Sirius" or "AIR-Cam" do NOT falsely match — only a standalone token counts.
fn has_ir_name_token(name: &str) -> bool {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| tok.eq_ignore_ascii_case("ir") || tok.eq_ignore_ascii_case("infrared"))
}

/// Heuristic: is this likely an IR camera?
///
/// See [`ir_source_with_quirks`] for the decision rules; this is the boolean form.
pub fn is_ir_camera(device: &DeviceInfo) -> bool {
    ir_source(device) != IrSource::None
}

/// Like [`is_ir_camera`] but accepts a quirks database for device-specific overrides.
pub fn is_ir_camera_with_quirks(
    device: &DeviceInfo,
    quirks: Option<&crate::quirks::QuirksDb>,
) -> bool {
    ir_source_with_quirks(device, quirks) != IrSource::None
}

/// Classify a device's IR provenance without a quirks database.
pub fn ir_source(device: &DeviceInfo) -> IrSource {
    ir_source_with_quirks(device, None)
}

/// V4L2 pixel formats intrinsic to IR/mono sensors. Fourcc strings are
/// space-padded to 4 chars by the V4L2 API; compare trimmed.
const IR_TYPICAL_FOURCCS: [&str; 5] = ["GREY", "Y16", "Y12", "Y10", "Y8"];

pub(crate) fn is_ir_typical_fourcc(fourcc: &str) -> bool {
    IR_TYPICAL_FOURCCS.contains(&fourcc.trim())
}

/// True if the device advertises at least one pixel format facelock can
/// decode (see [`crate::capture::DECODABLE_FORMATS`]). Raw sensor nodes
/// (e.g. Bayer-only Intel IPU6/IPU7 capture nodes) fail this check.
///
/// NOTE: this is a strictly different question from IR-ness. The two lists
/// overlap on GREY and Y16 but neither contains the other: Y8/Y10/Y12 are
/// IR-typical yet undecodable, and YUYV/NV12/MJPG are decodable yet not IR
/// evidence. A device classified IR purely on Y8/Y10/Y12 evidence is
/// therefore excluded from automatic selection. See [`pick_auto_device`],
/// which says so in syslog; setup uses this same predicate and reports its own
/// exclusion at camera-selection time.
pub fn has_decodable_format(device: &DeviceInfo) -> bool {
    device
        .formats
        .iter()
        .any(|f| crate::capture::DECODABLE_FORMATS.contains(&f.fourcc.trim()))
}

/// True if the device enumerates at least one IR-typical mono format
/// (GREY/Y8/Y10/Y12/Y16), possibly alongside other (e.g. color) formats.
///
/// Used for multi-node USB device disambiguation, where a sibling node's
/// mere *offering* of a mono format (not necessarily its ONLY format) is the
/// relevant signal. See [`has_mono_only_formats`] for the stricter "this
/// node IS an IR sensor" evidence used in classification.
fn has_native_ir_format(device: &DeviceInfo) -> bool {
    device
        .formats
        .iter()
        .any(|f| is_ir_typical_fourcc(&f.fourcc))
}

/// Comma-separated format listing for operator-facing messages.
fn format_listing(device: &DeviceInfo) -> String {
    device
        .formats
        .iter()
        .map(|f| f.fourcc.trim())
        .collect::<Vec<_>>()
        .join(", ")
}

/// True if this device enumerates ONLY IR-typical mono capture formats
/// (GREY/Y8/Y10/Y12/Y16), with at least one such format present.
///
/// A node that ALSO enumerates a color format (YUYV, MJPG, ...) alongside a
/// mono one is an ordinary color sensor that happens to offer a mono mode
/// too — not IR evidence (many ordinary RGB UVC webcams enumerate GREY
/// alongside YUYV/MJPG). A node whose ENTIRE format set is mono/IR-typical is
/// a genuine dedicated mono/IR sensor node.
fn has_mono_only_formats(device: &DeviceInfo) -> bool {
    !device.formats.is_empty()
        && device
            .formats
            .iter()
            .all(|f| is_ir_typical_fourcc(&f.fourcc))
}

/// An IR-ness verdict derived purely from queryable device evidence: the
/// capture formats the device actually enumerates, NEVER the free-text
/// device/card name, which is trivially attacker-controlled on virtual
/// devices such as v4l2loopback (#98).
///
/// Format evidence is stronger than the name, but it is NOT unforgeable:
/// deriving IR-ness from the enumerated formats raises the attacker's cost from
/// "set a `CARD_LABEL` string" to "also negotiate a mono-only pixel format", yet
/// a root-loaded `v4l2loopback` device or a programmable USB gadget can still
/// present a mono-only (GREY/Y16/…) format set and thus classify as IR. The
/// backstops against a fabricated IR device are the liveness / frame-variance
/// checks and the privilege required to create such a device — not this signal
/// alone. See `docs/security.md` §A ("Honest residual").
fn has_queried_ir_evidence(device: &DeviceInfo) -> bool {
    has_mono_only_formats(device)
}

/// The quirk-free heuristic classification, derived SOLELY from queried
/// device evidence (#98 — never the free-text name). The device name is not
/// consulted here at all; it is used only as a tiebreak hint during
/// auto-detection selection (see [`pick_auto_device`]).
fn heuristic_ir_source(device: &DeviceInfo) -> IrSource {
    if has_queried_ir_evidence(device) {
        IrSource::Format
    } else {
        IrSource::None
    }
}

/// Classify a device's IR provenance, honoring the quirks DB as authoritative.
///
/// Decision rules:
/// 1. A quirks DB `force_ir = false` is authoritative "not IR", regardless of
///    how the quirk matched.
/// 2. A quirks DB `force_ir = true` matched by USB vendor:product ID is
///    authoritative "IR" — a virtual device (e.g. v4l2loopback) has no real
///    USB node, so it can never win this path.
/// 3. A quirks DB `force_ir = true` matched by device NAME ONLY requires
///    corroboration before it is trusted: either the device has a real (if
///    DB-unlisted) USB identity, or its own queried formats independently
///    support IR (rule 4). Without corroboration a crafted device name alone
///    cannot win `force_ir` through the quirks path either (#98 Task 3).
/// 4. Otherwise, IR-ness is derived SOLELY from the device's own queried
///    capture formats (mono-only enumeration — GREY/Y8/Y10/Y12/Y16). The
///    free-text device name is NEVER, by itself, sufficient (#98): a crafted
///    `CARD_LABEL` on a color-only device does not classify as IR no matter
///    what it is called.
///
/// CAVEAT (multi-node USB devices): one physical USB camera can expose several
/// V4L2 capture nodes sharing the same VID:PID (e.g. the Logitech BRIO's RGB
/// node and IR node). Per-node this function classifies ALL of them by the
/// quirk. Use [`classify_ir_sources`] (list) or [`ir_source_resolved`] (single
/// device, enumerates siblings) to disambiguate the actual IR sensor node.
pub fn ir_source_with_quirks(
    device: &DeviceInfo,
    quirks: Option<&crate::quirks::QuirksDb>,
) -> IrSource {
    ir_source_with_quirks_and_ids(
        device,
        quirks,
        crate::quirks::read_usb_ids(&device.path).as_ref(),
    )
}

/// Per-node classification with the USB IDs supplied by the caller (keeps the
/// sysfs read at the call boundary for testability).
fn ir_source_with_quirks_and_ids(
    device: &DeviceInfo,
    quirks: Option<&crate::quirks::QuirksDb>,
    usb_ids: Option<&(String, String)>,
) -> IrSource {
    if let Some(db) = quirks
        && let Some((quirk, kind)) = db.find_match_with_kind(device, usb_ids)
    {
        match quirk.force_ir {
            Some(false) => return IrSource::None,
            Some(true) => {
                let corroborated = match kind {
                    // A real hardware identity match — authoritative on
                    // its own.
                    crate::quirks::QuirkMatchKind::UsbId => true,
                    // A name-only match needs corroboration: either a
                    // real (if DB-unlisted) USB identity, or the
                    // device's own queried evidence. A virtual
                    // v4l2loopback node has neither, so a crafted name
                    // alone can no longer win force_ir through the
                    // quirks path (#98 Task 3).
                    crate::quirks::QuirkMatchKind::NameOnly => {
                        usb_ids.is_some() || has_queried_ir_evidence(device)
                    }
                };
                if corroborated {
                    return IrSource::Quirk;
                }
                // Uncorroborated name-only force_ir: fall through to the
                // evidence-only heuristic below.
            }
            None => {}
        }
    }
    heuristic_ir_source(device)
}

/// Classify IR provenance for a whole set of enumerated capture nodes,
/// disambiguating multi-node USB devices.
///
/// A quirks `force_ir` entry means "this USB **device** has an IR sensor", not
/// "every capture node of it is IR". One physical camera can expose several
/// V4L2 nodes sharing the same VID:PID — e.g. the Logitech BRIO (046d:085e) has
/// an RGB node (YUYV/MJPG) *and* an IR node (native GREY). When multiple nodes
/// share one quirk-matched USB identity AND at least one of them exposes an
/// IR-typical format (GREY/Y8/Y10/Y12/Y16), only the node(s) with that format
/// are IR; siblings without it fall back to the quirk-free heuristic. A
/// quirk's `format_preference` counts as this evidence only when the preference
/// is itself IR-typical and the node advertises it. If NO node has an IR-like
/// format there is no evidence to disambiguate with, so `force_ir` is trusted
/// for all nodes (some quirk entries exist precisely because the camera
/// advertises no IR-like format).
pub fn classify_ir_sources(
    devices: &[DeviceInfo],
    quirks: Option<&crate::quirks::QuirksDb>,
) -> Vec<IrSource> {
    let usb_ids: Vec<Option<(String, String)>> = devices
        .iter()
        .map(|d| crate::quirks::read_usb_ids(&d.path))
        .collect();
    classify_ir_sources_with_ids(devices, quirks, &usb_ids)
}

fn classify_ir_sources_with_ids(
    devices: &[DeviceInfo],
    quirks: Option<&crate::quirks::QuirksDb>,
    usb_ids: &[Option<(String, String)>],
) -> Vec<IrSource> {
    let mut sources: Vec<IrSource> = devices
        .iter()
        .zip(usb_ids)
        .map(|(d, ids)| ir_source_with_quirks_and_ids(d, quirks, ids.as_ref()))
        .collect();

    // Node-level disambiguation for multi-node USB devices.
    let mut seen: Vec<&(String, String)> = Vec::new();
    for i in 0..devices.len() {
        if sources[i] != IrSource::Quirk {
            continue;
        }
        // Sibling grouping requires a readable USB identity.
        let Some(ids) = usb_ids[i].as_ref() else {
            continue;
        };
        if seen.contains(&ids) {
            continue;
        }
        seen.push(ids);

        let group: Vec<usize> = (0..devices.len())
            .filter(|&j| sources[j] == IrSource::Quirk && usb_ids[j].as_ref() == Some(ids))
            .collect();
        if group.len() < 2 {
            continue;
        }

        // IR-like formats: native GREY/Y8/Y10/Y12/Y16, including the quirk's
        // format_preference only when the preference is itself IR-typical.
        let pref = quirks
            .and_then(|db| db.find_match_with_ids(&devices[i], Some(ids)))
            .and_then(|q| q.format_preference.clone());
        // Both sides trimmed. Ingest normalizes FourCCs now, but this
        // comparison decides whether a node is demoted out of `force_ir`, and
        // a false here in *every* node of the group means no node is demoted
        // and `force_ir` is trusted for all of them — the RGB sibling
        // included. That is the wrong direction to fail in, so it does not
        // get to depend on normalization having happened upstream.
        let node_has_ir_format = |j: usize| {
            has_native_ir_format(&devices[j])
                || pref.as_deref().is_some_and(|p| {
                    is_ir_typical_fourcc(p)
                        && devices[j]
                            .formats
                            .iter()
                            .any(|f| f.fourcc.trim() == p.trim())
                })
        };

        // Only demote when format evidence exists within the group; otherwise
        // trust force_ir for every node.
        if group.iter().any(|&j| node_has_ir_format(j)) {
            for &j in &group {
                if !node_has_ir_format(j) {
                    let demoted = heuristic_ir_source(&devices[j]);
                    tracing::debug!(
                        device = %devices[j].path,
                        vid = %ids.0,
                        pid = %ids.1,
                        reclassified = ?demoted,
                        "multi-node quirk device: node lacks IR-like format, \
                         sibling node has it — not the IR sensor node"
                    );
                    sources[j] = demoted;
                }
            }
        }
    }

    sources
}

/// Sibling-aware IR classification for a single device.
///
/// Enumerates the host's other V4L2 nodes so that multi-node USB devices are
/// disambiguated exactly as in [`classify_ir_sources`]. Use this instead of
/// [`ir_source_with_quirks`] whenever the answer gates `require_ir`.
pub fn ir_source_resolved(
    device: &DeviceInfo,
    quirks: Option<&crate::quirks::QuirksDb>,
) -> IrSource {
    // Siblings only add context; the caller's DeviceInfo is authoritative for
    // its own path (replace any enumerated entry at the same path with it).
    let mut devices = list_devices().unwrap_or_default();
    devices.retain(|d| d.path != device.path);
    devices.push(device.clone());
    let sources = classify_ir_sources(&devices, quirks);
    // The device was appended last above.
    sources.last().copied().unwrap_or(IrSource::None)
}

/// Boolean form of [`ir_source_resolved`].
pub fn is_ir_camera_resolved(
    device: &DeviceInfo,
    quirks: Option<&crate::quirks::QuirksDb>,
) -> bool {
    ir_source_resolved(device, quirks) != IrSource::None
}

/// Auto-detect the best available video capture device.
///
/// Classifies all nodes with [`classify_ir_sources`] (so multi-node USB devices
/// resolve to their actual IR sensor node), then prefers: a quirks-confirmed IR
/// node with a native IR format, then any quirks-confirmed IR node, then an
/// evidence-classified IR node (preferring one whose name also carries an IR
/// token, as a tiebreak hint only), then the first enumerated device. It never
/// auto-selects an unknown camera *just because* its NAME claims to be IR
/// (#98) or *just because* it self-reports a GREY/Y16 format alongside color
/// formats (H1) — only queried mono-only format evidence, or a corroborated
/// quirk, counts.
///
/// NOTE (seam for Plan 02): device selection here is by capability/heuristic, not
/// by stable device identity. Plan 02 will pin the enrolled camera by identity.
pub fn auto_detect_device() -> Result<DeviceInfo> {
    auto_detect_device_with(&crate::quirks::QuirksDb::load())
}

/// [`auto_detect_device`] against a caller-supplied quirks DB.
///
/// Callers that have already loaded a DB use this so selection and the
/// classification recorded afterwards are decided by the SAME quirk set —
/// loading a second copy would let the two disagree if the quirks files
/// changed in between, and costs a directory walk either way.
pub fn auto_detect_device_with(quirks: &crate::quirks::QuirksDb) -> Result<DeviceInfo> {
    let devices = list_devices()?;
    if devices.is_empty() {
        return Err(FacelockError::Camera("no video devices found".into()));
    }
    let sources = classify_ir_sources(&devices, Some(quirks));
    pick_auto_device(&devices, &sources)
        .cloned()
        .ok_or_else(|| {
            let listing = devices
                .iter()
                .map(|d| format!("{} \"{}\" [{}]", d.path, d.name, format_listing(d)))
                .collect::<Vec<_>>()
                .join("; ");
            FacelockError::Camera(format!(
                "no camera with a decodable pixel format ({}) found; detected: {listing}. \
             Raw sensor nodes (e.g. Intel IPU6/IPU7) are excluded from auto-detection — \
             set device.path to a processed camera (see docs/compatibility.md)",
                crate::capture::DECODABLE_FORMATS.join("/"),
            ))
        })
}

/// Selection order for auto-detection, over pre-classified nodes.
///
/// Prefers the format-corroborated IR node so a multi-node camera's RGB
/// sibling is never picked over its IR sensor. Among evidence-classified
/// nodes with no quirk, the device name is consulted only as a tiebreak hint
/// (an IR name token breaks ties among already-qualified nodes; it never
/// promotes a node with no format evidence — see [`heuristic_ir_source`]).
///
/// Devices without a decodable pixel format (raw Bayer sensor nodes etc.) are
/// excluded from every tier — selecting one would guarantee capture failure.
/// The exclusion is applied AFTER classification, never before it: it changes
/// which node is selected, never whether a node counts as IR. A device that
/// would have been selected but is excluded here still cannot authenticate
/// (the `require_ir` gate reads the caps of whatever node is finally picked),
/// so the exclusion can only ever fail closed.
fn pick_auto_device<'a>(devices: &'a [DeviceInfo], sources: &[IrSource]) -> Option<&'a DeviceInfo> {
    // An excluded IR node is why auth later reports "not an IR camera" — say so
    // in syslog rather than leaving the operator with only the downstream error.
    for (device, source) in devices.iter().zip(sources) {
        if *source != IrSource::None && !has_decodable_format(device) {
            tracing::warn!(
                device = %device.path,
                name = %device.name,
                formats = %format_listing(device),
                source = ?source,
                "IR-classified camera has no decodable pixel format — excluded from auto-detection"
            );
        }
    }

    let nodes = || {
        devices
            .iter()
            .zip(sources)
            .filter(|(d, _)| has_decodable_format(d))
    };
    nodes()
        .find(|(d, s)| **s == IrSource::Quirk && has_native_ir_format(d))
        .or_else(|| nodes().find(|(_, s)| **s == IrSource::Quirk))
        .or_else(|| {
            nodes()
                .find(|(d, s)| **s != IrSource::None && has_ir_name_token(&d.name))
                .or_else(|| nodes().find(|(_, s)| **s != IrSource::None))
        })
        .map(|(d, _)| d)
        .or_else(|| devices.iter().find(|d| has_decodable_format(d)))
}

fn query_device(path: &str) -> Result<DeviceInfo> {
    let dev = Device::with_path(path).map_err(|e| FacelockError::Camera(format!("{path}: {e}")))?;

    let caps = dev
        .query_caps()
        .map_err(|e| FacelockError::Camera(format!("{path}: failed to query caps: {e}")))?;

    if !caps.capabilities.contains(Flags::VIDEO_CAPTURE) {
        return Err(FacelockError::Camera(format!(
            "{path}: not a video capture device"
        )));
    }

    let mut cap_strings = Vec::new();
    if caps.capabilities.contains(Flags::VIDEO_CAPTURE) {
        cap_strings.push("VIDEO_CAPTURE".to_string());
    }
    if caps.capabilities.contains(Flags::STREAMING) {
        cap_strings.push("STREAMING".to_string());
    }

    let mut formats = Vec::new();
    if let Ok(fmt_list) = dev.enum_formats() {
        for fmt in fmt_list {
            let fourcc = crate::capture::normalize_fourcc(fmt.fourcc);
            let description = fmt.description.clone();
            let mut sizes = Vec::new();
            if let Ok(size_list) = dev.enum_framesizes(fmt.fourcc) {
                for fs in size_list {
                    match fs.size {
                        FrameSizeEnum::Discrete(d) => {
                            sizes.push((d.width, d.height));
                        }
                        FrameSizeEnum::Stepwise(s) => {
                            sizes.push((s.min_width, s.min_height));
                            sizes.push((s.max_width, s.max_height));
                        }
                    }
                }
            }
            formats.push(FormatInfo {
                fourcc,
                description,
                sizes,
            });
        }
    }

    Ok(DeviceInfo {
        path: path.to_string(),
        name: caps.card.clone(),
        driver: caps.driver.clone(),
        capabilities: cap_strings,
        formats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_with(name: &str, fourccs: &[&str]) -> DeviceInfo {
        DeviceInfo {
            path: "/dev/nonexistent_test_video".into(),
            name: name.into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: fourccs
                .iter()
                .map(|f| FormatInfo {
                    fourcc: (*f).into(),
                    description: "test".into(),
                    sizes: vec![(640, 480)],
                })
                .collect(),
        }
    }

    #[test]
    fn is_ir_camera_mono_format_alone_is_ir_by_evidence() {
        // #98 fix: IR-ness is DERIVED from queried device evidence, never
        // from the free-text name. A device with an unrelated name that
        // enumerates ONLY a mono/IR-typical format is genuine IR evidence
        // ("a genuine IR-evidence device still classifies as IR" — no name
        // corroboration needed).
        let device = device_with("USB Camera", &["GREY"]);
        assert!(is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::Format);
    }

    #[test]
    fn crafted_card_label_with_color_formats_only_is_not_ir() {
        // #98 regression: a v4l2loopback device can set CARD_LABEL to
        // anything. A crafted "Fake IR Camera" label backed only by
        // ordinary color formats must not defeat require_ir.
        let device = device_with("Fake IR Camera", &["YUYV", "MJPG"]);
        assert!(!is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::None);
    }

    #[test]
    fn ir_classification_corpus() {
        // Real RGB camera name strings must classify not-IR, even the ones
        // whose names contain the substring "ir" but not the token "ir" —
        // and even though these carry no format evidence either.
        for name in [
            "Integrated Webcam",
            "USB2.0 HD UVC WebCam",
            "AIR-Cam",
            "Sirius",
            "Chicony USB2.0 Camera",
        ] {
            let dev = device_with(name, &["YUYV", "MJPG"]);
            assert!(!is_ir_camera(&dev), "{name} should be not-IR");
        }
        // A GREY-and-color mix (the H1 case) is still not-IR: only a
        // mono-ONLY format set counts as evidence.
        assert!(!is_ir_camera(&device_with(
            "Generic Cam",
            &["GREY", "YUYV"]
        )));
        // #98: an IR name token with only color formats is NOT sufficient —
        // IR-ness must be derived from queried evidence, never the name.
        assert_eq!(
            ir_source(&device_with("Integrated IR Camera", &["YUYV"])),
            IrSource::None
        );
        assert_eq!(
            ir_source(&device_with("Infrared Camera", &["MJPG"])),
            IrSource::None
        );
    }

    #[test]
    fn is_ir_camera_mjpg_only() {
        let device = device_with("USB Camera", &["MJPG"]);
        assert!(!is_ir_camera(&device));
    }

    #[test]
    fn is_ir_camera_name_token_alone_is_not_ir() {
        // #98 fix: a bare name token ("Infrared Camera") with only a color
        // format (MJPG) must NOT classify as IR — name alone is never
        // sufficient.
        let device = device_with("Infrared Camera", &["MJPG"]);
        assert!(!is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::None);
    }

    #[test]
    fn is_ir_camera_y16_alone_is_format_regardless_of_name() {
        // Y16 native mono format alone → Format provenance, independent of
        // the device name (#98: evidence-derived, not name-derived).
        //
        // Deliberately the PADDED spelling: `query_device` now normalizes
        // FourCCs at ingest, but IR classification must not start depending on
        // that — a `DeviceInfo` reaching here from anywhere else (a test, a
        // future caller, `CameraCaps`) still classifies the same. The trim in
        // `is_ir_typical_fourcc` is what holds that, and this pins it.
        let device = device_with("Integrated IR Camera", &["Y16 "]);
        assert!(is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::Format);
        // ...and the normalized spelling the real ingest path produces.
        let device = device_with("Integrated IR Camera", &["Y16"]);
        assert!(is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::Format);
    }

    #[test]
    fn is_ir_camera_y16_alone_is_ir_even_with_unrelated_name() {
        // #98 fix: mono format evidence alone is sufficient, even when the
        // device's name gives no IR hint at all — proving classification
        // does not depend on the name in either direction.
        let device = device_with("Depth Camera", &["Y16 "]);
        assert!(is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::Format);
        let device = device_with("Depth Camera", &["Y16"]);
        assert!(is_ir_camera(&device));
        assert_eq!(ir_source(&device), IrSource::Format);
    }

    #[test]
    fn quirk_force_ir_usb_id_match_is_authoritative() {
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(crate::quirks::Quirk {
            vendor_id: Some("dead".into()),
            product_id: Some("beef".into()),
            name_pattern: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: Some("test force_ir via USB ID".into()),
        });
        // No IR name token, no IR format — a USB-ID quirk match alone makes
        // it IR, no corroboration needed.
        let device = device_with("Generic Camera", &["YUYV"]);
        let ids = Some(("dead".into(), "beef".into()));
        assert_eq!(
            ir_source_with_quirks_and_ids(&device, Some(&db), ids.as_ref()),
            IrSource::Quirk
        );
    }

    #[test]
    fn quirk_force_ir_name_only_requires_corroboration() {
        // #98 Task 3: a name-only quirk match must not grant force_ir on its
        // own — it needs corroboration from a real USB identity or the
        // device's own format evidence.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(crate::quirks::Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)generic.*".into()),
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: Some("test force_ir".into()),
        });

        // No usb_ids (as on a virtual v4l2loopback node) and no format
        // evidence of its own — must NOT be trusted.
        let uncorroborated = device_with("Generic Camera", &["YUYV"]);
        assert_eq!(
            ir_source_with_quirks_and_ids(&uncorroborated, Some(&db), None),
            IrSource::None,
            "uncorroborated name-only force_ir must not grant IR"
        );

        // Corroborated by a real (if DB-unlisted) USB identity.
        let real_ids = Some(("1234".into(), "5678".into()));
        assert_eq!(
            ir_source_with_quirks_and_ids(&uncorroborated, Some(&db), real_ids.as_ref()),
            IrSource::Quirk,
            "a name-only match backed by a real USB identity is corroborated"
        );

        // Corroborated by the device's own mono-format evidence.
        let mono_evidence = device_with("Generic Camera", &["GREY"]);
        assert_eq!(
            ir_source_with_quirks_and_ids(&mono_evidence, Some(&db), None),
            IrSource::Quirk,
            "a name-only match backed by the device's own mono format evidence is corroborated"
        );
    }

    #[test]
    fn quirk_force_ir_false_is_authoritative_regardless_of_corroboration() {
        // A quirk with force_ir = false is authoritative "not IR" even if
        // the name has an IR token and the format looks IR-typical.
        let mut db_off = crate::quirks::QuirksDb::default();
        db_off.push_quirk_for_test(crate::quirks::Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)ir camera".into()),
            force_ir: Some(false),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        });
        let ir_named = device_with("IR Camera", &["GREY"]);
        assert_eq!(
            ir_source_with_quirks_and_ids(&ir_named, Some(&db_off), None),
            IrSource::None
        );
    }

    fn device_at(path: &str, name: &str, fourccs: &[&str]) -> DeviceInfo {
        DeviceInfo {
            path: path.into(),
            ..device_with(name, fourccs)
        }
    }

    fn brio_quirk(format_preference: Option<&str>) -> crate::quirks::Quirk {
        crate::quirks::Quirk {
            vendor_id: Some("046d".into()),
            product_id: Some("085e".into()),
            name_pattern: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: Some(1),
            format_preference: format_preference.map(Into::into),
            y16_bit_depth: None,
            rotation: None,
            notes: Some("Logitech BRIO 4K with IR sensor".into()),
        }
    }

    fn brio_ids() -> Option<(String, String)> {
        Some(("046d".into(), "085e".into()))
    }

    #[test]
    fn brio_multi_node_only_grey_node_classifies_ir() {
        // Regression (hardware-verified, Logitech BRIO 046d:085e): one physical
        // USB camera exposes TWO capture nodes sharing the same VID:PID —
        // /dev/video0 (RGB sensor, YUYV/MJPG) and /dev/video2 (IR sensor, native
        // GREY). A force_ir quirk means "this USB device has an IR sensor", NOT
        // "every capture node of it is IR": only the GREY-native node is IR.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(Some("GREY")));

        let rgb = device_at("/dev/video0", "Logitech BRIO", &["YUYV", "MJPG"]);
        let ir = device_at("/dev/video2", "Logitech BRIO", &["GREY"]);
        let devices = [rgb, ir];
        let ids = vec![brio_ids(), brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(
            sources[0],
            IrSource::None,
            "RGB sibling node must NOT classify IR"
        );
        assert_eq!(
            sources[1],
            IrSource::Quirk,
            "GREY-native node keeps quirk-IR classification"
        );

        // Auto-detect-equivalent selection must pick the IR (GREY) node, not
        // the first enumerated node (the RGB sensor with the white LED).
        let picked = pick_auto_device(&devices, &sources).expect("a device is picked");
        assert_eq!(picked.path, "/dev/video2");
    }

    #[test]
    fn brio_multi_node_disambiguates_without_format_preference() {
        // Even without format_preference on the quirk, the native GREY format
        // alone disambiguates the sibling nodes.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(None));

        let devices = [
            device_at("/dev/video0", "Logitech BRIO", &["YUYV", "MJPG"]),
            device_at("/dev/video2", "Logitech BRIO", &["GREY"]),
        ];
        let ids = vec![brio_ids(), brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(sources[0], IrSource::None);
        assert_eq!(sources[1], IrSource::Quirk);
    }

    #[test]
    fn rgb_format_preference_does_not_exempt_rgb_sibling_from_demotion() {
        // Regression #164: format_preference is node-level IR evidence only
        // when the preferred format is itself IR-typical. An RGB preference
        // such as MJPG must not let the RGB sibling keep force_ir when a GREY
        // sibling provides real evidence for disambiguation.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(Some("MJPG")));

        let devices = [
            device_at("/dev/video0", "Logitech BRIO", &["YUYV", "MJPG"]),
            device_at("/dev/video2", "Logitech BRIO", &["GREY"]),
        ];
        let ids = vec![brio_ids(), brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(
            sources[0],
            IrSource::None,
            "an RGB format preference must not preserve force_ir on the RGB sibling"
        );
        assert_eq!(sources[1], IrSource::Quirk);
    }

    #[test]
    fn quirk_multi_node_without_any_ir_format_trusts_force_ir_for_all() {
        // Edge case: some force_ir quirks exist precisely BECAUSE the camera
        // does not advertise an IR-like format. If no sibling node has one,
        // there is no format evidence to disambiguate — trust force_ir for all.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(None));

        let devices = [
            device_at("/dev/video0", "Some IR Module", &["YUYV"]),
            device_at("/dev/video2", "Some IR Module", &["MJPG"]),
        ];
        let ids = vec![brio_ids(), brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(sources[0], IrSource::Quirk);
        assert_eq!(sources[1], IrSource::Quirk);
        // With no format evidence, selection preserves enumeration order.
        let picked = pick_auto_device(&devices, &sources).expect("a device is picked");
        assert_eq!(picked.path, "/dev/video0");
    }

    #[test]
    fn quirk_single_node_without_ir_format_stays_ir() {
        // A single quirk-matched node with no IR-like format is the whole point
        // of force_ir — it must remain IR.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(None));

        let devices = [device_at("/dev/video0", "Oddball IR Module", &["YUYV"])];
        let ids = vec![brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(sources[0], IrSource::Quirk);
    }

    #[test]
    fn multi_node_demoted_sibling_falls_back_to_no_evidence() {
        // A demoted sibling falls back to the (quirk-free) heuristic, which
        // is evidence-only (#98): an IR name token alone does NOT resurrect
        // an IR classification for a node with no format evidence of its own.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(None));

        let devices = [
            device_at("/dev/video0", "Vendor IR Camera", &["YUYV"]),
            device_at("/dev/video2", "Vendor IR Camera", &["GREY"]),
        ];
        let ids = vec![brio_ids(), brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(sources[0], IrSource::None);
        assert_eq!(sources[1], IrSource::Quirk);
        // Selection still prefers the format-corroborated quirk node.
        let picked = pick_auto_device(&devices, &sources).expect("a device is picked");
        assert_eq!(picked.path, "/dev/video2");
    }

    #[test]
    fn classify_without_usb_ids_uncorroborated_name_quirk_falls_back() {
        // #98 Task 3: nodes whose USB identity is unreadable (as on a
        // virtual v4l2loopback device) cannot corroborate a name-only quirk
        // match. A node with no format evidence of its own is NOT granted
        // force_ir just because its (attacker-controlled) name matches the
        // pattern. A sibling node that DOES carry its own mono format
        // evidence still classifies IR — via that evidence, not blind trust
        // in the quirk.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(crate::quirks::Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)generic.*".into()),
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        });

        let devices = [
            device_at("/dev/video0", "Generic Camera", &["YUYV"]),
            device_at("/dev/video2", "Generic Camera", &["GREY"]),
        ];
        let ids = vec![None, None];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        assert_eq!(
            sources[0],
            IrSource::None,
            "uncorroborated name-only force_ir must not grant IR"
        );
        assert_eq!(
            sources[1],
            IrSource::Quirk,
            "corroborated by its own mono format evidence"
        );
    }

    #[test]
    fn classify_mixed_identities_only_groups_same_usb_device() {
        // Two DIFFERENT USB cameras (different VID:PID) both quirk-matched:
        // no cross-device demotion may happen.
        let mut db = crate::quirks::QuirksDb::default();
        db.push_quirk_for_test(brio_quirk(None));
        db.push_quirk_for_test(crate::quirks::Quirk {
            vendor_id: Some("8086".into()),
            product_id: Some("0b07".into()),
            name_pattern: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        });
        let devices = [
            device_at("/dev/video0", "RealSense", &["YUYV"]),
            device_at("/dev/video2", "Logitech BRIO", &["GREY"]),
        ];
        let ids = vec![Some(("8086".into(), "0b07".into())), brio_ids()];

        let sources = classify_ir_sources_with_ids(&devices, Some(&db), &ids);
        // Different physical devices — both keep their quirk classification.
        assert_eq!(sources[0], IrSource::Quirk);
        assert_eq!(sources[1], IrSource::Quirk);
    }

    #[test]
    fn pick_auto_device_skips_undecodable_raw_node() {
        // Issue #89: Intel IPU7 exposes raw Bayer nodes first (/dev/video0)
        // and a processed loopback camera later. Auto-detection must skip the
        // raw node even though it enumerates first.
        let devices = [
            device_at("/dev/video0", "ipu7", &["SGRBG10"]),
            device_at("/dev/video50", "Hardware ISP Camera", &["NV12", "YUYV"]),
        ];
        let sources = [IrSource::None, IrSource::None];
        let picked = pick_auto_device(&devices, &sources).expect("a device is picked");
        assert_eq!(picked.path, "/dev/video50");
    }

    #[test]
    fn pick_auto_device_skips_undecodable_ir_classified_node() {
        // Even an IR-classified node is unusable without a decodable format;
        // fall through to a decodable device rather than guarantee capture
        // failure.
        //
        // Y10 is the reachable shape of this after #98: the IR-typical list
        // (GREY/Y8/Y10/Y12/Y16) and the decodable list (GREY/Y16/YUYV/NV12/
        // MJPG) are different sets, so a Y10-only sensor node classifies as
        // `IrSource::Format` on its own evidence and is *still* excluded here.
        // Note what this costs: on a machine whose only IR sensor is Y10-only,
        // auto-detection now lands on the RGB webcam instead. That fails the
        // `require_ir` gate (the gate reads the caps of the node finally
        // picked), so it fails closed — but it fails with "not an IR camera"
        // rather than "cannot decode Y10", which is why the exclusion is also
        // logged with the device path and its formats.
        let devices = [
            device_at("/dev/video0", "Vendor IR Camera", &["Y10"]),
            device_at("/dev/video2", "USB Camera", &["MJPG"]),
        ];
        let sources = [IrSource::Format, IrSource::None];
        let picked = pick_auto_device(&devices, &sources).expect("a device is picked");
        assert_eq!(picked.path, "/dev/video2");
    }

    #[test]
    fn ir_typical_and_decodable_format_sets_deliberately_disagree() {
        // The rebase of #90 onto the evidence-based IR classification (#98)
        // put two format lists side by side, and they are NOT the same set.
        // Pinning the disagreement here so a later edit to either list is a
        // deliberate act rather than an accident:
        //
        //   - Y8/Y10/Y12 are IR evidence but facelock cannot decode them, so a
        //     node whose only IR evidence is one of them classifies as IR and
        //     is then excluded from auto-detection.
        //   - YUYV/NV12/MJPG are decodable but are not IR evidence.
        //
        // If Y8/Y10/Y12 decode support is ever added, the first assertion is
        // what will fail and point at this comment.
        let ir_only: Vec<&str> = IR_TYPICAL_FOURCCS
            .iter()
            .copied()
            .filter(|f| !crate::capture::DECODABLE_FORMATS.contains(f))
            .collect();
        assert_eq!(
            ir_only,
            vec!["Y12", "Y10", "Y8"],
            "IR-typical but undecodable — these classify as IR and are then \
             excluded from auto-detection"
        );

        let decodable_only: Vec<&str> = crate::capture::DECODABLE_FORMATS
            .iter()
            .copied()
            .filter(|f| !IR_TYPICAL_FOURCCS.contains(f))
            .collect();
        assert_eq!(decodable_only, vec!["YUYV", "NV12", "MJPG"]);

        // And the consequence, stated as behavior: adding Y16 decode support
        // did NOT make Y16 evidence of anything new — a Y16-only node was IR
        // before #90 and is IR after it. #90 only made it openable.
        let y16_only = device_at("/dev/video0", "Depth Camera", &["Y16"]);
        assert!(has_decodable_format(&y16_only));
        assert_eq!(heuristic_ir_source(&y16_only), IrSource::Format);
    }

    #[test]
    fn decodable_format_predicate_keeps_grey_and_y16_but_rejects_y8_y10_y12() {
        for format in ["GREY", "Y16", "Y16 "] {
            assert!(
                has_decodable_format(&device_at("/dev/video0", "IR", &[format])),
                "{format} must remain decodable"
            );
        }
        for format in ["Y8", "Y10", "Y12"] {
            assert!(
                !has_decodable_format(&device_at("/dev/video0", "IR", &[format])),
                "{format} is IR evidence but is not decodable"
            );
        }
    }

    #[test]
    fn pick_auto_device_none_when_nothing_decodable() {
        let devices = [
            device_at("/dev/video0", "ipu7", &["SGRBG10"]),
            device_at("/dev/video1", "ipu7", &["SBGGR12"]),
        ];
        let sources = [IrSource::None, IrSource::None];
        assert!(pick_auto_device(&devices, &sources).is_none());
    }

    #[test]
    fn pick_auto_device_y16_only_device_is_decodable() {
        // Y16 decode support means a Y16-only IR camera stays usable.
        let devices = [device_at("/dev/video0", "Integrated IR Camera", &["Y16"])];
        let sources = [IrSource::Format];
        let picked = pick_auto_device(&devices, &sources).expect("a device is picked");
        assert_eq!(picked.path, "/dev/video0");
    }

    #[test]
    fn list_devices_does_not_crash() {
        // Should return Ok even if no devices exist
        let result = list_devices();
        assert!(result.is_ok());
    }
}
