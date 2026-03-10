use visage_core::error::{VisageError, Result};
use visage_core::types::{Frame, Point2D};

/// Standard 112x112 reference landmarks (InsightFace canonical positions)
const REFERENCE_LANDMARKS: [[f32; 2]; 5] = [
    [38.2946, 51.6963], // Left eye
    [73.5318, 51.5014], // Right eye
    [56.0252, 71.7366], // Nose tip
    [41.5493, 92.3655], // Left mouth corner
    [70.7299, 92.2041], // Right mouth corner
];

const ALIGNED_SIZE: u32 = 112;

/// An aligned face image (112x112 RGB).
pub struct AlignedFace {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Compute a 2x3 affine transformation matrix from source landmarks to reference landmarks
/// using Umeyama's method with analytical 2x2 SVD.
pub fn compute_affine_matrix(src: &[Point2D; 5]) -> [[f32; 3]; 2] {
    let n = 5.0f32;

    // Compute centroids
    let (mut src_cx, mut src_cy) = (0.0f32, 0.0f32);
    let (mut dst_cx, mut dst_cy) = (0.0f32, 0.0f32);
    for (s, d) in src.iter().zip(REFERENCE_LANDMARKS.iter()) {
        src_cx += s.x;
        src_cy += s.y;
        dst_cx += d[0];
        dst_cy += d[1];
    }
    src_cx /= n;
    src_cy /= n;
    dst_cx /= n;
    dst_cy /= n;

    // Compute variance of source points
    let mut src_var = 0.0f32;
    for s in src {
        let dx = s.x - src_cx;
        let dy = s.y - src_cy;
        src_var += dx * dx + dy * dy;
    }
    src_var /= n;

    // Compute covariance matrix H = src_centered^T * dst_centered / N
    // H is 2x2: [[h00, h01], [h10, h11]]
    let (mut h00, mut h01, mut h10, mut h11) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for (s, d) in src.iter().zip(REFERENCE_LANDMARKS.iter()) {
        let sx = s.x - src_cx;
        let sy = s.y - src_cy;
        let dx = d[0] - dst_cx;
        let dy = d[1] - dst_cy;
        h00 += sx * dx;
        h01 += sx * dy;
        h10 += sy * dx;
        h11 += sy * dy;
    }
    h00 /= n;
    h01 /= n;
    h10 /= n;
    h11 /= n;

    // Analytical SVD of 2x2 matrix H
    // H = U * S * V^T
    let (u, s, v) = svd_2x2(h00, h01, h10, h11);

    // Determine D to ensure proper rotation (not reflection)
    let det_uv = (u[0][0] * u[1][1] - u[0][1] * u[1][0])
        * (v[0][0] * v[1][1] - v[0][1] * v[1][0]);
    let d = if det_uv < 0.0 {
        [1.0f32, -1.0]
    } else {
        [1.0f32, 1.0]
    };

    // R = V * D * U^T
    // First compute V * D
    let vd = [
        [v[0][0] * d[0], v[0][1] * d[1]],
        [v[1][0] * d[0], v[1][1] * d[1]],
    ];
    // Then (V*D) * U^T
    let r = [
        [
            vd[0][0] * u[0][0] + vd[0][1] * u[0][1],
            vd[0][0] * u[1][0] + vd[0][1] * u[1][1],
        ],
        [
            vd[1][0] * u[0][0] + vd[1][1] * u[0][1],
            vd[1][0] * u[1][0] + vd[1][1] * u[1][1],
        ],
    ];

    // Scale = trace(S * D) / var(src)
    let scale = if src_var.abs() < 1e-10 {
        1.0
    } else {
        (s[0] * d[0] + s[1] * d[1]) / src_var
    };

    // Translation: t = dst_mean - scale * R * src_mean
    let tx = dst_cx - scale * (r[0][0] * src_cx + r[0][1] * src_cy);
    let ty = dst_cy - scale * (r[1][0] * src_cx + r[1][1] * src_cy);

    [
        [scale * r[0][0], scale * r[0][1], tx],
        [scale * r[1][0], scale * r[1][1], ty],
    ]
}

/// Analytical SVD of a 2x2 matrix [[a, b], [c, d]].
/// Returns (U, singular_values, V) where H = U * diag(S) * V^T.
fn svd_2x2(a: f32, b: f32, c: f32, d: f32) -> ([[f32; 2]; 2], [f32; 2], [[f32; 2]; 2]) {
    // H^T * H
    let ata00 = a * a + c * c;
    let ata01 = a * b + c * d;
    let ata11 = b * b + d * d;

    // Eigenvalues of H^T * H (symmetric 2x2)
    let trace = ata00 + ata11;
    let det = ata00 * ata11 - ata01 * ata01;
    let disc = ((trace * trace / 4.0 - det).max(0.0)).sqrt();
    let lambda1 = trace / 2.0 + disc;
    let lambda2 = (trace / 2.0 - disc).max(0.0);

    let s1 = lambda1.sqrt();
    let s2 = lambda2.sqrt();

    // Eigenvectors of H^T * H for V
    let v = eigenvectors_2x2_sym(ata00, ata01, ata11, lambda1, lambda2);

    // U = H * V * S^{-1}
    let mut u = [[0.0f32; 2]; 2];
    if s1 > 1e-10 {
        u[0][0] = (a * v[0][0] + b * v[1][0]) / s1;
        u[1][0] = (c * v[0][0] + d * v[1][0]) / s1;
    }
    if s2 > 1e-10 {
        u[0][1] = (a * v[0][1] + b * v[1][1]) / s2;
        u[1][1] = (c * v[0][1] + d * v[1][1]) / s2;
    } else {
        // Second singular value is ~0, set u2 orthogonal to u1
        u[0][1] = -u[1][0];
        u[1][1] = u[0][0];
    }

    (u, [s1, s2], v)
}

/// Compute eigenvectors of a symmetric 2x2 matrix.
fn eigenvectors_2x2_sym(
    a: f32,
    b: f32,
    _c: f32,
    lambda1: f32,
    lambda2: f32,
) -> [[f32; 2]; 2] {
    let mut v = [[1.0f32, 0.0], [0.0, 1.0]];

    // First eigenvector for lambda1
    if b.abs() > 1e-10 {
        let vx = b;
        let vy = lambda1 - a;
        let norm = (vx * vx + vy * vy).sqrt();
        if norm > 1e-10 {
            v[0][0] = vx / norm;
            v[1][0] = vy / norm;
        }
    } else if (a - lambda1).abs() > (a - lambda2).abs() {
        // lambda1 corresponds to index 1
        v[0][0] = 0.0;
        v[1][0] = 1.0;
    }

    // Second eigenvector: orthogonal to first
    v[0][1] = -v[1][0];
    v[1][1] = v[0][0];

    v
}

/// Align a face by warping from the source frame using the inverse affine transform.
pub fn align_face(frame: &Frame, landmarks: &[Point2D; 5]) -> Result<AlignedFace> {
    let m = compute_affine_matrix(landmarks);

    // Compute inverse of the 2x3 affine matrix
    // [a b tx]    ->   inv = [a b tx]^{-1}
    // [c d ty]
    let a = m[0][0];
    let b = m[0][1];
    let tx = m[0][2];
    let c = m[1][0];
    let d = m[1][1];
    let ty = m[1][2];

    let det = a * d - b * c;
    if det.abs() < 1e-10 {
        return Err(VisageError::Alignment(
            "Singular affine matrix".to_string(),
        ));
    }

    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ib = -b * inv_det;
    let ic = -c * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ib * ty);
    let ity = -(ic * tx + id * ty);

