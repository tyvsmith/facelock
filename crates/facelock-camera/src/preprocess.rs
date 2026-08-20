use facelock_core::types::BoundingBox;

/// BT.601 chroma coefficients in fixed point (multiplied by 1024).
///
/// One copy, shared by every YUV converter here. They were duplicated per
/// converter once, with a test asserting the two produced identical output —
/// which is the test you write when the math has been copied rather than
/// shared. That test now guards this.
const C_RV: i32 = 1436; // 1.402 * 1024
const C_GU: i32 = 352; // 0.344136 * 1024
const C_GV: i32 = 731; // 0.714136 * 1024
const C_BU: i32 = 1814; // 1.772 * 1024

/// The R/G/B chroma contributions of one (u, v) pair, centered and scaled to
/// Q10. A 4:2:2 or 4:2:0 converter computes this once per chroma sample and
/// reuses it for every luma sample the pair covers.
#[inline]
fn chroma_q10(u: u8, v: u8) -> (i32, i32, i32) {
    let u = u as i32 - 128;
    let v = v as i32 - 128;
    (C_RV * v, -(C_GU * u + C_GV * v), C_BU * u)
}

/// One YUV pixel to RGB, given the chroma contributions from [`chroma_q10`].
#[inline]
fn yuv_to_rgb_px(y: u8, (cr, cg, cb): (i32, i32, i32)) -> [u8; 3] {
    let y = (y as i32) << 10; // scale Y to Q10
    [
        ((y + cr) >> 10).clamp(0, 255) as u8,
        ((y + cg) >> 10).clamp(0, 255) as u8,
        ((y + cb) >> 10).clamp(0, 255) as u8,
    ]
}

/// Convert YUYV (YUV 4:2:2) packed data to RGB.
/// Each 4-byte YUYV group produces 2 RGB pixels.
/// Uses fixed-point integer math (Q10) to avoid per-pixel float conversion.
pub fn yuyv_to_rgb(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgb = Vec::with_capacity(pixel_count * 3);

    // Process 4 bytes at a time (2 pixels), which share one chroma pair.
    for chunk in data.chunks_exact(4) {
        let chroma = chroma_q10(chunk[1], chunk[3]);
        for &y_val in &[chunk[0], chunk[2]] {
            rgb.extend_from_slice(&yuv_to_rgb_px(y_val, chroma));
        }
    }

    rgb
}

/// Convert NV12 (YUV 4:2:0 semi-planar) data to RGB.
/// Layout: full-resolution Y plane followed by one interleaved UV plane at
/// half resolution in both dimensions (each UV pair covers a 2x2 pixel block).
/// Uses the same fixed-point (Q10) coefficients as [`yuyv_to_rgb`].
/// Returns `None` if `data` is shorter than a full NV12 frame.
pub fn nv12_to_rgb(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let y_len = w * h;
    // One UV pair covers a 2x2 block, so a UV row is 2 * ceil(w/2) bytes — for
    // odd widths that is wider than the Y row.
    let uv_stride = 2 * w.div_ceil(2);
    if data.len() < y_len + uv_stride * h.div_ceil(2) {
        return None;
    }

    let uv_plane = &data[y_len..];
    let mut rgb = Vec::with_capacity(y_len * 3);
    for row in 0..h {
        let uv_row = &uv_plane[(row / 2) * uv_stride..];
        for col in 0..w {
            let chroma = chroma_q10(uv_row[(col / 2) * 2], uv_row[(col / 2) * 2 + 1]);
            rgb.extend_from_slice(&yuv_to_rgb_px(data[row * w + col], chroma));
        }
    }
    Some(rgb)
}

/// Brightest 16-bit sample in a Y16 buffer (little-endian).
///
/// Callers accumulate this across a burst of calibration frames, which raises
/// the estimate toward the sensor's full scale. It does not bound it from
/// above: this is an unbounded max over every pixel, so a single stuck pixel
/// or specular IR glint sets it alone and pins a shift that is too large for
/// the rest of the scene. See `calibrate_y16_shift` for what that costs and
/// why a quirk's `y16_bit_depth` is the way out.
pub fn y16_peak(data: &[u8]) -> u16 {
    data.chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .max()
        .unwrap_or(0)
}

