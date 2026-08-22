use facelock_core::config::DeviceConfig;
use facelock_core::error::{FacelockError, Result};
use facelock_core::traits::CameraSource;
use facelock_core::types::Frame;
use image::ImageReader;
use std::io::Cursor;
use std::time::{Duration, Instant};
use v4l::Device;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;

/// Timeout for V4L2 DQBUF poll. If the camera doesn't produce a frame
/// within this time, capture returns an error instead of blocking forever.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of MMAP buffers the capture stream is created with.
///
/// Public because it is not a private tuning knob: a consumer that keeps a
/// stream open between requests (the daemon's warm camera hold, ADR 008 §4)
/// must discard `MMAP_BUFFERS - 1` frames before analyzing anything, since
/// V4L2 leaves exactly that many buffers filled with the frames captured
/// right after the previous request. Reading the count from here is what
/// keeps that discard correct if the buffer depth ever changes.
pub const MMAP_BUFFERS: u32 = 4;

/// Minimum frames sampled at open to calibrate the Y16 -> 8-bit scale when no
/// quirk supplies a sensor bit depth. Calibrating from a single frame races the
/// camera's own AGC/AE warmup: a dark first frame pins an 8-bit scale that
/// clips every warmed-up 10/12-bit frame to flat white, which the IR texture
/// check then rejects. A short burst gives exposure time to move.
///
/// This burst is paid on a COLD open only, and only on a Y16 device: the
/// daemon's warm hold (ADR 008 §4) reuses the `Camera` and therefore its
/// already-pinned scale. It is nonetheless emitter-LED-on time on IR
/// hardware, which is exactly what ADR 008 set out to shorten — a device that
/// declares `y16_bit_depth` in its quirk skips the burst entirely and is the
/// preferred configuration on any camera where the reopen cost matters.
const Y16_CALIBRATION_FRAMES: usize = 8;

/// Wall-clock ceiling on that burst. Frames are already flowing when it runs,
/// so it normally costs a fraction of a second; the budget keeps a stalling
/// camera from stretching `Camera::open` by `CAPTURE_TIMEOUT` eight times over.
///
/// This bounds when the burst STOPS starting new captures, not when it
/// returns: the check sits at the top of the loop, so a dequeue begun just
/// under the budget can still block for `CAPTURE_TIMEOUT`. Worst case is
/// therefore this budget plus one `CAPTURE_TIMEOUT`, not this budget alone.
const Y16_CALIBRATION_BUDGET: Duration = Duration::from_secs(1);

/// Pixel formats facelock can decode to RGB, in negotiation priority order:
/// IR-native grayscale first, then cheap lossless raw conversions, then MJPG
/// (JPEG decode). Also used by device auto-detection to skip devices (e.g. raw
/// Bayer sensor nodes) that advertise none of these. The order is part of the
/// documented negotiation contract (`docs/contracts.md`).
pub(crate) const DECODABLE_FORMATS: &[&str] = &["GREY", "Y16", "YUYV", "NV12", "MJPG"];

/// V4L2 FourCCs are four characters padded with trailing spaces ("Y16 ").
/// Normalize at every point a FourCC enters facelock so no comparison
/// downstream has to trim.
pub(crate) fn normalize_fourcc(fourcc: v4l::format::FourCC) -> String {
    fourcc.to_string().trim().to_string()
}

/// Index into `available` of the first advertised format in `preferred`
/// priority order. Both lists hold normalized FourCCs.
fn select_format(preferred: &[&str], available: &[String]) -> Option<usize> {
    preferred
        .iter()
        .find_map(|pref| available.iter().position(|f| f == pref))
}

/// A quirk's `format_preference`, dropped when facelock cannot decode it —
/// honoring it would negotiate a format every subsequent capture fails on.
///
/// Trimmed rather than compared raw. `QuirksDb::load_file` already normalizes
/// what it parses, but the shipped `config/quirks.d/00-defaults.toml` writes
/// the PADDED V4L2 spelling (`format_preference = "Y16 "`) for all three
/// RealSense entries — so if that normalization ever regressed, an exact
/// comparison here would silently reclassify every one of them as
/// "undecodable" and drop the preference that picks their IR sensor node.
/// Trimming makes this predicate independent of where the `Quirk` came from
/// (parsed file, `push_quirk_for_test`, or constructed in code).
fn quirk_format_preference(quirk: Option<&Quirk>) -> Option<&str> {
    let pref = quirk.and_then(|q| q.format_preference.as_deref())?.trim();
    if !DECODABLE_FORMATS.contains(&pref) {
        tracing::warn!(
            format = pref,
            "quirk format_preference is not a decodable format — ignoring"
        );
        return None;
    }
    Some(pref)
}

