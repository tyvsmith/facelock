use std::fs;
use std::path::Path;

pub fn run() -> anyhow::Result<()> {
    println!("Available video devices:\n");

    let mut found = false;

    // Enumerate /dev/video* devices
    let dev_dir = Path::new("/dev");
    if let Ok(entries) = fs::read_dir(dev_dir) {
        let mut video_devices: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("video")
            })
            .collect();

        video_devices.sort_by_key(|e| e.file_name());

        for entry in &video_devices {
            let path = entry.path();
            let name = read_sysfs_name(&path);
            let driver = read_sysfs_driver(&path);

            println!("  {}", path.display());
            if let Some(name) = &name {
                println!("    Name:   {name}");
            }
            if let Some(driver) = &driver {
                println!("    Driver: {driver}");
            }
            println!();
            found = true;
        }
    }

    if !found {
        println!("  No video devices found.");
        println!("  Check that your camera is connected and the v4l2 module is loaded.");
    }

    Ok(())
}

/// Try to read the device name from sysfs.
fn read_sysfs_name(dev_path: &Path) -> Option<String> {
    let dev_name = dev_path.file_name()?.to_string_lossy();
    let sysfs_path = format!("/sys/class/video4linux/{dev_name}/name");
    fs::read_to_string(sysfs_path).ok().map(|s| s.trim().to_string())
}

/// Try to read the driver name from sysfs.
fn read_sysfs_driver(dev_path: &Path) -> Option<String> {
    let dev_name = dev_path.file_name()?.to_string_lossy();
    // The driver is typically a symlink under the device directory
    let sysfs_path = format!("/sys/class/video4linux/{dev_name}/device/driver");
    let driver_link = fs::read_link(sysfs_path).ok()?;
    driver_link
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_sysfs_name_nonexistent() {
        let result = read_sysfs_name(Path::new("/dev/nonexistent_device"));
        assert!(result.is_none());
    }

    #[test]
    fn read_sysfs_driver_nonexistent() {
        let result = read_sysfs_driver(Path::new("/dev/nonexistent_device"));
        assert!(result.is_none());
    }
}
