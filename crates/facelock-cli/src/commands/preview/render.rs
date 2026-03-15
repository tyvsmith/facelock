// Rendering primitives for the preview overlay.

use super::font;

/// Info bar height in pixels (font height + padding).
pub const INFO_BAR_H: u32 = font::CHAR_H + 4;

/// Colors used for rendering overlays.
pub const COLOR_GREEN: [u8; 3] = [0, 255, 0];
pub const COLOR_RED: [u8; 3] = [255, 0, 0];
pub const COLOR_WHITE: [u8; 3] = [255, 255, 255];
#[cfg(test)]
pub const COLOR_BLACK: [u8; 3] = [0, 0, 0];

/// Convert RGB frame data to XRGB8888 (little-endian: BGRX bytes).
///
/// `rgb` is width*height*3 bytes (R, G, B per pixel).
/// Output is width*height*4 bytes (B, G, R, 0xFF per pixel).
#[cfg(test)]
pub fn rgb_to_xrgb(rgb: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let pixel_count = (width * height) as usize;
    let expected_rgb = pixel_count * 3;
    let expected_xrgb = pixel_count * 4;

    if rgb.len() < expected_rgb || out.len() < expected_xrgb {
        return;
    }

    for i in 0..pixel_count {
        let si = i * 3;
        let di = i * 4;
        out[di] = rgb[si + 2]; // B
        out[di + 1] = rgb[si + 1]; // G
        out[di + 2] = rgb[si]; // R
        out[di + 3] = 0xFF; // X
    }
}

/// Draw a rectangle outline with the given color and thickness.
#[allow(clippy::too_many_arguments)]
pub fn draw_rect(
    buf: &mut [u8],
    stride: u32,
    buf_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [u8; 3],
    thickness: u32,
) {
    // Top and bottom edges
    for t in 0..thickness {
        draw_hline(buf, stride, buf_h, x, y + t, w, color);
        if y + h > t {
            draw_hline(buf, stride, buf_h, x, y + h - 1 - t, w, color);
        }
    }
    // Left and right edges
    for t in 0..thickness {
        draw_vline(buf, stride, buf_h, x + t, y, h, color);
        if x + w > t {
            draw_vline(buf, stride, buf_h, x + w - 1 - t, y, h, color);
        }
    }
}

/// Draw a horizontal line.
fn draw_hline(buf: &mut [u8], stride: u32, buf_h: u32, x: u32, y: u32, w: u32, color: [u8; 3]) {
    if y >= buf_h {
        return;
    }
    let max_w = stride / 4;
    for px in x..x.saturating_add(w).min(max_w) {
        set_pixel(buf, stride, buf_h, px, y, color);
    }
}

/// Draw a vertical line.
fn draw_vline(buf: &mut [u8], stride: u32, buf_h: u32, x: u32, y: u32, h: u32, color: [u8; 3]) {
    let max_w = stride / 4;
    if x >= max_w {
        return;
    }
    for py in y..y.saturating_add(h).min(buf_h) {
        set_pixel(buf, stride, buf_h, x, py, color);
    }
}

/// Set a single pixel in the XRGB8888 buffer.
fn set_pixel(buf: &mut [u8], stride: u32, buf_h: u32, x: u32, y: u32, color: [u8; 3]) {
    if y >= buf_h || x >= stride / 4 {
        return;
    }
    let offset = (y * stride + x * 4) as usize;
    if offset + 3 < buf.len() {
        buf[offset] = color[2]; // B
        buf[offset + 1] = color[1]; // G
        buf[offset + 2] = color[0]; // R
        buf[offset + 3] = 0xFF;
    }
}

/// Fill a rectangular area with a solid color.
#[allow(clippy::too_many_arguments)]
pub fn fill_rect(
    buf: &mut [u8],
    stride: u32,
    buf_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [u8; 3],
) {
    let max_w = stride / 4;
    let x_end = x.saturating_add(w).min(max_w);
    let y_end = y.saturating_add(h).min(buf_h);
    let pixel = [color[2], color[1], color[0], 0xFF]; // BGRX

    for py in y..y_end {
        let row_start = (py * stride) as usize;
        for px in x..x_end {
            let offset = row_start + (px * 4) as usize;
            if offset + 3 < buf.len() {
                buf[offset..offset + 4].copy_from_slice(&pixel);
            }
        }
    }
}