/// The Y16 shift implied by a quirk's declared sensor bit depth, if any.
/// A depth the 16-bit container cannot hold is a quirks-file typo rather than
/// hardware truth: warn and let frame calibration decide instead.
fn quirk_y16_shift(quirk: Option<&Quirk>) -> Option<u8> {
    let bit_depth = quirk.and_then(|q| q.y16_bit_depth)?;
    let shift = preprocess::y16_shift_from_bit_depth(bit_depth);
    if shift.is_none() {
        tracing::warn!(
            bit_depth,
            "quirk y16_bit_depth is outside 8..=16 — ignoring, calibrating from frames"
        );
    }
    shift
}

/// How many frames the calibration burst aims for on this device. A quirk's
/// `warmup_frames` is the device's own statement of how long its exposure takes
/// to settle, so a camera that declares more than the default gets a burst at
/// least that long. The wall-clock budget still bounds it.
fn y16_calibration_frames(quirk: Option<&Quirk>) -> usize {
    quirk
        .and_then(|q| q.warmup_frames)
        .unwrap_or(0)
        .max(Y16_CALIBRATION_FRAMES as u32) as usize
}

/// Pin the session Y16 scale by sampling a burst of frames and taking the
/// brightest sample any of them produced.
///
/// More frames raise the peak toward the sensor's full scale, which is the
/// point of the burst. They do NOT bound it from above: `y16_peak` is an
/// unbounded max over every pixel, so one stuck pixel or one specular IR
/// glint sets it alone. On a 10-bit sensor a pixel latched at 4095 pins
/// shift 4 where 2 was right, and every later frame is scaled down 4x —
/// enough to drop a real face under `ir_texture_min_stddev` for the whole
/// session. `Y16Calibration::Saturated` only catches the `u16::MAX`
/// endpoint, so that case warns nothing. A quirk's `y16_bit_depth` is the
/// only way to take the scale out of the scene's hands entirely.
///
/// A capture error ends the open. It is tempting to tolerate a few and keep
/// sampling, but no `next()` error is transient here: `v4l`'s capture stream
/// advances `arena_index` only on a successful dequeue and re-queues that
/// same index on the following call, so one failed dequeue leaves the buffer
/// queued in the kernel and every later call re-queues it and gets EINVAL.
/// Tolerating errors would therefore trade a hard failure carrying the real
/// message for a `Camera` that returns "Invalid argument" from every capture,
/// having reported a successfully pinned scale on the way out.
fn calibrate_y16_shift(stream: &mut Stream<'_>, device_path: &str, target: usize) -> Result<u8> {
    let start = Instant::now();
    let mut peak = 0u16;
    let mut frames = 0usize;

    while frames < target && start.elapsed() < Y16_CALIBRATION_BUDGET {
        match stream.next() {
            Ok((buf, _meta)) => {
                peak = peak.max(preprocess::y16_peak(buf));
                frames += 1;
            }
            Err(e) => {
                return Err(FacelockError::Camera(format!(
                    "{device_path}: Y16 calibration failed after {frames} frame(s): {e}"
                )));
            }
        }
    }

    if frames == 0 {
        return Err(FacelockError::Camera(format!(
            "{device_path}: Y16 calibration failed: no frame arrived within the \
             calibration budget"
        )));
    }

    // A burst cut short by the wall-clock budget pins the scale from however
    // few frames arrived, and at one frame that is the single-frame AGC race
    // the burst exists to avoid. It was decided on less evidence than
    // intended, and nothing else says so: a mid-range peak off one dark frame
    // takes the `Normal` arm below and logs nothing.
    if frames < target {
        tracing::warn!(
            device = %device_path,
            frames,
            target,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "Y16 calibration burst ended early: pinned the session scale from \
             fewer frames than intended, so the camera's exposure may not have \
             settled. Set the quirk's y16_bit_depth for this device to pin the \
             scale from hardware instead."
        );
    }

    let (shift, verdict) = preprocess::y16_calibration(peak);
    match verdict {
        preprocess::Y16Calibration::LowRange => tracing::warn!(
            device = %device_path,
            peak,
            frames,
            "Y16 scale calibrated to 8-bit: no sample above 255 during calibration. \
             Correct for a true 8-bit sensor, but a covered or unlit camera at open \
             looks identical and makes later frames clip to white. Set the quirk's \
             y16_bit_depth for this device to pin the scale from hardware instead."
        ),
        preprocess::Y16Calibration::Saturated => tracing::warn!(
            device = %device_path,
            peak,
            frames,
            "Y16 calibration saturated the 16-bit container: an IR glint or hot pixel \
             at open pins a scale that darkens every later frame. Set the quirk's \
             y16_bit_depth for this device to pin the scale from hardware instead."
        ),
        preprocess::Y16Calibration::Normal => {}
    }
    tracing::info!(device = %device_path, shift, peak, frames, "Y16 session scale pinned");
    Ok(shift)
}

