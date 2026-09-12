// The laptop-lid resolver behind `security.abort_if_lid_closed`. Dependency-
// free on purpose (std only, no `use` of anything outside it): this file is
// compiled twice — as `pam_facelock`'s `lid` module and `include!`d into
// `facelock-daemon` (auth.rs) — so the PAM module and the daemon run one
// resolver rather than two lists of device names. `pam_facelock.so` cannot
// link facelock-core (its dependency ceiling is libc/toml/serde/zbus), so
// sharing the source is what keeps the two gates agreeing (issue #365).
//
// The rule is Omarchy's `omarchy-hw-laptop-closed`: enumerate every
// `/proc/acpi/button/lid/*/state` (the kernel names the device `LID0`,
// `LID`, `LID1`, ... depending on the firmware) and treat the lid as closed
// when any of them reports `closed`. No lid device at all means "absent",
// which the gate reads as open: a desktop or a VM has no lid to be closed.

/// Where the kernel exposes ACPI lid buttons.
pub(crate) const LID_ROOT: &str = "/proc/acpi/button/lid";

/// What the ACPI lid devices under one root report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LidState {
    /// At least one lid device reports `closed`.
    Closed,
    /// At least one lid device is readable and none reports `closed`.
    Open,
    /// No readable lid device: no root, an empty root, or devices with no
    /// readable `state` file.
    Absent,
}

/// Resolve the lid state from every `<root>/*/state` file.
///
/// Order-independent: `Closed` if any device says so, `Open` if any device
/// was readable, `Absent` otherwise. Stray files directly under the root and
/// devices whose `state` cannot be read are skipped, never treated as closed.
pub(crate) fn lid_state_under(root: &std::path::Path) -> LidState {
    let Ok(entries) = std::fs::read_dir(root) else {
        return LidState::Absent;
    };
    let mut readable = false;
    for entry in entries.flatten() {
        let Ok(contents) = std::fs::read_to_string(entry.path().join("state")) else {
            continue;
        };
        readable = true;
        if contents.split_whitespace().any(|word| word == "closed") {
            return LidState::Closed;
        }
    }
    if readable {
        LidState::Open
    } else {
        LidState::Absent
    }
}

/// The gate's reading of [`lid_state_under`]: only `Closed` aborts, so a
/// machine with no lid device is treated as open.
pub(crate) fn is_lid_closed_under(root: &std::path::Path) -> bool {
    lid_state_under(root) == LidState::Closed
}