/// Draw the info bar at the bottom of the frame.
pub fn draw_info_bar(
    buf: &mut [u8],
    stride: u32,
    width: u32,
    height: u32,
    fps: f32,
    recognized: u32,
    unrecognized: u32,
) {
    let bar_y = height.saturating_sub(INFO_BAR_H);

    // Semi-dark background
    fill_rect(
        buf,
        stride,
        height,
        0,
        bar_y,
        width,
        INFO_BAR_H,
        [20, 20, 20],
    );

    let frame_h = height.saturating_sub(INFO_BAR_H);
    let face_info = match (recognized, unrecognized) {
        (0, 0) => String::new(),
        (r, 0) => format!(" | {r} recognized"),
        (0, u) => format!(" | {u} unrecognized"),
        (r, u) => format!(" | {r} recognized, {u} unrecognized"),
    };
    let info = format!(" {width}x{frame_h} | {fps:.0} fps{face_info}");
    font::draw_text(buf, stride, 2, bar_y + 2, &info, COLOR_WHITE);
}

/// Draw a bounding box with a confidence label.
#[allow(clippy::too_many_arguments)]
pub fn draw_detection_box(
    buf: &mut [u8],
    stride: u32,
    buf_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    confidence: f32,
    matched: bool,
) {
    let color = if matched { COLOR_GREEN } else { COLOR_RED };
    draw_rect(buf, stride, buf_h, x, y, w, h, color, 2);

    // Draw confidence label above the box
    let label = format!("{:.0}%", confidence * 100.0);
    let label_y = if y >= font::CHAR_H + 2 {
        y - font::CHAR_H - 2
    } else {
        y + h + 2
    };
    font::draw_text(buf, stride, x, label_y, &label, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_to_xrgb_converts_correctly() {
        let rgb = [255u8, 0, 0, 0, 255, 0, 0, 0, 255];
        let mut out = vec![0u8; 12];
        rgb_to_xrgb(&rgb, 3, 1, &mut out);

        // First pixel: R=255 -> [B=0, G=0, R=255, X=0xFF]
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 255);
        assert_eq!(out[3], 0xFF);

        // Second pixel: G=255 -> [B=0, G=255, R=0, X=0xFF]
        assert_eq!(out[4], 0);
        assert_eq!(out[5], 255);
        assert_eq!(out[6], 0);
        assert_eq!(out[7], 0xFF);

        // Third pixel: B=255 -> [B=255, G=0, R=0, X=0xFF]
        assert_eq!(out[8], 255);
        assert_eq!(out[9], 0);
        assert_eq!(out[10], 0);
        assert_eq!(out[11], 0xFF);
    }

    #[test]
    fn draw_rect_does_not_panic() {
        let w = 32u32;
        let h = 32u32;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        draw_rect(&mut buf, stride, h, 2, 2, 20, 15, COLOR_GREEN, 2);
    }

    #[test]
    fn draw_rect_at_edge_does_not_panic() {
        let w = 32u32;
        let h = 32u32;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        draw_rect(&mut buf, stride, h, 28, 28, 20, 15, COLOR_RED, 2);
    }

    #[test]
    fn draw_info_bar_does_not_panic() {
        let w = 64u32;
        let h = 32u32;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        draw_info_bar(&mut buf, stride, w, h, 29.7, 1, 1);
    }

    #[test]
    fn fill_rect_does_not_panic() {
        let w = 16u32;
        let h = 16u32;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        fill_rect(&mut buf, stride, h, 0, 0, w, h, COLOR_BLACK);
    }

    #[test]
    fn draw_detection_box_does_not_panic() {
        let w = 64u32;
        let h = 64u32;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        draw_detection_box(&mut buf, stride, h, 10, 15, 30, 25, 0.95, true);
        draw_detection_box(&mut buf, stride, h, 5, 5, 20, 20, 0.42, false);
    }

    #[test]
    fn rgb_to_xrgb_short_buffers_safe() {
        let rgb = [255u8, 0, 0];
        let mut out = vec![0u8; 2]; // too short
        rgb_to_xrgb(&rgb, 1, 1, &mut out);
        // Should not panic, just return early
        assert_eq!(out, [0, 0]);
    }
}
