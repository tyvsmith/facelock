use facelock_core::Config;
use facelock_core::ipc::IpcDeviceInfo;

use crate::backend::Backend;

pub fn run(config: &Config, json: bool) -> anyhow::Result<()> {
    // DEC-6/N13: `ListDevices` is root-only now (was facelock-group); root
    // is established by `main`'s `require_root_for` gate (C6).

    // One selection, one enumeration (D1); both transports render here. The
    // renderers used to live twice — this one and a direct copy that printed
    // its own loop.
    let backend = Backend::select(config);
    let devices = backend.list_devices()?;

    if json {
        print_json(&devices);
        return Ok(());
    }

    if devices.is_empty() {
        println!("No video devices found.");
        println!("Check that your camera is connected and the v4l2 module is loaded.");
        return Ok(());
    }

    // Format detail is a direct-only capability (the D-Bus DeviceInfo does
    // not carry it) — stated by the caps instead of silently printing
    // nothing.
    let show_formats = backend.caps().device_formats;

    println!("Available video devices:\n");
    for dev in &devices {
        let ir_tag = if dev.is_ir { " [IR]" } else { "" };
        println!("  {}{ir_tag}", dev.path);
        println!("    Name:    {}", dev.name);
        println!("    Driver:  {}", dev.driver);
        println!("    Platform: {}", platform_line(dev));

        if show_formats && !dev.formats.is_empty() {
            println!("    Formats:");
            for fmt in &dev.formats {
                let sizes: Vec<String> =
                    fmt.sizes.iter().map(|(w, h)| format!("{w}x{h}")).collect();
                println!(
                    "      {} ({}) — {}",
                    fmt.fourcc.trim(),
                    fmt.description,
                    if sizes.is_empty() {
                        "no sizes reported".to_string()
                    } else {
                        sizes.join(", ")
                    }
                );
            }
        }
        println!();
    }

    Ok(())
}

/// The platform identity a `modalias` + `node_name` quirk would name, as
/// `modalias / node_name`, or `none` for a USB or virtual node.
fn platform_line(dev: &IpcDeviceInfo) -> String {
    match (&dev.modalias, &dev.node_name) {
        (Some(modalias), Some(node_name)) => format!("{modalias} / {node_name}"),
        _ => "none".to_string(),
    }
}

/// `facelock devices --json`: serde-derived output (see
/// `facelock_core::ipc::IpcDeviceInfo`), so a consumer parses a stable typed
/// schema instead of the human-readable renderer above — the renderer's
/// layout (columns, the `[IR]` tag, indentation) is free to change without
/// breaking anything that reads `--json`.
fn print_json(devices: &[IpcDeviceInfo]) {
    let json = serde_json::to_string(devices).unwrap_or_else(|_| "[]".to_string());
    // The seam's machine sink: never localized, and silenced by `--quiet`
    // like every other payload (before #140 this command ignored the flag).
    crate::message::payload(&json);
}

#[cfg(test)]
mod tests {
    use super::*;
    use facelock_core::ipc::IpcFormatInfo;

    fn sample_devices() -> Vec<IpcDeviceInfo> {
        vec![
            IpcDeviceInfo {
                path: "/dev/video0".to_string(),
                name: "Logitech BRIO".to_string(),
                driver: "uvcvideo".to_string(),
                is_ir: false,
                formats: vec![IpcFormatInfo {
                    fourcc: "YUYV".to_string(),
                    description: "YUYV 4:2:2".to_string(),
                    sizes: vec![(1920, 1080), (640, 480)],
                }],
                modalias: None,
                node_name: None,
            },
            IpcDeviceInfo {
                path: "/dev/video2".to_string(),
                name: "Logitech BRIO IR".to_string(),
                driver: "uvcvideo".to_string(),
                is_ir: true,
                formats: vec![IpcFormatInfo {
                    fourcc: "GREY".to_string(),
                    description: "8-bit Greyscale".to_string(),
                    sizes: vec![(640, 480)],
                }],
                modalias: Some("auxiliary:intel_ipu6.isys".to_string()),
                node_name: Some("Intel IPU6 ISYS Capture 0".to_string()),
            },
        ]
    }

    // Device listing requires hardware or a running daemon; the transport
    // fork and its failure policy are pinned in `crate::backend`. What is
    // pure and testable here is the JSON shape itself.

    #[test]
    fn json_output_is_a_parseable_array_with_the_documented_fields() {
        let rendered = serde_json::to_string(&sample_devices()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        let arr = parsed.as_array().expect("top level must be a JSON array");
        assert_eq!(arr.len(), 2);

        assert_eq!(arr[0]["path"], "/dev/video0");
        assert_eq!(arr[0]["is_ir"], false);
        assert_eq!(arr[0]["formats"][0]["fourcc"], "YUYV");
        assert_eq!(arr[0]["formats"][0]["sizes"][0][0], 1920);
        assert_eq!(arr[0]["formats"][0]["sizes"][0][1], 1080);

        assert_eq!(arr[1]["path"], "/dev/video2");
        assert_eq!(arr[1]["is_ir"], true);
        assert_eq!(arr[1]["formats"][0]["fourcc"], "GREY");

        // Platform identity is additive: null for a USB node, the quirk
        // keys verbatim for a platform node.
        assert!(arr[0]["modalias"].is_null());
        assert!(arr[0]["node_name"].is_null());
        assert_eq!(arr[1]["modalias"], "auxiliary:intel_ipu6.isys");
        assert_eq!(arr[1]["node_name"], "Intel IPU6 ISYS Capture 0");
    }

    #[test]
    fn human_platform_line_names_both_keys_or_none() {
        let devices = sample_devices();
        assert_eq!(platform_line(&devices[0]), "none");
        assert_eq!(
            platform_line(&devices[1]),
            "auxiliary:intel_ipu6.isys / Intel IPU6 ISYS Capture 0"
        );
    }

    #[test]
    fn json_output_is_an_empty_array_when_no_devices() {
        let rendered = serde_json::to_string(&Vec::<IpcDeviceInfo>::new()).unwrap();
        assert_eq!(rendered, "[]");
    }
}