/// Bytes per row facelock's converters assume for an uncompressed format.
/// `None` for compressed formats, whose `bytesperline` is not a row size.
fn expected_stride(format: &str, width: u32) -> Option<u32> {
    match format {
        "GREY" | "NV12" => Some(width),
        "Y16" | "YUYV" => Some(width * 2),
        _ => None,
    }
}

use crate::ir_emitter;
use crate::ir_emitter::EmitterXuInfo;
use crate::preprocess;
use crate::quirks::{Quirk, QuirksDb};
use facelock_core::types::CameraCaps;

/// A V4L2 camera for frame capture.
pub struct Camera<'a> {
    stream: Stream<'a>,
    width: u32,
    height: u32,
    format: String,
    rotation: u16,
    /// Device path, stored for IR emitter cleanup on drop.
    device_path: String,
    /// Whether the IR emitter was activated and should be disabled on drop.
    ir_emitter_active: bool,
    /// Emitter XU info, stored for disable on drop when the quirk ref is gone.
    emitter_xu_info: Option<EmitterXuInfo>,
    /// Capabilities computed at construction — see `crate::caps` (gap D8).
    caps: CameraCaps,
    /// Y16 -> 8-bit shift, derived once at open. Never recomputed per frame:
    /// a per-frame scale is contrast normalization upstream of the IR texture
    /// check (see `docs/security.md` §1.C).
    ///
    /// Scoped to this `Camera` value, which is what makes it safe under the
    /// daemon's warm camera hold (ADR 008 §4): a hold reuses this same struct,
    /// so the scale it pinned stays pinned; a reopen constructs a new `Camera`
    /// and recalibrates. Nothing carries a scale across a reopen.
    y16_shift: u8,
}

