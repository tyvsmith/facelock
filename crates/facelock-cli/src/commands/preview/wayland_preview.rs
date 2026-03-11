use std::time::Instant;

use anyhow::Context;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use facelock_core::ipc::{DaemonRequest, DaemonResponse, PreviewFace};

use super::render;
use crate::ipc_client;

/// Maximum preview dimensions.
const MAX_WIDTH: u32 = 640;
const MAX_HEIGHT: u32 = 480;

/// Run the Wayland layer-shell preview.
pub fn run(socket_path: &str, user: &str) -> anyhow::Result<()> {
    // Catch Ctrl+C so we can clean up the camera
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, std::sync::Arc::clone(&stop))
        .context("failed to register SIGINT handler")?;

    let conn = Connection::connect_to_env().context("failed to connect to Wayland display")?;

    let (globals, mut event_queue) =
        registry_queue_init(&conn).context("failed to initialize Wayland registry")?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).context("wl_compositor not available")?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).context("zwlr_layer_shell_v1 not available")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("facelock-preview"), None);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.set_size(MAX_WIDTH, MAX_HEIGHT);
    layer.commit();

    let pool = SlotPool::new(
        (MAX_WIDTH * MAX_HEIGHT * 4) as usize,
        &shm,
    )
    .context("failed to create SHM pool")?;

    let mut state = PreviewState {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        width: MAX_WIDTH,
        height: MAX_HEIGHT,
        exit: false,
        first_configure: true,
        keyboard: None,
        keyboard_focus: false,
        pointer: None,
        socket_path: socket_path.to_string(),
        user: user.to_string(),
        fps: 0.0,
        frame_count: 0,
        fps_frame_count: 0,
        last_fps_time: Instant::now(),
    };

    eprintln!("Preview window opened. Press Escape or 'q' to close.");

    loop {
        event_queue
            .blocking_dispatch(&mut state)
            .context("Wayland dispatch error")?;

        if state.exit || stop.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("preview closed");
            let _ = ipc_client::send_request(socket_path, &DaemonRequest::ReleaseCamera);
            break;
        }
    }

    Ok(())
}

struct PreviewState {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    width: u32,
    height: u32,
    exit: bool,
    first_configure: bool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_focus: bool,
    pointer: Option<wl_pointer::WlPointer>,
    socket_path: String,
    user: String,
    fps: f32,
    frame_count: u64,
    fps_frame_count: u64,
    last_fps_time: Instant,
}

impl PreviewState {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let width = self.width;
        let height = self.height;
        let stride = width as i32 * 4;
        let socket_path = self.socket_path.clone();
        let user = self.user.clone();

        // Request a frame with detection from the daemon
        let frame_result = ipc_client::send_request(
            &socket_path,
            &DaemonRequest::PreviewDetectFrame { user },
        );

        // Update FPS tracking
        let fps = match &frame_result {
            Ok(DaemonResponse::DetectFrame { .. } | DaemonResponse::Frame { .. }) => {
                self.frame_count += 1;
                self.fps_frame_count += 1;
                let now = Instant::now();
                let elapsed = now.duration_since(self.last_fps_time).as_secs_f32();
                if elapsed >= 1.0 {
                    self.fps = self.fps_frame_count as f32 / elapsed;
                    self.fps_frame_count = 0;
                    self.last_fps_time = now;
                }
                self.fps
            }
            _ => self.fps,
        };

        // Now borrow the pool to create a buffer
        let (buffer, canvas) = match self
            .pool
            .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Xrgb8888)
        {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("failed to create SHM buffer: {e}");
                return;
            }
        };

        // Render into the canvas
        match frame_result {
            Ok(DaemonResponse::DetectFrame { jpeg_data, faces }) => {
                render_frame(&jpeg_data, canvas, width, height, fps, &faces);
            }
            Ok(DaemonResponse::Frame { jpeg_data }) => {
                render_frame(&jpeg_data, canvas, width, height, fps, &[]);
            }
            Ok(DaemonResponse::Error { message }) => {
                tracing::warn!("daemon error: {message}");
                render_error(canvas, width, height, &message);
            }
            Ok(_) => {
                tracing::warn!("unexpected response from daemon");
                render_error(canvas, width, height, "unexpected daemon response");
            }
            Err(e) => {
                tracing::warn!("IPC error: {e}");
                render_error(canvas, width, height, &format!("IPC error: {e}"));
            }
        }

        // Damage, request next frame, attach, commit
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());
        if let Err(e) = buffer.attach_to(self.layer.wl_surface()) {
            tracing::error!("buffer attach failed: {e}");
            return;
        }
        self.layer.commit();
    }
}