    let size = ALIGNED_SIZE as usize;
    let mut rgb = vec![0u8; size * size * 3];

    for out_y in 0..size {
        for out_x in 0..size {
            // Map output pixel to source coordinates
            let src_x = ia * out_x as f32 + ib * out_y as f32 + itx;
            let src_y = ic * out_x as f32 + id * out_y as f32 + ity;

            // Bilinear interpolation
            let x0 = src_x.floor() as i32;
            let y0 = src_y.floor() as i32;
            let x1 = x0 + 1;
            let y1 = y0 + 1;

            let fx = src_x - x0 as f32;
            let fy = src_y - y0 as f32;

            let w = frame.width as i32;
            let h = frame.height as i32;

            for c in 0..3 {
                let get_pixel = |px: i32, py: i32| -> f32 {
                    if px >= 0 && px < w && py >= 0 && py < h {
                        frame.rgb[(py as usize * frame.width as usize + px as usize) * 3 + c]
                            as f32
                    } else {
                        0.0 // black padding
                    }
                };

                let val = get_pixel(x0, y0) * (1.0 - fx) * (1.0 - fy)
                    + get_pixel(x1, y0) * fx * (1.0 - fy)
                    + get_pixel(x0, y1) * (1.0 - fx) * fy
                    + get_pixel(x1, y1) * fx * fy;

                rgb[(out_y * size + out_x) * 3 + c] = val.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    Ok(AlignedFace {
        rgb,
        width: ALIGNED_SIZE,
        height: ALIGNED_SIZE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_matrix_identity_when_src_equals_reference() {
        let src: [Point2D; 5] = [
            Point2D { x: 38.2946, y: 51.6963 },
            Point2D { x: 73.5318, y: 51.5014 },
            Point2D { x: 56.0252, y: 71.7366 },
            Point2D { x: 41.5493, y: 92.3655 },
            Point2D { x: 70.7299, y: 92.2041 },
        ];

        let m = compute_affine_matrix(&src);

        // Should be near identity: [[1, 0, 0], [0, 1, 0]]
        assert!(
            (m[0][0] - 1.0).abs() < 1e-3,
            "m[0][0] should be ~1.0, got {}",
            m[0][0]
        );
        assert!(
            m[0][1].abs() < 1e-3,
            "m[0][1] should be ~0.0, got {}",
            m[0][1]
        );
        assert!(
            m[0][2].abs() < 1e-3,
            "m[0][2] should be ~0.0, got {}",
            m[0][2]
        );
        assert!(
            m[1][0].abs() < 1e-3,
            "m[1][0] should be ~0.0, got {}",
            m[1][0]
        );
        assert!(
            (m[1][1] - 1.0).abs() < 1e-3,
            "m[1][1] should be ~1.0, got {}",
            m[1][1]
        );
        assert!(
            m[1][2].abs() < 1e-3,
            "m[1][2] should be ~0.0, got {}",
            m[1][2]
        );
    }
}