/// [`is_lid_closed_under`] on the real kernel root.
pub(crate) fn is_lid_closed() -> bool {
    is_lid_closed_under(std::path::Path::new(LID_ROOT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway `/proc/acpi/button/lid` stand-in. Hand-rolled rather than
    /// `tempfile` so this file stays dependency-free in both crates.
    struct LidRoot(PathBuf);

    /// Create and return the first candidate directory that does not already
    /// exist. `create_dir` is the exclusion primitive: it refuses a name
    /// something else owns instead of adopting it, so a directory left by a
    /// crashed run costs a retry rather than a panic or a shared fixture.
    fn create_first_free<I: Iterator<Item = PathBuf>>(candidates: I) -> PathBuf {
        for candidate in candidates {
            match std::fs::create_dir(&candidate) {
                Ok(()) => return candidate,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create {}: {e}", candidate.display()),
            }
        }
        panic!("no free temp directory candidate");
    }

    impl LidRoot {
        fn new(devices: &[(&str, Option<&str>)]) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let crate_name = env!("CARGO_PKG_NAME");
            let candidates = (0..64).map(|_| {
                let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                base.join(format!("facelock-lid-{crate_name}-{pid}-{nanos}-{n}"))
            });
            let root = create_first_free(candidates);
            for (name, state) in devices {
                let dir = root.join(name);
                std::fs::create_dir(&dir).unwrap();
                if let Some(contents) = state {
                    std::fs::write(dir.join("state"), contents).unwrap();
                }
            }
            Self(root)
        }
    }

    impl Drop for LidRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One lid device directory: its name and its `state` file (None = a
    /// device directory with no state file).
    type Device = (&'static str, Option<&'static str>);

    /// The fixture set both crates run: a name, the devices, and the answer.
    /// The kernel's format is `state:      closed\n` / `state:      open\n`.
    const FIXTURES: &[(&str, &[Device], LidState)] = &[
        ("no devices", &[], LidState::Absent),
        (
            "LID0 open",
            &[("LID0", Some("state:      open\n"))],
            LidState::Open,
        ),
        (
            "LID0 closed",
            &[("LID0", Some("state:      closed\n"))],
            LidState::Closed,
        ),
        (
            "LID open",
            &[("LID", Some("state:      open\n"))],
            LidState::Open,
        ),
        (
            "LID closed",
            &[("LID", Some("state:      closed\n"))],
            LidState::Closed,
        ),
        (
            "LID1 closed",
            &[("LID1", Some("state:      closed\n"))],
            LidState::Closed,
        ),
        (
            "firmware-named closed",
            &[("LID9", Some("state:      closed\n"))],
            LidState::Closed,
        ),
        (
            "any closed wins",
            &[
                ("LID0", Some("state:      open\n")),
                ("LID1", Some("state:      closed\n")),
            ],
            LidState::Closed,
        ),
        (
            "all open",
            &[
                ("LID0", Some("state:      open\n")),
                ("LID1", Some("state:      open\n")),
            ],
            LidState::Open,
        ),
        (
            "device without state file",
            &[("LID0", None)],
            LidState::Absent,
        ),
        (
            "unreadable device beside a closed one",
            &[("LID0", None), ("LID1", Some("state:      closed\n"))],
            LidState::Closed,
        ),
        ("empty state file", &[("LID0", Some(""))], LidState::Open),
        (
            "unknown state word",
            &[("LID0", Some("state:      unknown\n"))],
            LidState::Open,
        ),
    ];

    /// A name a crashed run left behind (same PID, later reused) is skipped,
    /// never adopted and never fatal. `create_dir` is the exclusion
    /// primitive; this pins that its `AlreadyExists` is a retry.
    #[test]
    fn create_first_free_skips_a_taken_name() {
        let taken = LidRoot::new(&[]);
        let fresh = taken.0.with_extension("fresh");
        let chosen = create_first_free(vec![taken.0.clone(), fresh.clone()].into_iter());
        assert_eq!(chosen, fresh);
        assert!(fresh.is_dir());
        let _ = std::fs::remove_dir_all(&fresh);
    }

    /// Two fixtures never share a directory, so one test's devices cannot
    /// leak into another's answer.
    #[test]
    fn lid_roots_are_unique_per_fixture() {
        let a = LidRoot::new(&[("LID0", Some("state:      open\n"))]);
        let b = LidRoot::new(&[("LID0", Some("state:      closed\n"))]);
        assert_ne!(a.0, b.0);
        assert_eq!(lid_state_under(&a.0), LidState::Open);
        assert_eq!(lid_state_under(&b.0), LidState::Closed);
    }

    /// The kernel path is the whole contract with the firmware: a typo here
    /// silently disables the gate on every machine, which is how issue #365
    /// stayed invisible. Pinned as a literal, not derived from the constant.
    #[test]
    fn lid_root_is_the_kernel_acpi_button_path() {
        assert_eq!(LID_ROOT, "/proc/acpi/button/lid");
    }

    #[test]
    fn lid_state_under_matches_the_fixture_table() {
        for (name, devices, expected) in FIXTURES {
            let root = LidRoot::new(devices);
            assert_eq!(lid_state_under(&root.0), *expected, "fixture: {name}");
        }
    }

    #[test]
    fn lid_state_under_missing_root_is_absent() {
        let root = LidRoot::new(&[]);
        let missing = root.0.join("does-not-exist");
        assert_eq!(lid_state_under(&missing), LidState::Absent);
    }

    #[test]
    fn lid_state_under_ignores_stray_files_in_the_root() {
        let root = LidRoot::new(&[("LID0", Some("state:      open\n"))]);
        std::fs::write(root.0.join("state"), "state:      closed\n").unwrap();
        assert_eq!(lid_state_under(&root.0), LidState::Open);
    }

    #[test]
    fn is_lid_closed_under_reads_only_closed_as_closed() {
        let closed = LidRoot::new(&[("LID", Some("state:      closed\n"))]);
        let open = LidRoot::new(&[("LID", Some("state:      open\n"))]);
        let none = LidRoot::new(&[]);
        assert!(is_lid_closed_under(&closed.0));
        assert!(!is_lid_closed_under(&open.0));
        assert!(!is_lid_closed_under(&none.0));
    }
}