/// Right-shift implied by an observed Y16 `peak` sample.
///
/// Many IR sensors deliver 10/12-bit samples right-justified in the 16-bit
/// container, so a plain `>> 8` yields near-black frames. The shift comes from
/// the highest set bit of the peak, floored at 0 so samples that already fit in
/// 8 bits pass through unshifted.
pub fn y16_shift_from_peak(peak: u16) -> u8 {
    (16 - peak.leading_zeros()).saturating_sub(8) as u8
}

/// Right-shift that maps a Y16 container onto 8 bits, derived from the
/// brightest sample in `data` for non-auth conversion.
///
/// The shift MUST be derived once per camera session and reused for every
/// frame. Recomputing it per frame is contrast normalization upstream of
/// [`check_ir_texture`], whose `min_stddev` cutoff requires verified hardware
/// scale provenance instead (see `docs/security.md` §1.C).
///
/// Test-only, deliberately: a one-shot whole-buffer helper is the shape a
/// per-frame call takes, and shipping it as API invites exactly the recompute
/// the paragraph above forbids. Non-auth capture pins its conversion scale
/// from a burst instead (`calibrate_y16_shift`); this remains so the tests can
/// pin the peak-to-shift mapping in one call.
#[cfg(test)]
fn y16_shift(data: &[u8]) -> u8 {
    y16_shift_from_peak(y16_peak(data))
}

/// Right-shift for a sensor bit depth supplied by the hardware quirks DB.
///
/// `None` for a depth that cannot describe a Y16 sample (below 8 bits, or above
/// the 16-bit container); the caller never trusts a typo as authentication
/// provenance. Non-auth conversion may still fall back to frame calibration.
pub fn y16_shift_from_bit_depth(bit_depth: u8) -> Option<u8> {
    (8..=16).contains(&bit_depth).then(|| bit_depth - 8)
}

/// What a Y16 calibration burst's peak says about the scale it produced.
///
/// The shift itself is usable in every case — this only distinguishes the peaks
/// that are equally consistent with a correctly-calibrated sensor and with a
/// camera that was covered, unlit, or still pre-AGC while the burst ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Y16Calibration {
    /// The burst never saw a sample above 8 bits. True for a genuinely 8-bit
    /// sensor padded into Y16, and equally true for a covered lens.
    LowRange,
    /// The burst saturated the 16-bit container, which is what an IR glint (or
    /// a hot pixel) looks like on a sensor whose real range is narrower.
    Saturated,
    /// Peak landed inside the container with room on both sides.
    Normal,
}

/// Pin a session Y16 shift from the peak sample of a calibration burst, along
/// with a verdict on how trustworthy that peak is.
pub fn y16_calibration(peak: u16) -> (u8, Y16Calibration) {
    let verdict = match peak {
        u16::MAX => Y16Calibration::Saturated,
        p if p < 256 => Y16Calibration::LowRange,
        _ => Y16Calibration::Normal,
    };
    (y16_shift_from_peak(peak), verdict)
}

/// Convert Y16 (16-bit little-endian grayscale) data to 8-bit grayscale using
/// the session-fixed `shift` from [`y16_shift`].
///
/// Samples brighter than the session scale (e.g. an IR glint) clamp to white
/// rather than wrapping to black.
pub fn y16_to_gray(data: &[u8], shift: u8) -> Vec<u8> {
    data.chunks_exact(2)
        .map(|c| (u16::from_le_bytes([c[0], c[1]]) >> shift).min(255) as u8)
        .collect()
}

/// Convert RGB image to grayscale using luminance formula.
/// Uses fixed-point integer math (Q8) to avoid per-pixel float conversion.
pub fn rgb_to_gray(rgb: &[u8], _width: u32, _height: u32) -> Vec<u8> {
    // Fixed-point coefficients (multiplied by 256)
    const CR: u32 = 77; // 0.299 * 256
    const CG: u32 = 150; // 0.587 * 256
    const CB: u32 = 29; // 0.114 * 256

    rgb.chunks_exact(3)
        .map(|px| ((CR * px[0] as u32 + CG * px[1] as u32 + CB * px[2] as u32) >> 8) as u8)
        .collect()
}

