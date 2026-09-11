//! A procedurally rendered face for the loopback camera tier.
//!
//! No photograph, scan, or generative-model output is involved: every pixel
//! comes from the arithmetic in this file, so the identity it replays belongs
//! to nobody (`test/loopback/NOTICE.md`). The output is what a near-infrared
//! sensor would see — a single 8-bit channel, dark background, shaded skin
//! with fine texture — so a v4l2loopback node fed with it enumerates `GREY`
//! only and classifies as IR by format evidence, the documented residual in
//! `docs/security.md` §A.
//!
//! The sequence is the fixture. A single still would fail two live gates on
//! purpose: enrollment wants angle diversity (some pair of accepted
//! embeddings below 0.95) and authentication's frame-variance gate wants
//! every consecutive matched pair at or below
//! `security.frame_variance_max_similarity`. So each frame is rendered from a
//! [`Pose`] that drifts along a slow sweep — translation, scale, yaw — the
//! way a person sitting at a login prompt drifts. There is no per-frame
//! sensor noise on purpose: the drift the gates see is the motion and
//! nothing else, so a paused replay of any one frame is byte-identical and
//! reads as static.
//! The bands those gates need are pinned against the real models by
//! `crates/facelock-daemon/tests/synthetic_face_contract.rs`.

/// Frame width the tier negotiates (`Camera::open` caps at 640x480).
pub const WIDTH: u32 = 640;
/// Frame height the tier negotiates.
pub const HEIGHT: u32 = 480;
/// Frames in one loop of the fixture.
pub const FRAMES: u32 = 24;

/// Where the head sits in a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// Horizontal head offset from frame centre, pixels.
    pub dx: f32,
    /// Vertical head offset from frame centre, pixels.
    pub dy: f32,
    /// Head scale relative to the nominal 100x130 px half-axes.
    pub scale: f32,
    /// Horizontal head turn, -1 (left) .. 1 (right). Moves the features
    /// across the head the way a turn does in projection.
    pub yaw: f32,
}

impl Pose {
    /// The pose of frame `index` in a loop of `total` frames: one slow
    /// sweep of yaw and position per loop, with a small faster wobble so
    /// consecutive frames never repeat.
    pub fn for_frame(index: u32, total: u32) -> Self {
        let total = total.max(1) as f32;
        let t = index as f32 / total;
        let theta = t * std::f32::consts::TAU;
        Self {
            dx: 18.0 * theta.sin() + 4.0 * (3.0 * theta).cos(),
            dy: 10.0 * theta.cos() + 3.0 * (5.0 * theta).sin(),
            scale: 1.0 + 0.04 * (theta + 1.0).sin(),
            yaw: 0.7 * theta.sin() + 0.15 * (2.0 * theta + 0.7).cos(),
        }
    }
}

