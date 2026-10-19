//! IR illuminators exposed as Linux LED class devices.
//!
//! Some IR cameras behind a media-controller pipeline (e.g. the Surface Pro
//! 11's Qualcomm CAMSS path) have no UVC extension unit: the illuminator is a
//! separate LED class device, such as a PMIC flash LED in torch mode, that
//! nothing turns on during streaming. [`IrLed`] lights it at full brightness
//! for as long as it lives and restores the prior brightness when dropped.
//! [`probe`] reads a configured LED's state without writing to it.

use std::fs;
use std::path::{Path, PathBuf};

use facelock_core::config::led_class_name;

const LED_CLASS_DIR: &str = "/sys/class/leds";

/// A lit LED class device; its prior brightness is restored on drop.
#[derive(Debug)]
pub struct IrLed {
    dir: PathBuf,
    /// Written back on drop.
    restore: String,
}

/// A configured LED as read from sysfs, without writing to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrLedState {
    pub brightness: u32,
    pub max_brightness: u32,
}

/// Read the LED named by `spec` without changing it: the checks [`IrLed::on`]
/// makes before lighting it, plus whether `brightness` opens for writing.
/// Opening writes nothing, so a lit LED stays lit and an off one stays off.
pub fn probe(spec: &str) -> Result<IrLedState, String> {
    let name = led_class_name(spec)
        .ok_or_else(|| format!("{spec} is not a /sys/class/leds/<name> path"))?;
    probe_in(Path::new(LED_CLASS_DIR), name)
}

fn probe_in(class_dir: &Path, name: &str) -> Result<IrLedState, String> {
    let dir = class_dir.join(name);
    let max = read_max_brightness(&dir)?;
    let brightness = fs::read_to_string(dir.join("brightness"))
        .map_err(|e| format!("{}: cannot read brightness: {e}", dir.display()))?;
    let brightness = brightness.trim().parse::<u32>().map_err(|_| {
        format!(
            "{}: unusable brightness {:?}",
            dir.display(),
            brightness.trim()
        )
    })?;
    fs::OpenOptions::new()
        .write(true)
        .open(dir.join("brightness"))
        .map_err(|e| format!("{}: cannot open brightness for writing: {e}", dir.display()))?;
    Ok(IrLedState {
        brightness,
        max_brightness: max,
    })
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
        let max = read_max_brightness(&dir)?.to_string();
        let prior = fs::read_to_string(dir.join("brightness"))
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        write_brightness(&dir, &max)?;
        Ok(Self {
            dir,
            restore: restore_value(&prior, &max).to_string(),
        })
    }

    /// The LED class directory this controls.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for IrLed {
    fn drop(&mut self) {
        match write_brightness(&self.dir, &self.restore) {
            Ok(()) => tracing::debug!(
                "IR LED restored to {}: {}",
                self.restore,
                self.dir.display()
            ),
            Err(e) => tracing::warn!("failed to restore IR LED brightness: {e}"),
        }
    }
}

/// The brightness to write back on drop. A prior value already at `max` is
/// indistinguishable from this LED left lit by a daemon that died without
/// unwinding, so it restores to off; so does an unreadable or non-numeric one.
fn restore_value<'a>(prior: &'a str, max: &str) -> &'a str {
    if prior == max || prior.parse::<u32>().is_err() {
        "0"
    } else {
        prior
    }
}

fn read_max_brightness(dir: &Path) -> Result<u32, String> {
    let max = fs::read_to_string(dir.join("max_brightness"))
        .map_err(|e| format!("{}: cannot read max_brightness: {e}", dir.display()))?;
    let max = max.trim();
    match max.parse::<u32>() {
        Ok(v) if v > 0 => Ok(v),
        _ => Err(format!(
            "{}: unusable max_brightness {max:?}",
            dir.display()
        )),
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
        fake_led_at(test, max, "0\n")
    }

    fn fake_led_at(test: &str, max: &str, brightness: &str) -> (FakeClass, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("facelock-ir-led-{}-{test}", std::process::id()));
        let dir = root.join("ir:flash");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("max_brightness"), max).unwrap();
        fs::write(dir.join("brightness"), brightness).unwrap();
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
    fn restores_the_prior_brightness_on_drop() {
        let (root, dir) = fake_led_at("prior", "255\n", "40\n");
        let led = IrLed::on_in(&root.0, "ir:flash").unwrap();
        assert_eq!(brightness(&dir), "255");
        drop(led);
        assert_eq!(brightness(&dir), "40");
    }

    #[test]
    fn a_prior_brightness_at_max_restores_to_off() {
        // What an LED left lit by a SIGKILLed daemon reads as.
        let (root, dir) = fake_led_at("left-lit", "255\n", "255\n");
        let led = IrLed::on_in(&root.0, "ir:flash").unwrap();
        drop(led);
        assert_eq!(brightness(&dir), "0");
    }

    #[test]
    fn probe_reads_state_without_writing() {
        let (root, dir) = fake_led_at("probe", "255\n", "7\n");
        let state = probe_in(&root.0, "ir:flash").unwrap();
        assert_eq!(
            state,
            IrLedState {
                brightness: 7,
                max_brightness: 255
            }
        );
        assert_eq!(brightness(&dir), "7\n");
    }

    #[test]
    fn probe_names_a_missing_led() {
        let (root, _dir) = fake_led("probe-missing", "255\n");
        let err = probe_in(&root.0, "ir:absent").unwrap_err();
        assert!(err.contains("ir:absent"), "{err}");
        assert!(err.contains("max_brightness"), "{err}");
    }

    #[test]
    fn probe_refuses_paths_outside_the_led_class() {
        assert!(probe("/etc/shadow").is_err());
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