/// CLAHE: Contrast Limited Adaptive Histogram Equalization.
/// Uses 8x8 tile grid with clip limit of 2.0 * average, bilinear interpolation between tile CDFs.
pub fn clahe(gray: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let tiles_x: usize = 8;
    let tiles_y: usize = 8;
    let tile_w = w as f64 / tiles_x as f64;
    let tile_h = h as f64 / tiles_y as f64;

    // Build CDF for each tile
    let mut cdfs = vec![vec![0u8; 256]; tiles_x * tiles_y];

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let x_start = (tx as f64 * tile_w) as usize;
            let y_start = (ty as f64 * tile_h) as usize;
            let x_end = ((tx + 1) as f64 * tile_w) as usize;
            let y_end = ((ty + 1) as f64 * tile_h) as usize;
            let x_end = x_end.min(w);
            let y_end = y_end.min(h);

            // Histogram
            let mut hist = [0u32; 256];
            let mut count = 0u32;
            for row in y_start..y_end {
                for col in x_start..x_end {
                    hist[gray[row * w + col] as usize] += 1;
                    count += 1;
                }
            }

            if count == 0 {
                // Empty tile, identity mapping
                for (i, val) in cdfs[ty * tiles_x + tx].iter_mut().enumerate() {
                    *val = i as u8;
                }
                continue;
            }

            // Clip histogram
            let avg = count as f64 / 256.0;
            let clip_limit = (2.0 * avg).max(1.0) as u32;
            let mut excess = 0u32;
            for bin in &mut hist {
                if *bin > clip_limit {
                    excess += *bin - clip_limit;
                    *bin = clip_limit;
                }
            }
            // Redistribute excess equally
            let redist = excess / 256;
            let residual = (excess % 256) as usize;
            for (i, bin) in hist.iter_mut().enumerate() {
                *bin += redist;
                if i < residual {
                    *bin += 1;
                }
            }

            // Build CDF
            let mut cdf = [0u32; 256];
            cdf[0] = hist[0];
            for i in 1..256 {
                cdf[i] = cdf[i - 1] + hist[i];
            }

            // Normalize CDF to [0, 255]
            let cdf_min = cdf.iter().copied().find(|&v| v > 0).unwrap_or(0);
            let total = cdf[255];
            let denom = total.saturating_sub(cdf_min);
            for i in 0..256 {
                if denom == 0 {
                    cdfs[ty * tiles_x + tx][i] = i as u8;
                } else {
                    cdfs[ty * tiles_x + tx][i] =
                        (((cdf[i].saturating_sub(cdf_min)) as f64 / denom as f64) * 255.0) as u8;
                }
            }
        }
    }

    // Apply bilinear interpolation
    let mut output = vec![0u8; w * h];
    for row in 0..h {
        for col in 0..w {
            // Map pixel to tile coordinate space
            // Center of each tile in pixel coords
            let fy = (row as f64 + 0.5) / tile_h - 0.5;
            let fx = (col as f64 + 0.5) / tile_w - 0.5;

            let tx0 = (fx.floor() as isize).clamp(0, tiles_x as isize - 1) as usize;
            let ty0 = (fy.floor() as isize).clamp(0, tiles_y as isize - 1) as usize;
            let tx1 = (tx0 + 1).min(tiles_x - 1);
            let ty1 = (ty0 + 1).min(tiles_y - 1);

            let ax = (fx - tx0 as f64).clamp(0.0, 1.0);
            let ay = (fy - ty0 as f64).clamp(0.0, 1.0);

            let pixel = gray[row * w + col] as usize;

            let v00 = cdfs[ty0 * tiles_x + tx0][pixel] as f64;
            let v10 = cdfs[ty0 * tiles_x + tx1][pixel] as f64;
            let v01 = cdfs[ty1 * tiles_x + tx0][pixel] as f64;
            let v11 = cdfs[ty1 * tiles_x + tx1][pixel] as f64;

            let top = v00 * (1.0 - ax) + v10 * ax;
            let bot = v01 * (1.0 - ax) + v11 * ax;
            let val = top * (1.0 - ay) + bot * ay;

            output[row * w + col] = val.clamp(0.0, 255.0) as u8;
        }
    }

    output
}

