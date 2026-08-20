pub mod caps;
pub mod capture;
pub mod device;
pub mod ir_emitter;
pub mod preprocess;
pub mod quirks;

pub use caps::{ResolvedCamera, quirk_id};
pub use capture::{Camera, MMAP_BUFFERS, is_dark_with_config};
pub use device::{
    DeviceInfo, FormatInfo, IrSource, auto_detect_device, auto_detect_device_with,
    classify_ir_sources, has_decodable_format, ir_source, ir_source_resolved,
    ir_source_with_quirks, is_ir_camera, is_ir_camera_resolved, is_ir_camera_with_quirks,
    list_devices, validate_device,
};
pub use ir_emitter::EmitterXuInfo;
pub use preprocess::{
    check_ir_texture, clahe, extract_bbox_region, nv12_to_rgb, rgb_to_gray, y16_to_gray,
    yuyv_to_rgb,
};
pub use quirks::{Quirk, QuirksDb, device_fingerprint};