impl<'a> Camera<'a> {
    /// Resolve, interrogate and open a camera in one step. If `config.path`
    /// is `None`, auto-detects the best available camera. The matched quirk
    /// and the capabilities both come from the resolution — callers that need
    /// the caps *before* opening (pre-flight gates) use
    /// [`crate::caps::ResolvedCamera`] directly and open from that.
    pub fn open(config: &DeviceConfig, quirks: &QuirksDb) -> Result<Camera<'static>> {
        crate::caps::ResolvedCamera::resolve(config, quirks)?.open(config)
    }

    /// Open a camera device with the given configuration and pre-computed
    /// capabilities (from [`crate::caps::ResolvedCamera`] — the one place
    /// caps are derived, so an arbitrary caller cannot assert caps a device
    /// never demonstrated).
    ///
    /// When a `quirk` is provided, its overrides take precedence:
    /// - `format_preference` is prepended to the format priority list.
    /// - `warmup_frames` replaces `config.warmup_frames`.
    /// - `rotation` replaces `config.rotation`.
    /// - `y16_bit_depth` pins the Y16 scale, skipping frame calibration.
    pub(crate) fn open_resolved(
        config: &DeviceConfig,
        quirk: Option<&Quirk>,
        caps: CameraCaps,
    ) -> Result<Camera<'static>> {
        let device_path = match config.path {
            Some(ref p) => p.clone(),
            None => {
                let info = crate::device::auto_detect_device()?;
                info.path
            }
        };

        let dev = Device::with_path(&device_path)
            .map_err(|e| FacelockError::Camera(format!("failed to open {device_path}: {e}")))?;

        // Verify VIDEO_CAPTURE capability
        let v4l_caps = dev.query_caps().map_err(|e| {
            FacelockError::Camera(format!("failed to query caps for {device_path}: {e}"))
        })?;
        if !v4l_caps
            .capabilities
            .contains(v4l::capability::Flags::VIDEO_CAPTURE)
        {
            return Err(FacelockError::Camera(format!(
                "{device_path}: not a video capture device",
            )));
        }

        // Select format by DECODABLE_FORMATS priority. If a quirk specifies a
        // format_preference, prepend it to the priority list. A device that
        // advertises no decodable format (e.g. a raw Bayer sensor node) fails
        // here with an actionable error.
        let formats = dev
            .enum_formats()
            .map_err(|e| FacelockError::Camera(format!("failed to enum formats: {e}")))?;

        let mut preferred: Vec<&str> = Vec::with_capacity(DECODABLE_FORMATS.len() + 1);
        if let Some(fmt_pref) = quirk_format_preference(quirk) {
            tracing::debug!(format = fmt_pref, "quirk: prepending format preference");
            preferred.push(fmt_pref);
        }
        preferred.extend_from_slice(DECODABLE_FORMATS);

        let available: Vec<String> = formats.iter().map(|f| normalize_fourcc(f.fourcc)).collect();
        let selected_fourcc = select_format(&preferred, &available)
            .map(|idx| formats[idx].fourcc)
            .ok_or_else(|| {
                FacelockError::Camera(format!(
                    "{device_path}: no decodable pixel format — device advertises [{}] but \
                     facelock supports [{}]. Raw sensor nodes (e.g. Intel IPU6/IPU7) cannot \
                     be used directly; set device.path to a processed camera instead \
                     (see docs/compatibility.md)",
                    available.join(", "),
                    DECODABLE_FORMATS.join(", "),
                ))
            })?;

        // Set format with resolution capped at 640x480, respecting max_height
        let max_h = config.max_height.min(480);
        let max_w = 640u32;

        let mut fmt = dev
            .format()
            .map_err(|e| FacelockError::Camera(format!("failed to get format: {e}")))?;
        fmt.fourcc = selected_fourcc;
        fmt.width = max_w;
        fmt.height = max_h;
        let fmt = dev
            .set_format(&fmt)
            .map_err(|e| FacelockError::Camera(format!("failed to set format: {e}")))?;

        let width = fmt.width;
        let height = fmt.height;
        let format_str = normalize_fourcc(fmt.fourcc);
        tracing::info!(
            device = %device_path,
            format = %format_str,
            width,
            height,
            "camera format negotiated"
        );

        // The converters index rows by pixel width. ISP devices can pad
        // bytesperline, which would shear every decoded frame silently.
        if let Some(expected) = expected_stride(&format_str, width)
            && fmt.stride != expected
        {
            return Err(FacelockError::Camera(format!(
                "{device_path}: {format_str} bytesperline is {} but facelock expects {expected} \
                     for {width} pixels — padded strides are not supported",
                fmt.stride
            )));
        }

        // Create MMAP stream with `MMAP_BUFFERS` buffers and a capture timeout
        let mut stream = Stream::with_buffers(&dev, Type::VideoCapture, MMAP_BUFFERS)
            .map_err(|e| FacelockError::Camera(format!("failed to create stream: {e}")))?;
        stream.set_timeout(CAPTURE_TIMEOUT);

        // Extract emitter XU info from quirk (if available) for use during
        // enable and later in Drop.
        let emitter_xu_info = quirk.and_then(EmitterXuInfo::from_quirk);

        // Attempt to enable IR emitter if configured
        let ir_emitter_active = if config.ir_emitter {
            match ir_emitter::enable_emitter(&device_path, quirk) {
                Ok(true) => {
                    tracing::info!("IR emitter enabled on {device_path}");
                    true
                }
                Ok(false) => {
                    tracing::debug!("no controllable IR emitter on {device_path}");
                    false
                }
                Err(e) => {
                    tracing::warn!("failed to enable IR emitter on {device_path}: {e}");
                    false
                }
            }
        } else {
            false
        };

        // Pin the Y16 -> 8-bit scale for the whole session. A quirk-declared
        // sensor bit depth is authoritative; otherwise calibrate from a burst
        // of frames. Must run after the IR emitter is enabled, otherwise the
        // scale is derived from unlit frames and every lit frame clips to white.
        let y16_shift = if format_str == "Y16" {
            match quirk_y16_shift(quirk) {
                Some(shift) => {
                    tracing::info!(device = %device_path, shift, "Y16 session scale from quirk bit depth");
                    shift
                }
                None => {
                    // Not `?`: the emitter is already lit and `Camera` does not
                    // exist yet, so an early return here would skip the `Drop`
                    // that is the only thing which turns it back off. Every
                    // other failure path above runs before the emitter is
                    // enabled; this one is the exception, so it cleans up.
                    match calibrate_y16_shift(
                        &mut stream,
                        &device_path,
                        y16_calibration_frames(quirk),
                    ) {
                        Ok(shift) => shift,
                        Err(e) => {
                            if ir_emitter_active
                                && let Some(ref xu_info) = emitter_xu_info
                                && let Err(off) =
                                    ir_emitter::disable_emitter_with_info(&device_path, xu_info)
                            {
                                tracing::warn!(
                                    "failed to disable IR emitter on {device_path} after \
                                             Y16 calibration failed: {off}"
                                );
                            }
                            return Err(e);
                        }
                    }
                }
            }
        } else {
            0
        };

        // Apply quirk overrides for rotation (warmup_frames is handled by the caller
        // since Camera::open doesn't consume warmup frames itself).
        let rotation = quirk.and_then(|q| q.rotation).unwrap_or(config.rotation);
        if let Some(q) = quirk
            && (q.rotation.is_some() || q.warmup_frames.is_some() || q.format_preference.is_some())
        {
            tracing::info!(
                rotation = ?q.rotation,
                warmup_frames = ?q.warmup_frames,
                format_preference = ?q.format_preference,
                "applied quirk overrides for {device_path}"
            );
        }

        Ok(Camera {
            stream,
            width,
            height,
            format: format_str,
            rotation,
            device_path,
            ir_emitter_active,
            emitter_xu_info,
            caps,
            y16_shift,
        })
    }

    /// Capabilities computed at construction. See [`CameraCaps`].
    pub fn capabilities(&self) -> &CameraCaps {
        &self.caps
    }

    /// Return the negotiated pixel format string (e.g. "MJPG", "YUYV", "GREY").
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Capture a single frame with preprocessing (RGB + raw grayscale).
    /// CLAHE is not applied here — callers that need it (e.g. IR texture checks)
    /// should run `preprocess::clahe()` on `frame.gray` themselves.
    pub fn capture(&mut self) -> Result<Frame> {
        let (rgb, width, height) = self.capture_rgb()?;

        // Convert to grayscale (no CLAHE — deferred to callers that need it)
        let gray = preprocess::rgb_to_gray(&rgb, width, height);

        Ok(Frame {
            rgb,
            gray,
            width,
            height,
        })
    }

    /// Capture a frame and return only the RGB data, skipping grayscale
    /// conversion and CLAHE. Use this for preview where no face detection runs.
    pub fn capture_rgb_only(&mut self) -> Result<Frame> {
        let (rgb, width, height) = self.capture_rgb()?;

        Ok(Frame {
            rgb,
            gray: Vec::new(),
            width,
            height,
        })
    }

    /// Internal: capture and convert to RGB, applying downscale and rotation.
    fn capture_rgb(&mut self) -> Result<(Vec<u8>, u32, u32)> {
        // stream.next() uses the v4l built-in poll with CAPTURE_TIMEOUT.
        // If the camera stops producing frames, this returns TimedOut error
        // instead of blocking forever.
        let (buf, _meta) = self
            .stream
            .next()
            .map_err(|e| FacelockError::Camera(format!("capture failed: {e}")))?;

        let rgb: Vec<u8> = match self.format.as_str() {
            "GREY" => {
                // Replicate single channel 3x
                let mut rgb = Vec::with_capacity(buf.len() * 3);
                for &p in buf {
                    rgb.push(p);
                    rgb.push(p);
                    rgb.push(p);
                }
                rgb
            }
            "Y16" => {
                // 16-bit grayscale -> 8-bit at the session-fixed scale, replicated 3x
                let gray = preprocess::y16_to_gray(buf, self.y16_shift);
                let mut rgb = Vec::with_capacity(gray.len() * 3);
                for &p in &gray {
                    rgb.push(p);
                    rgb.push(p);
                    rgb.push(p);
                }
                rgb
            }
            "YUYV" => preprocess::yuyv_to_rgb(buf, self.width, self.height),
            "NV12" => preprocess::nv12_to_rgb(buf, self.width, self.height).ok_or_else(|| {
                FacelockError::Camera(format!(
                    "NV12 frame too short: {} bytes for {}x{}",
                    buf.len(),
                    self.width,
                    self.height
                ))
            })?,
            "MJPG" => {
                let reader = ImageReader::with_format(Cursor::new(buf), image::ImageFormat::Jpeg)
                    .decode()
                    .map_err(|e| FacelockError::Camera(format!("MJPG decode failed: {e}")))?;
                reader.to_rgb8().into_raw()
            }
            other => {
                return Err(FacelockError::Camera(format!(
                    "unsupported format: {other}"
                )));
            }
        };

        let mut width = self.width;
        let mut height = self.height;
        let mut rgb = rgb;

        // Downscale if needed
        if height > self.height {
            let img = image::RgbImage::from_raw(width, height, rgb)
                .ok_or_else(|| FacelockError::Camera("failed to create image for resize".into()))?;
            let aspect = width as f64 / height as f64;
            let new_h = self.height;
            let new_w = (new_h as f64 * aspect) as u32;
            let resized =
                image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle);
            width = new_w;
            height = new_h;
            rgb = resized.into_raw();
        }

        // Apply rotation
        if self.rotation != 0 {
            let img = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
                FacelockError::Camera("failed to create image for rotation".into())
            })?;
            let rotated = match self.rotation {
                90 => image::imageops::rotate90(&img),
                180 => image::imageops::rotate180(&img),
                270 => image::imageops::rotate270(&img),
                _ => img,
            };
            width = rotated.width();
            height = rotated.height();
            rgb = rotated.into_raw();
        }

        Ok((rgb, width, height))
    }
}

