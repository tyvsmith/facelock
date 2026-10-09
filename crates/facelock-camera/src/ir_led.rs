//! IR illuminators exposed as Linux LED class devices.
//!
//! Some IR cameras behind a media-controller pipeline (e.g. the Surface Pro
//! 11's Qualcomm CAMSS path) have no UVC extension unit: the illuminator is a
//! separate LED class device, such as a PMIC flash LED in torch mode, that
//! nothing turns on during streaming. [`IrLed`] lights it at full brightness
//! for as long as it lives and switches it off when dropped.

use std::fs;
use std::path::{Path, PathBuf};

use facelock_core::config::led_class_name;

const LED_CLASS_DIR: &str = "/sys/class/leds";

/// A lit LED class device; switched off on drop.
#[derive(Debug)]
pub struct IrLed {
    dir: PathBuf,
}

impl IrLed {
    /// Light the LED named by `spec`, which must be `/sys/class/leds/<name>`.
    pub fn on(spec: &str) -> Result<Self, String> {
        let name = led_class_name(spec)
            .ok_or_else(|| format!("{spec} is not a /sys/class/leds/<name> path"))?;
        Self::on_in(Path::new(LED_CLASS_DIR), name)
    }

    fn on_in(class_dir: &Path, name: &str) -> Result<Self, String> {
        let dir = class_dir.join(name);
        let max = fs::read_to_string(dir.join("max_brightness"))
            .map_err(|e| format!("{}: cannot read max_brightness: {e}", dir.display()))?;
        let max = max.trim();
        if !max.parse::<u32>().is_ok_and(|v| v > 0) {
            return Err(format!(
                "{}: unusable max_brightness {max:?}",
                dir.display()
            ));
        }
        write_brightness(&dir, max)?;
        Ok(Self { dir })
    }

    /// The LED class directory this controls.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for IrLed {
    fn drop(&mut self) {
        match write_brightness(&self.dir, "0") {
            Ok(()) => tracing::debug!("IR LED off: {}", self.dir.display()),
            Err(e) => tracing::warn!("failed to switch IR LED off: {e}"),
        }
    }
}

fn write_brightness(dir: &Path, value: &str) -> Result<(), String> {
    fs::write(dir.join("brightness"), value)
        .map_err(|e| format!("{}: cannot set brightness {value}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake LED class directory, removed when the guard drops.
    struct FakeClass(PathBuf);

    impl Drop for FakeClass {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_led(test: &str, max: &str) -> (FakeClass, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("facelock-ir-led-{}-{test}", std::process::id()));
        let dir = root.join("ir:flash");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("max_brightness"), max).unwrap();
        fs::write(dir.join("brightness"), "0\n").unwrap();
        (FakeClass(root), dir)
    }

    fn brightness(dir: &Path) -> String {
        fs::read_to_string(dir.join("brightness")).unwrap()
    }

    #[test]
    fn lights_at_max_brightness_and_switches_off_on_drop() {
        let (root, dir) = fake_led("lit", "255\n");
        let led = IrLed::on_in(&root.0, "ir:flash").unwrap();
        assert_eq!(brightness(&dir), "255");
        drop(led);
        assert_eq!(brightness(&dir), "0");
    }

    #[test]
    fn refuses_an_led_that_cannot_be_lit() {
        let (root, dir) = fake_led("unusable", "0\n");
        assert!(IrLed::on_in(&root.0, "ir:flash").is_err());
        assert_eq!(brightness(&dir), "0\n");
    }

    #[test]
    fn refuses_paths_outside_the_led_class() {
        for spec in [
            "/sys/class/leds",
            "/sys/class/leds/",
            "/sys/class/leds/..",
            "/sys/class/leds/a/b",
            "/etc/shadow",
            "ir:flash",
        ] {
            assert!(IrLed::on(spec).is_err(), "{spec} must be refused");
        }
    }
}