/// Render a decoded JPEG frame into the XRGB8888 canvas.
fn render_frame(
    jpeg_data: &[u8],
    canvas: &mut [u8],
    width: u32,
    height: u32,
    fps: f32,
    faces: &[PreviewFace],
) {
    let stride = width * 4;

    match decode_jpeg(jpeg_data) {
        Ok((rgb, img_w, img_h)) => {
            let (disp_w, disp_h) = fit_dimensions(img_w, img_h, width, height);
            let offset_x = (width.saturating_sub(disp_w)) / 2;
            let offset_y = (height.saturating_sub(disp_h)) / 2;

            // Clear canvas to black
            for chunk in canvas.chunks_exact_mut(4) {
                chunk.copy_from_slice(&[0, 0, 0, 0xFF]);
            }

            // Pre-compute source X lookup table to avoid per-pixel division
            let src_x_table: Vec<u32> = (0..disp_w)
                .map(|dx| (dx as u64 * img_w as u64 / disp_w as u64) as u32)
                .collect();

            // Nearest-neighbor scale and blit
            for dy in 0..disp_h {
                let src_y = (dy as u64 * img_h as u64 / disp_h as u64) as u32;
                let src_row_offset = (src_y * img_w * 3) as usize;
                let dst_row_offset = ((offset_y + dy) * stride + offset_x * 4) as usize;

                for (dx, &src_x) in src_x_table.iter().enumerate() {
                    let src_idx = src_row_offset + (src_x * 3) as usize;
                    let dst_idx = dst_row_offset + dx * 4;

                    if src_idx + 2 < rgb.len() && dst_idx + 3 < canvas.len() {
                        canvas[dst_idx] = rgb[src_idx + 2]; // B
                        canvas[dst_idx + 1] = rgb[src_idx + 1]; // G
                        canvas[dst_idx + 2] = rgb[src_idx]; // R
                        canvas[dst_idx + 3] = 0xFF;
                    }
                }
            }

            // Draw detection bounding boxes scaled to display coordinates
            let scale_x = disp_w as f32 / img_w as f32;
            let scale_y = disp_h as f32 / img_h as f32;

            for face in faces {
                let bx = offset_x + (face.x * scale_x) as u32;
                let by = offset_y + (face.y * scale_y) as u32;
                let bw = (face.width * scale_x) as u32;
                let bh = (face.height * scale_y) as u32;
                render::draw_detection_box(
                    canvas,
                    stride,
                    height,
                    bx,
                    by,
                    bw,
                    bh,
                    face.similarity,
                    face.recognized,
                );
            }

            let recognized = faces.iter().filter(|f| f.recognized).count() as u32;
            let unrecognized = faces.len() as u32 - recognized;
            render::draw_info_bar(canvas, stride, width, height, fps, recognized, unrecognized);
        }
        Err(e) => {
            tracing::warn!("JPEG decode failed: {e}");
            render_error(canvas, width, height, "JPEG decode error");
        }
    }
}

/// Render an error message on a dark red background.
fn render_error(canvas: &mut [u8], width: u32, height: u32, message: &str) {
    let stride = width * 4;

    for chunk in canvas.chunks_exact_mut(4) {
        chunk[0] = 0;
        chunk[1] = 0;
        chunk[2] = 40;
        chunk[3] = 0xFF;
    }

    let text_x = 8;
    let text_y = height / 2;
    super::font::draw_text(canvas, stride, text_x, text_y, message, render::COLOR_WHITE);
}

/// Decode JPEG bytes to raw RGB pixel data.
fn decode_jpeg(data: &[u8]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    use image::ImageReader;
    let reader = ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .context("failed to detect image format")?;
    let img = reader.decode().context("failed to decode JPEG")?;
    let rgb = img.to_rgb8();
    let w = rgb.width();
    let h = rgb.height();
    Ok((rgb.into_raw(), w, h))
}

/// Compute display dimensions to fit source into target, preserving aspect ratio.
fn fit_dimensions(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (max_w, max_h);
    }

    let scale_w = max_w as f64 / src_w as f64;
    let scale_h = max_h as f64 / src_h as f64;
    let scale = scale_w.min(scale_h).min(1.0); // Don't upscale

    let disp_w = (src_w as f64 * scale) as u32;
    let disp_h = (src_h as f64 * scale) as u32;
    (disp_w.max(1), disp_h.max(1))
}

// --- Wayland protocol handler implementations ---

impl CompositorHandler for PreviewState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for PreviewState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for PreviewState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 > 0 {
            self.width = configure.new_size.0;
        }
        if configure.new_size.1 > 0 {
            self.height = configure.new_size.1;
        }

        if self.first_configure {
            self.first_configure = false;
            self.draw(qh);
        }
    }
}

impl SeatHandler for PreviewState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(e) => tracing::warn!("failed to get keyboard: {e}"),
            }
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(e) => tracing::warn!("failed to get pointer: {e}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(ptr) = self.pointer.take() {
                ptr.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for PreviewState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _keysyms: &[Keysym],
    ) {
        if self.layer.wl_surface() == surface {
            self.keyboard_focus = true;
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.layer.wl_surface() == surface {
            self.keyboard_focus = false;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        // Close on Escape or 'q'
        if event.keysym == Keysym::Escape || event.keysym == Keysym::q {
            self.exit = true;
        }
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
    }
}

impl PointerHandler for PreviewState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            // We don't need pointer interaction for preview,
            // but we must implement the handler.
            if let PointerEventKind::Press { .. } = event.kind {
                // Click to close could be added here if desired
            }
        }
    }
}

impl ShmHandler for PreviewState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(PreviewState);
delegate_output!(PreviewState);
delegate_shm!(PreviewState);
delegate_seat!(PreviewState);
delegate_keyboard!(PreviewState);
delegate_pointer!(PreviewState);
delegate_layer!(PreviewState);
delegate_registry!(PreviewState);

impl ProvidesRegistryState for PreviewState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_dimensions_no_upscale() {
        let (w, h) = fit_dimensions(320, 240, 640, 480);
        assert_eq!((w, h), (320, 240));
    }

    #[test]
    fn fit_dimensions_downscale() {
        let (w, h) = fit_dimensions(1280, 720, 640, 480);
        // 1280/640 = 2.0, 720/480 = 1.5 -> scale = 0.5
        assert_eq!((w, h), (640, 360));
    }

    #[test]
    fn fit_dimensions_zero_source() {
        let (w, h) = fit_dimensions(0, 0, 640, 480);
        assert_eq!((w, h), (640, 480));
    }

    #[test]
    fn fit_dimensions_square_into_wide() {
        let (w, h) = fit_dimensions(800, 800, 640, 480);
        // scale = min(640/800, 480/800) = min(0.8, 0.6) = 0.6
        assert_eq!((w, h), (480, 480));
    }
}
