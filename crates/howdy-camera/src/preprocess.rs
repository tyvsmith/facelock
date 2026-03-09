use howdy_core::types::BoundingBox;

/// Convert YUYV (YUV 4:2:2) packed data to RGB.
/// Each 4-byte YUYV group produces 2 RGB pixels.
pub fn yuyv_to_rgb(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgb = Vec::with_capacity(pixel_count * 3);

    // Process 4 bytes at a time (2 pixels)
    for chunk in data.chunks_exact(4) {
        let y0 = chunk[0] as f32;
        let u = chunk[1] as f32 - 128.0;
        let y1 = chunk[2] as f32;
        let v = chunk[3] as f32 - 128.0;

        for y in [y0, y1] {
            let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
            let g = (y - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0) as u8;
            let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }

    rgb
}

/// Convert RGB image to grayscale using luminance formula.
pub fn rgb_to_gray(rgb: &[u8], _width: u32, _height: u32) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|px| {
            let r = px[0] as f32;
            let g = px[1] as f32;
            let b = px[2] as f32;
            (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8
        })
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
pub fn check_ir_texture(gray: &[u8], bbox: &BoundingBox, width: u32) -> bool {
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
    std_dev > 10.0
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
        assert!(!check_ir_texture(&gray, &bbox, w));
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
        assert!(check_ir_texture(&gray, &bbox, w));
    }
}