/// Check if a frame is too dark to process.
///
/// A free function, not a `CameraSource` method: darkness is a property of a
/// frame, and hanging it off the trait was what made the trait non-object-safe
/// (`where Self: Sized`).
///
/// The thresholds are always passed in, from `device.dark_threshold` and
/// `device.dark_pixel_value`. There is deliberately no default-threshold
/// variant: one existed, hardcoding the same 0.6/10 the config defaults to,
/// which is a second source of truth that drifts silently the day those
/// defaults change.
///
/// Uses both a per-pixel threshold check and mean brightness:
/// - If the fraction of pixels below `dark_value` exceeds `threshold`, the frame is dark.
/// - If the mean brightness is below 20.0, the frame is dark (catches uniformly dim frames).
pub fn is_dark_with_config(frame: &Frame, threshold: f32, dark_value: u8) -> bool {
    if frame.gray.is_empty() {
        return false;
    }
    // Per-pixel dark ratio
    let dark_count = frame.gray.iter().filter(|&&p| p < dark_value).count();
    let dark_ratio = dark_count as f32 / frame.gray.len() as f32;
    if dark_ratio >= threshold {
        return true;
    }
    // Mean brightness check (catches uniformly dim frames)
    let sum: u64 = frame.gray.iter().map(|&p| p as u64).sum();
    let mean = sum as f32 / frame.gray.len() as f32;
    mean < 20.0
}