/// Deterministic white noise in [-1, 1] for a pixel and seed (splitmix-style
/// hash, so a frame renders identically on every host). Seeds are fixed, so
/// the grain is part of the picture, not something that changes per frame.
fn noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = seed ^ x.wrapping_mul(0x85EB_CA6B) ^ y.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    (h as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Smooth, low-frequency noise for skin texture (bilinear hash lattice).
fn smooth_noise(x: f32, y: f32, seed: u32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (x0, y0) = (x0 as i32 as u32, y0 as i32 as u32);
    let n00 = noise(x0, y0, seed);
    let n10 = noise(x0.wrapping_add(1), y0, seed);
    let n01 = noise(x0, y0.wrapping_add(1), seed);
    let n11 = noise(x0.wrapping_add(1), y0.wrapping_add(1), seed);
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let a = n00 + (n10 - n00) * sx;
    let b = n01 + (n11 - n01) * sx;
    a + (b - a) * sy
}

fn inside_ellipse(u: f32, v: f32, cu: f32, cv: f32, ru: f32, rv: f32) -> bool {
    let du = (u - cu) / ru;
    let dv = (v - cv) / rv;
    du * du + dv * dv <= 1.0
}

/// Render one 8-bit mono frame of `WIDTH` x `HEIGHT` pixels.
pub fn render(pose: &Pose) -> Vec<u8> {
    render_sized(WIDTH, HEIGHT, pose)
}

/// Render one 8-bit mono frame at an arbitrary size. The head is scaled
/// with the frame so the face keeps its share of the picture.
pub fn render_sized(width: u32, height: u32, pose: &Pose) -> Vec<u8> {
    let mut out = vec![0u8; (width * height) as usize];
    let cx = width as f32 / 2.0 + pose.dx;
    let cy = height as f32 / 2.0 + pose.dy;
    let unit = height as f32 / 480.0;
    let rx = 100.0 * unit * pose.scale;
    let ry = 130.0 * unit * pose.scale;
    // A turned head moves its features toward the side it faces.
    let shift = 0.22 * pose.yaw;
    let squeeze = 1.0 - 0.18 * pose.yaw.abs();

    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let u = (px - cx) / rx;
            let v = (py - cy) / ry;
            // Feature coordinates: the same space, turned with the head.
            let fu = (u - shift) / squeeze;

            // Background: an unlit room with a faint wall gradient.
            let mut value = 26.0 + 10.0 * (py / height as f32);

            let in_head = u * u + v * v <= 1.0;
            if !in_head {
                // Neck and shoulders below the jaw.
                if v > 0.0 && u.abs() < 0.42 {
                    value = 118.0 - 30.0 * (v - 1.0).max(0.0);
                } else if v > 0.95 && u.abs() < 1.9 {
                    value = 78.0 - 12.0 * (v - 0.95);
                }
                value += 2.0 * noise(x, y, 0xBA5E);
                out[(y * width + x) as usize] = value.clamp(0.0, 255.0) as u8;
                continue;
            }

            // Skin: an emitter-lit sphere, brighter where it faces the lens.
            let r2 = u * u + v * v;
            let mut skin = 168.0 - 48.0 * r2 - 14.0 * v - 8.0 * u;
            // Pores and micro-texture: what the IR texture gate looks for.
            skin += 7.0 * smooth_noise(px / 3.0, py / 3.0, 0x5C1D) + 4.0 * noise(x, y, 0x7E57);
            value = skin;

            // Hair above the forehead, with a slightly ragged hairline.
            let hairline = -0.58 + 0.03 * smooth_noise(px / 9.0, 0.0, 0x4A1F);
            if v < hairline {
                value = 44.0 + 12.0 * smooth_noise(px / 2.0, py / 6.0, 0x4A1F);
            }

            // Brows: dark arcs above each eye.
            for side in [-1.0f32, 1.0] {
                let ecu = side * 0.40;
                let d = (fu - ecu) / 0.21;
                if d.abs() <= 1.0 {
                    let arc = -0.34 - 0.05 * (1.0 - d * d);
                    if (v - arc).abs() <= 0.035 {
                        value = 52.0 + 8.0 * noise(x, y, 0xB20);
                    }
                }
            }

            // Eyes: sclera, iris, pupil, a lid shadow along the upper edge.
            for side in [-1.0f32, 1.0] {
                let ecu = side * 0.40;
                let ecv = -0.20;
                if inside_ellipse(fu, v, ecu, ecv, 0.15, 0.085) {
                    value = 198.0 - 20.0 * ((v - ecv) / 0.085).abs();
                    if inside_ellipse(fu, v, ecu, ecv, 0.072, 0.072 * (rx / ry)) {
                        value = 74.0;
                    }
                    if inside_ellipse(fu, v, ecu, ecv, 0.034, 0.034 * (rx / ry)) {
                        value = 16.0;
                    }
                    // Corneal glint from the emitter.
                    if inside_ellipse(fu, v, ecu - 0.02, ecv - 0.025, 0.014, 0.014 * (rx / ry)) {
                        value = 235.0;
                    }
                }
                if inside_ellipse(fu, v, ecu, ecv, 0.17, 0.105)
                    && !inside_ellipse(fu, v, ecu, ecv, 0.15, 0.085)
                    && v < ecv
                {
                    value = 88.0;
                }
            }

            // Nose: a lit ridge, a shadowed side, nostrils.
            if (-0.18..=0.28).contains(&v) {
                let ridge = (fu - 0.35 * shift).abs();
                if ridge < 0.055 {
                    value += 16.0;
                } else if ridge < 0.15 && fu > 0.0 {
                    value -= 22.0 * (1.0 - (ridge - 0.055) / 0.095);
                }
            }
            if inside_ellipse(fu, v, 0.0, 0.25, 0.09, 0.06) {
                value += 12.0;
            }
            for side in [-1.0f32, 1.0] {
                if inside_ellipse(fu, v, side * 0.085, 0.31, 0.04, 0.026) {
                    value = 58.0;
                }
            }

            // Mouth: two lips and the dark seam between them.
            let seam = 0.56 + 0.02 * fu * fu;
            if fu.abs() <= 0.30 {
                let lip_half = 0.045 * (1.0 - (fu / 0.30).powi(2)).max(0.0);
                if (v - seam).abs() <= lip_half {
                    value = 118.0 - 10.0 * ((v - seam) / lip_half.max(1e-3));
                }
                if (v - seam).abs() <= 0.011 {
                    value = 40.0;
                }
            }

            // Chin and jaw fall away from the emitter.
            if v > 0.75 {
                value -= 34.0 * (v - 0.75) / 0.25;
            }

            // Fixed-pattern sensor grain, the same on every frame.
            value += 2.0 * noise(x, y, 0xBA5E);
            out[(y * width + x) as usize] = value.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Render the whole loop: `FRAMES` frames of `WIDTH` x `HEIGHT`.
pub fn render_sequence() -> Vec<Vec<u8>> {
    (0..FRAMES)
        .map(|i| render(&Pose::for_frame(i, FRAMES)))
        .collect()
}

/// Pack a mono frame as YUYV 4:2:2 with neutral chroma, for the RGB twin.
/// Two pixels become `Y0 U Y1 V`; U and V at 128 keep the picture grey once
/// a colour pipeline decodes it, which is exactly what an ordinary webcam
/// shows in an unlit room.
pub fn mono_to_yuyv(gray: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(gray.len() * 2);
    for pair in gray.chunks(2) {
        let y0 = pair[0];
        let y1 = pair.get(1).copied().unwrap_or(y0);
        out.extend_from_slice(&[y0, 128, y1, 128]);
    }
    out
}

/// Encode a mono frame as a binary PGM (P5), for eyeballing a frame.
pub fn to_pgm(width: u32, height: u32, gray: &[u8]) -> Vec<u8> {
    let mut out = format!("P5\n{width} {height}\n255\n").into_bytes();
    out.extend_from_slice(gray);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stddev(px: &[u8]) -> f32 {
        let n = px.len() as f32;
        let mean = px.iter().map(|&p| p as f32).sum::<f32>() / n;
        (px.iter().map(|&p| (p as f32 - mean).powi(2)).sum::<f32>() / n).sqrt()
    }

    #[test]
    fn frame_has_the_negotiated_geometry() {
        let frame = render(&Pose::for_frame(0, FRAMES));
        assert_eq!(frame.len(), (WIDTH * HEIGHT) as usize);
    }

    #[test]
    fn rendering_is_deterministic() {
        let a = render(&Pose::for_frame(5, FRAMES));
        let b = render(&Pose::for_frame(5, FRAMES));
        assert_eq!(a, b);
    }

    #[test]
    fn consecutive_frames_differ() {
        let a = render(&Pose::for_frame(0, FRAMES));
        let b = render(&Pose::for_frame(1, FRAMES));
        assert_ne!(a, b);
    }

    #[test]
    fn the_same_pose_renders_the_same_bytes() {
        // No per-frame noise: a paused replay is byte-identical, so the
        // variance gate has nothing but motion to pass on.
        let pose = Pose::for_frame(3, FRAMES);
        assert_eq!(render(&pose), render(&pose));
    }

    #[test]
    fn poses_sweep_and_return() {
        // A loop must close: the last frame's pose is one step from the
        // first, not a jump that a variance gate would read as a cut.
        let first = Pose::for_frame(0, FRAMES);
        let last = Pose::for_frame(FRAMES - 1, FRAMES);
        assert!((first.dx - last.dx).abs() < 8.0, "{first:?} vs {last:?}");
        assert!((first.yaw - last.yaw).abs() < 0.25, "{first:?} vs {last:?}");
        let mid = Pose::for_frame(FRAMES / 4, FRAMES);
        assert!(mid.yaw.abs() > 0.3, "the sweep must actually turn the head");
    }

    #[test]
    fn face_region_has_ir_texture_above_the_default_gate() {
        // security.ir_texture_min_stddev defaults to 10.0 and is measured on
        // the raw frame inside the detector's box; the head region is a
        // superset of any box SCRFD will draw around it.
        let frame = render(&Pose::for_frame(0, FRAMES));
        let mut face = Vec::new();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let u = (x as f32 - 320.0) / 100.0;
                let v = (y as f32 - 240.0) / 130.0;
                if u * u + v * v <= 0.8 {
                    face.push(frame[(y * WIDTH + x) as usize]);
                }
            }
        }
        assert!(stddev(&face) > 10.0, "face stddev {}", stddev(&face));
    }

    #[test]
    fn frame_is_not_dark() {
        // device.dark_threshold: a frame is dark when most pixels sit under
        // dark_pixel_value (default 30). The lit face and the wall gradient
        // keep the mean well above it.
        let frame = render(&Pose::for_frame(0, FRAMES));
        let mean = frame.iter().map(|&p| p as u32).sum::<u32>() / frame.len() as u32;
        assert!(mean > 40, "mean {mean}");
    }

    #[test]
    fn yuyv_twin_carries_luma_with_neutral_chroma() {
        let out = mono_to_yuyv(&[10, 20, 30]);
        assert_eq!(out, vec![10, 128, 20, 128, 30, 128, 30, 128]);
    }

    #[test]
    fn pgm_header_matches_payload() {
        let pgm = to_pgm(2, 1, &[1, 2]);
        assert_eq!(&pgm[..], b"P5\n2 1\n255\n\x01\x02");
    }
}