/// Extract a rectangular region from a grayscale image given a bounding box.
pub fn extract_bbox_region(gray: &[u8], bbox: &BoundingBox, width: u32) -> Vec<u8> {
    let w = width as usize;
    let img_h = gray.len() / w;

    let x0 = (bbox.x.max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = (bbox.y.max(0.0) as usize).min(img_h.saturating_sub(1));
    let x1 = ((bbox.x + bbox.width).max(0.0) as usize).min(w);
    let y1 = ((bbox.y + bbox.height).max(0.0) as usize).min(img_h);

    let mut region = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for row in y0..y1 {
        for col in x0..x1 {
            region.push(gray[row * w + col]);
        }
    }
    region
}

/// Check that face region has sufficient IR texture (real skin vs flat photo/screen).
/// Returns true if texture is consistent with a real face.
///
/// `min_stddev` is the rejection cutoff for the per-face standard deviation of
/// pixel intensity. It MUST be measured on the RAW grayscale frame — running this
/// on a CLAHE-equalized frame inflates the std_dev of flat surfaces and masks
/// photo/screen replays. Configurable via `security.ir_texture_min_stddev`.
pub fn check_ir_texture(gray: &[u8], bbox: &BoundingBox, width: u32, min_stddev: f32) -> bool {
    let face_pixels = extract_bbox_region(gray, bbox, width);
    if face_pixels.is_empty() {
        return false;
    }
    let mean = face_pixels.iter().map(|&p| p as f32).sum::<f32>() / face_pixels.len() as f32;
    let variance = face_pixels
        .iter()
        .map(|&p| (p as f32 - mean).powi(2))
        .sum::<f32>()
        / face_pixels.len() as f32;
    let std_dev = variance.sqrt();
    std_dev > min_stddev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_to_rgb_known_values() {
        // Y=128, U=128, V=128 -> neutral gray
        // Y0=128, U=128(->0), Y1=128, V=128(->0) => R=128, G=128, B=128
        let data = [128u8, 128, 128, 128];
        let rgb = yuyv_to_rgb(&data, 2, 1);
        assert_eq!(rgb.len(), 6);
        // With U=128 and V=128 (both become 0.0 after subtracting 128),
        // R = Y, G = Y, B = Y = 128
        assert_eq!(rgb[0], 128);
        assert_eq!(rgb[1], 128);
        assert_eq!(rgb[2], 128);
        assert_eq!(rgb[3], 128);
        assert_eq!(rgb[4], 128);
        assert_eq!(rgb[5], 128);
    }

    #[test]
    fn nv12_to_rgb_neutral_gray() {
        // 2x2 frame: Y plane all 128, one UV pair at (128, 128) -> neutral gray
        let data = [128u8, 128, 128, 128, 128, 128];
        let rgb = nv12_to_rgb(&data, 2, 2).expect("full frame");
        assert_eq!(rgb.len(), 12);
        assert!(rgb.iter().all(|&c| c == 128));
    }

    #[test]
    fn nv12_to_rgb_red_chroma() {
        // Y=128, U=128 (neutral), V=255 -> strong red shift, like YUYV math:
        // R = 128 + 1.402*127 ~= 306 -> clamped 255; B = 128 (U neutral)
        let data = [128u8, 128, 128, 128, 128, 255];
        let rgb = nv12_to_rgb(&data, 2, 2).expect("full frame");
        for px in rgb.chunks_exact(3) {
            assert_eq!(px[0], 255, "red channel should clamp high");
            assert!(px[1] < 100, "green pulled down by positive V");
            assert_eq!(px[2], 128, "blue unaffected by neutral U");
        }
    }

    #[test]
    fn nv12_to_rgb_matches_yuyv_for_same_yuv() {
        // The same (Y, U, V) triple must produce identical RGB through both
        // converters (they share coefficients).
        let (y, u, v) = (90u8, 100u8, 200u8);
        let yuyv = yuyv_to_rgb(&[y, u, y, v], 2, 1);
        let nv12 = nv12_to_rgb(&[y, y, y, y, u, v], 2, 2).expect("full frame");
        assert_eq!(&yuyv[0..3], &nv12[0..3]);
    }

    #[test]
    fn nv12_to_rgb_short_buffer_returns_none() {
        // Y plane only, missing the UV plane entirely.
        let data = [128u8; 4];
        assert!(nv12_to_rgb(&data, 2, 2).is_none());
    }

    #[test]
    fn nv12_to_rgb_odd_height_uses_last_uv_row() {
        // 2x3 frame: Y plane 6 bytes + UV plane 2 * ceil(3/2) = 4 bytes.
        let mut data = vec![128u8; 6];
        data.extend_from_slice(&[128, 128, 128, 128]);
        let rgb = nv12_to_rgb(&data, 2, 3).expect("full frame");
        assert_eq!(rgb.len(), 18);
        assert!(rgb.iter().all(|&c| c == 128));
    }

    #[test]
    fn nv12_to_rgb_odd_width_uses_padded_uv_stride() {
        // 3x3 frame: a UV row is 2 * ceil(3/2) = 4 bytes, wider than the Y row.
        // Sizing the guard by the Y width instead accepts 15 bytes and then
        // indexes past the end of the UV plane.
        let mut data = vec![128u8; 9];
        data.extend_from_slice(&[128; 8]);
        let rgb = nv12_to_rgb(&data, 3, 3).expect("full frame");
        assert_eq!(rgb.len(), 27);
        assert!(rgb.iter().all(|&c| c == 128));
        // 15 bytes is what the Y-width guard would have accepted.
        assert!(nv12_to_rgb(&data[..15], 3, 3).is_none());
    }

    #[test]
    fn y16_full_range_uses_high_byte() {
        // 16-bit full-scale data: 0xFFFF -> 255, 0x8000 -> 128
        let data = [0xFF, 0xFF, 0x00, 0x80];
        assert_eq!(y16_shift(&data), 8);
        assert_eq!(y16_to_gray(&data, 8), vec![255, 128]);
    }

    #[test]
    fn y16_ten_bit_data_maps_to_full_range() {
        // 10-bit sensor data right-justified in the 16-bit container:
        // full-scale 1023 must map near 255, not to 3 (which >>8 would give).
        let data = [
            0xFF, 0x03, // 1023
            0x00, 0x02, // 512
        ];
        assert_eq!(y16_shift(&data), 2);
        assert_eq!(y16_to_gray(&data, 2), vec![255, 128]);
    }

    #[test]
    fn y16_dim_frame_stays_dim_at_the_session_shift() {
        // Session shift derived once from a full-scale 10-bit frame.
        let shift = y16_shift(&[0xFF, 0x03]);
        assert_eq!(shift, 2);
        // A dim frame later in the session must stay dim. Deriving the shift
        // from this frame's own max (300 -> shift 1) would instead have
        // measured [140, 145, 150, 125] — twice the intensity, and a different
        // std_dev for the IR texture check.
        let data = [0x18, 0x01, 0x22, 0x01, 0x2C, 0x01, 0xFA, 0x00];
        assert_eq!(y16_to_gray(&data, shift), vec![70, 72, 75, 62]);
    }

    #[test]
    fn y16_bright_pixel_clamps_instead_of_wrapping() {
        // A saturated glint pixel above the session scale clamps to white. At a
        // per-frame shift it would have rescaled (and darkened) the whole frame;
        // truncating instead of clamping would wrap 1024 >> 2 to black.
        let shift = y16_shift(&[0xFF, 0x03]);
        let data = [0xFF, 0xFF, 0x00, 0x04, 0x2C, 0x01];
        assert_eq!(y16_to_gray(&data, shift), vec![255, 255, 75]);
    }

    #[test]
    fn y16_all_zero_frame() {
        let data = [0u8; 8];
        assert_eq!(y16_shift(&data), 0);
        assert_eq!(y16_to_gray(&data, 0), vec![0, 0, 0, 0]);
    }

    #[test]
    fn y16_peak_accumulates_toward_the_true_bit_depth() {
        // A dark pre-AGC frame alone pins an 8-bit scale that clips every later
        // 10-bit frame to white. Taking the peak across a burst recovers the
        // real depth, and can never overshoot it: no sample can exceed the
        // sensor's full scale.
        let dark = [0x40, 0x00, 0x10, 0x00]; // 64, 16
        let warm = [0xFF, 0x03, 0x00, 0x02]; // 1023, 512
        assert_eq!(y16_shift_from_peak(y16_peak(&dark)), 0);
        let burst = y16_peak(&dark).max(y16_peak(&warm));
        assert_eq!(y16_shift_from_peak(burst), 2);
    }

    #[test]
    fn y16_shift_from_bit_depth_is_the_quirk_channel() {
        assert_eq!(y16_shift_from_bit_depth(8), Some(0));
        assert_eq!(y16_shift_from_bit_depth(10), Some(2));
        assert_eq!(y16_shift_from_bit_depth(12), Some(4));
        assert_eq!(y16_shift_from_bit_depth(16), Some(8));
    }

    #[test]
    fn y16_shift_from_bit_depth_rejects_out_of_range() {
        // A depth outside the container is a quirks-file typo, not hardware
        // truth: the caller warns and calibrates from frames instead.
        assert_eq!(y16_shift_from_bit_depth(0), None);
        assert_eq!(y16_shift_from_bit_depth(7), None);
        assert_eq!(y16_shift_from_bit_depth(17), None);
        assert_eq!(y16_shift_from_bit_depth(u8::MAX), None);
    }

    #[test]
    fn y16_calibration_flags_suspicious_peaks() {
        // Indistinguishable from a covered lens.
        assert_eq!(y16_calibration(0), (0, Y16Calibration::LowRange));
        assert_eq!(y16_calibration(255), (0, Y16Calibration::LowRange));
        // Full-container saturation: an IR glint on a narrower sensor.
        assert_eq!(y16_calibration(u16::MAX), (8, Y16Calibration::Saturated));
        // Ordinary 10/12-bit peaks.
        assert_eq!(y16_calibration(1023), (2, Y16Calibration::Normal));
        assert_eq!(y16_calibration(4095), (4, Y16Calibration::Normal));
    }

    #[test]
    fn rgb_to_gray_pure_red() {
        let rgb = [255u8, 0, 0];
        let gray = rgb_to_gray(&rgb, 1, 1);
        assert_eq!(gray.len(), 1);
        // 0.299 * 255 = 76.245
        assert!((gray[0] as i32 - 76).unsigned_abs() <= 1);
    }

    #[test]
    fn rgb_to_gray_pure_green() {
        let rgb = [0u8, 255, 0];
        let gray = rgb_to_gray(&rgb, 1, 1);
        // 0.587 * 255 = 149.685
        assert!((gray[0] as i32 - 150).unsigned_abs() <= 1);
    }

    #[test]
    fn rgb_to_gray_pure_blue() {
        let rgb = [0u8, 0, 255];
        let gray = rgb_to_gray(&rgb, 1, 1);
        // 0.114 * 255 = 29.07
        assert!((gray[0] as i32 - 29).unsigned_abs() <= 1);
    }

    #[test]
    fn clahe_uniform_stays_roughly_uniform() {
        // 64x64 image with uniform value 128
        let w = 64u32;
        let h = 64u32;
        let gray = vec![128u8; (w * h) as usize];
        let result = clahe(&gray, w, h);
        assert_eq!(result.len(), gray.len());
        // All pixels should map to the same value (uniform input)
        let min = *result.iter().min().unwrap();
        let max = *result.iter().max().unwrap();
        // Tolerance: uniform image should stay roughly uniform
        assert!(
            (max - min) <= 10,
            "CLAHE on uniform image should stay uniform, got range {min}..{max}"
        );
    }

    const TEST_MIN_STDDEV: f32 = 10.0;

    #[test]
    fn check_ir_texture_uniform_region_false() {
        let w = 64u32;
        let h = 64u32;
        let gray = vec![128u8; (w * h) as usize];
        let bbox = BoundingBox {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        assert!(!check_ir_texture(&gray, &bbox, w, TEST_MIN_STDDEV));
    }

    #[test]
    fn check_ir_texture_varied_region_true() {
        let w = 64u32;
        let h = 64u32;
        let mut gray = vec![0u8; (w * h) as usize];
        // Fill region with varied values
        for row in 10..30 {
            for col in 10..30 {
                gray[row * w as usize + col] = ((row * 13 + col * 7) % 256) as u8;
            }
        }
        let bbox = BoundingBox {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        assert!(check_ir_texture(&gray, &bbox, w, TEST_MIN_STDDEV));
    }

    #[test]
    fn check_ir_texture_threshold_is_a_configurable_cutoff() {
        // A region with std_dev ~8 sits below the default 10.0 cutoff (flat photo
        // territory) but a caller can lower the cutoff to accept it.
        let w = 64u32;
        let h = 64u32;
        let mut gray = vec![100u8; (w * h) as usize];
        // Alternate two values 8 apart => std_dev ~= 8.
        for row in 10..30 {
            for col in 10..30 {
                gray[row * w as usize + col] = if (row + col) % 2 == 0 { 92 } else { 108 };
            }
        }
        let bbox = BoundingBox {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        // std_dev is 8, so it fails at the default cutoff (photo-like)...
        assert!(!check_ir_texture(&gray, &bbox, w, 10.0));
        // ...but passes if the operator lowers the cutoff below the measured value.
        assert!(check_ir_texture(&gray, &bbox, w, 5.0));
    }
}