impl Drop for Camera<'_> {
    fn drop(&mut self) {
        if self.ir_emitter_active
            && let Some(ref xu_info) = self.emitter_xu_info
        {
            match ir_emitter::disable_emitter_with_info(&self.device_path, xu_info) {
                Ok(()) => tracing::debug!("IR emitter disabled on {}", self.device_path),
                Err(e) => {
                    tracing::warn!("failed to disable IR emitter on {}: {e}", self.device_path)
                }
            }
        }
    }
}

impl CameraSource for Camera<'_> {
    fn capabilities(&self) -> &CameraCaps {
        Camera::capabilities(self)
    }

    fn capture(&mut self) -> Result<Frame> {
        Camera::capture(self)
    }

    fn capture_rgb_only(&mut self) -> Result<Frame> {
        Camera::capture_rgb_only(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fourccs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_format_prefers_priority_order() {
        // Device advertises MJPG and YUYV: YUYV wins (earlier in priority).
        let available = fourccs(&["MJPG", "YUYV"]);
        assert_eq!(select_format(DECODABLE_FORMATS, &available), Some(1));
        // GREY beats everything.
        let available = fourccs(&["MJPG", "YUYV", "GREY"]);
        assert_eq!(select_format(DECODABLE_FORMATS, &available), Some(2));
    }

    #[test]
    fn select_format_picks_nv12_over_mjpg() {
        let available = fourccs(&["MJPG", "NV12"]);
        assert_eq!(select_format(DECODABLE_FORMATS, &available), Some(1));
    }

    #[test]
    fn normalize_fourcc_strips_v4l2_padding() {
        // v4l reports "Y16 " with its FourCC trailing space; every comparison
        // downstream expects the normalized form.
        let available = vec![normalize_fourcc(v4l::format::FourCC::new(b"Y16 "))];
        assert_eq!(available[0], "Y16");
        assert_eq!(select_format(DECODABLE_FORMATS, &available), Some(0));
    }

    #[test]
    fn decodable_formats_negotiation_order_is_pinned() {
        // Changing this order changes the documented negotiation contract —
        // update docs/contracts.md with it.
        assert_eq!(DECODABLE_FORMATS, &["GREY", "Y16", "YUYV", "NV12", "MJPG"]);
    }

    #[test]
    fn expected_stride_is_the_row_size_of_uncompressed_formats() {
        assert_eq!(expected_stride("GREY", 640), Some(640));
        assert_eq!(expected_stride("NV12", 640), Some(640));
        assert_eq!(expected_stride("Y16", 640), Some(1280));
        assert_eq!(expected_stride("YUYV", 640), Some(1280));
        assert_eq!(expected_stride("MJPG", 640), None);
    }

    #[test]
    fn select_format_rejects_undecodable_only_device() {
        // Raw Bayer IPU7 node (issue #89): nothing decodable -> None, so
        // Camera::open fails fast instead of negotiating an unusable format.
        let available = fourccs(&["SGRBG10", "SGBRG10"]);
        assert_eq!(select_format(DECODABLE_FORMATS, &available), None);
    }

    #[test]
    fn select_format_quirk_preference_first() {
        let mut preferred = vec!["MJPG"];
        preferred.extend_from_slice(DECODABLE_FORMATS);
        let available = fourccs(&["GREY", "MJPG"]);
        assert_eq!(select_format(&preferred, &available), Some(1));
    }

    fn quirk_with_format(format_preference: &str) -> Quirk {
        Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: None,
            force_ir: None,
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: Some(format_preference.into()),
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        }
    }

    #[test]
    fn quirk_format_preference_accepts_decodable() {
        let quirk = quirk_with_format("GREY");
        assert_eq!(quirk_format_preference(Some(&quirk)), Some("GREY"));
        assert_eq!(quirk_format_preference(None), None);
    }

    #[test]
    fn quirk_format_preference_accepts_the_padded_v4l2_spelling() {
        // `config/quirks.d/00-defaults.toml` writes `format_preference = "Y16 "`
        // for the three RealSense entries. `QuirksDb::load_file` normalizes it,
        // but this predicate must not DEPEND on that having happened: an exact
        // comparison here would turn a regression in the loader into three
        // silently-dropped IR-sensor preferences plus a misleading "not a
        // decodable format" warning.
        let quirk = quirk_with_format("Y16 ");
        assert_eq!(quirk_format_preference(Some(&quirk)), Some("Y16"));
    }

    #[test]
    fn quirk_y16_shift_uses_declared_bit_depth() {
        let mut quirk = quirk_with_format("Y16");
        quirk.y16_bit_depth = Some(10);
        assert_eq!(quirk_y16_shift(Some(&quirk)), Some(2));
        quirk.y16_bit_depth = Some(16);
        assert_eq!(quirk_y16_shift(Some(&quirk)), Some(8));
    }

    #[test]
    fn quirk_y16_shift_absent_falls_back_to_calibration() {
        // No quirk, and a quirk without the field, both leave the burst
        // calibration in charge.
        assert_eq!(quirk_y16_shift(None), None);
        assert_eq!(quirk_y16_shift(Some(&quirk_with_format("Y16"))), None);
    }

    #[test]
    fn y16_calibration_frames_follows_declared_warmup() {
        // Default burst when the device says nothing.
        assert_eq!(y16_calibration_frames(None), Y16_CALIBRATION_FRAMES);
        let mut quirk = quirk_with_format("Y16");
        assert_eq!(y16_calibration_frames(Some(&quirk)), 8);
        // A camera that declares a longer warmup (RealSense SR300) gets a burst
        // that covers it; a shorter one never shortens the default.
        quirk.warmup_frames = Some(10);
        assert_eq!(y16_calibration_frames(Some(&quirk)), 10);
        quirk.warmup_frames = Some(1);
        assert_eq!(y16_calibration_frames(Some(&quirk)), 8);
    }

    #[test]
    fn quirk_y16_shift_ignores_impossible_bit_depth() {
        let mut quirk = quirk_with_format("Y16");
        quirk.y16_bit_depth = Some(24);
        assert_eq!(quirk_y16_shift(Some(&quirk)), None);
        quirk.y16_bit_depth = Some(4);
        assert_eq!(quirk_y16_shift(Some(&quirk)), None);
    }

    #[test]
    fn quirk_format_preference_ignores_undecodable() {
        // A quirk naming a format facelock cannot decode would otherwise win
        // negotiation and then fail every capture.
        let quirk = quirk_with_format("SGRBG10");
        assert_eq!(quirk_format_preference(Some(&quirk)), None);
    }

    #[test]
    fn all_black_frame_is_dark_with_config() {
        let frame = Frame {
            rgb: vec![0; 30],
            gray: vec![0; 10],
            width: 5,
            height: 2,
        };
        assert!(is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn bright_frame_is_not_dark_with_config() {
        let frame = Frame {
            rgb: vec![128; 30],
            gray: vec![128; 10],
            width: 5,
            height: 2,
        };
        assert!(!is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn custom_threshold_below_cutoff() {
        // 50% dark pixels (5/10), 60% threshold = not dark by ratio
        // but mean = (5*5 + 5*128)/10 = 66.5 > 20 so not dark by mean either
        let mut gray = vec![128u8; 10];
        for p in gray.iter_mut().take(5) {
            *p = 5;
        }
        let frame = Frame {
            rgb: vec![0; 30],
            gray,
            width: 5,
            height: 2,
        };
        assert!(!is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn custom_threshold_above_cutoff() {
        // 70% dark pixels (7/10), 60% threshold = dark by ratio
        let mut gray = vec![128u8; 10];
        for p in gray.iter_mut().take(7) {
            *p = 5;
        }
        let frame = Frame {
            rgb: vec![0; 30],
            gray,
            width: 5,
            height: 2,
        };
        assert!(is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn dim_frame_caught_by_mean_brightness() {
        // All pixels at 15 (above dark_value=10) so ratio=0, but mean=15 < 20
        let frame = Frame {
            rgb: vec![0; 30],
            gray: vec![15; 10],
            width: 5,
            height: 2,
        };
        assert!(is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn empty_gray_is_not_dark_with_config() {
        let frame = Frame {
            rgb: vec![0; 30],
            gray: vec![],
            width: 5,
            height: 2,
        };
        assert!(!is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    #[ignore]
    fn camera_open_and_capture() {
        let config = DeviceConfig {
            path: Some("/dev/video0".into()),
            max_height: 480,
            rotation: 0,
            warmup_frames: 5,
            dark_threshold: 0.6,
            dark_pixel_value: 10,
            ir_emitter: false,
            camera_release_secs: 5,
            camera_release_after_success_secs: 0,
        };
        let mut cam = Camera::open(&config, &QuirksDb::load()).expect("failed to open camera");
        let frame = cam.capture().expect("failed to capture frame");
        assert!(frame.width > 0);
        assert!(frame.height > 0);
        assert_eq!(frame.rgb.len(), (frame.width * frame.height * 3) as usize);
        assert_eq!(frame.gray.len(), (frame.width * frame.height) as usize);
    }
}
