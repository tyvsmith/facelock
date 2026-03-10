# Spec 08: Preview Window

**Phase**: 5 (Polish) | **Crate**: visage-cli | **Depends on**: 07 | **Parallel with**: 09, 10

## Goal

Live camera preview in a Wayland layer-shell overlay window with real-time face detection visualization. Invoked via `visage preview`.

## Dependencies (added to visage-cli)

- `smithay-client-toolkit` (Wayland client, layer-shell)
- `wayland-client` (Wayland protocol)

## Design

### Window Properties

- Protocol: `zwlr_layer_shell_v1` (layer-shell)
- Layer: Overlay (top)
- Size: camera resolution, capped at 640x480
- Exclusive zone: 0 (doesn't push other windows)
- Keyboard: on-demand (for Escape key to close)
- Pixel format: XRGB8888 via `wl_shm` shared memory

### Rendering

Direct pixel manipulation (no GPU, no Cairo, no Skia):

1. Copy camera RGB frame to XRGB8888 SHM buffer
2. Draw bounding boxes: green = matched face, red = unmatched, 2px thickness
3. Draw confidence labels with embedded 8x8 bitmap font
4. Info bar: FPS counter, resolution, detection count

### Event Loop

```
1. Initialize Wayland connection, layer-shell, SHM pool (double-buffered)
2. Connect to daemon
3. Per frame (~30fps target):
   a. Dispatch Wayland events (keyboard, close)
   b. Send PreviewFrame request to daemon
   c. Receive JPEG frame + detection overlays
   d. Decode JPEG to RGB
   e. Run face detection overlay rendering
   f. Copy to SHM buffer
   g. Attach buffer, damage, commit to wl_surface
4. Exit on Escape, 'q', or window close
```

### Fallback

If Wayland not available or `zwlr_layer_shell_v1` not supported:
- `--text-only` mode: print detection results to stdout (JSON per frame)
- Useful for SSH sessions, non-Wayland environments, and testing

## Implementation Notes

- Embedded 8x8 bitmap font for text rendering (no freetype dependency)
- Double-buffered SHM: render to back buffer while front buffer is displayed
- Frame timing: request new frame immediately after rendering previous
- The daemon handles all inference; the preview just displays results

## Tests

- Build verification (compile with smithay features)
- Text-only fallback validation
- Note: visual testing is manual

## Acceptance Criteria

1. Layer-shell window appears on Wayland compositors (Hyprland, Sway)
2. Camera feed displays with bounding box overlays
3. FPS counter and info bar visible
4. Escape key closes window
5. `--text-only` fallback works without Wayland
6. No crashes on compositor disconnect

## Verification

```bash
cargo build -p visage-cli
# Manual: cargo run --bin visage -- preview
```
