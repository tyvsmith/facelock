//! `facelock capabilities` — what *this* build can do, as capability names.
//!
//! ```text
//! facelock capabilities [--json] [--quiet]
//! ```
//!
//! # What it replaces
//!
//! Integrations branch on features today by grepping help text —
//! `facelock setup --help 2>/dev/null | grep -q -- "--no-pam"` is the shape in
//! the wild. **Help text is not an API**: it breaks on a reworded flag
//! description, a line wrap, a translated help template, or a flag that moved
//! to a subcommand. This command is the supported probe, so a wrapper script
//! can ask a direct question and get a stable answer.
//!
//! Bare form prints one name per line; `--json` prints
//! `{"version": "...", "capabilities": [...]}`. Both exit 0 — there is no
//! failure path — and `--quiet` suppresses stdout entirely, leaving the exit
//! code as the whole answer, as it does for [`crate::commands::is_enrolled`].
//! A build predating the command answers with clap's usage error on stderr and
//! exit 2, which a caller reads as "no capabilities at all".
//!
//! # Hard requirements
//!
//! The answer comes from this binary's own clap tree and compiled-in
//! constants. Nothing here may read a config file, open the database, activate
//! the daemon over D-Bus, or touch a camera — a probe that needed root or a
//! working install to answer "what can you do?" would be useless to the
//! user-level setup script that is its consumer. That is also why it is
//! dispatched ahead of every config-consuming command in `main`.
//!
//! Output is **not** localized. A capability name is an identifier, not prose,
//! so it goes out through `message::payload` — the seam's machine sink, which
//! consults no catalog (`message/mod.rs`, §"What must NOT come through
//! here") — and never through `Terminal::info`. That is the same rule
//! `is-enrolled` follows for its state words, and it is what makes `--quiet`
//! one implementation instead of a `bool` threaded through this file.
//!
//! # Why the list has to be provable
//!
//! A hand-maintained feature list rots the moment a flag is renamed, and a
//! stale list is worse than no list: a consumer that trusts it invokes
//! something that is not there. So [`CAPABILITIES`] is not documentation —
//! `capability_names_are_all_implemented` in `conformance/capabilities.rs`
//! maps every name to the clap subcommand or argument that declares the
//! surface it names, and a name nothing backs fails the build rather than
//! shipping.

use crate::message;

/// The capability names this build emits. Sorted and deduplicated — the
/// contract says the array is, and a const that is already sorted is the
/// cheapest way to keep it so.
///
/// Adding a name here is a promise. `capability_names_are_all_implemented`
/// in `conformance/capabilities.rs` refuses to build a name that nothing in
/// the clap tree backs.
pub const CAPABILITIES: &[&str] = &[
    "capabilities",
    "config-edit",
    "daemon-restart",
    "devices-json",
    "is-enrolled",
    "is-enrolled-json",
    "pam-allow-sensitive",
    "pam-dry-run",
    "pam-if-present",
    "pam-json",
    "pam-multi-service",
    "pam-remove-all",
    "pam-status",
    "pam-status-all",
    "quiet",
    "setup-if-present",
    "setup-no-pam",
    "setup-systemd",
    "status-json",
    "tpm-decrypt",
    "tpm-encrypt",
    "tpm-reseal",
];

/// This binary's version — the string `facelock --version` prints.
///
/// Read from the same place `#[command(version)]` reads it, so the two cannot
/// disagree. It is for humans and bug reports: consumers branch on names, not
/// on this (a git or distro build carries a version that says nothing about
/// what is in it, and a backport can add a feature without moving the number).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `{"version": "...", "capabilities": [...]}` — the documented shape.
///
/// Serialized through `serde_json` rather than `format!` so the array is
/// escaped by the serializer, as `is_enrolled::enrolled_json` does. Key order
/// is whatever the serializer picks and is explicitly not contractual.
fn payload_json() -> String {
    serde_json::json!({
        "version": VERSION,
        "capabilities": CAPABILITIES,
    })
    .to_string()
}

/// What [`run`] prints. The whole of this command's logic, with the printing
/// left to the caller so a test can assert on it without capturing stdout (the
/// `is_enrolled::report` split).
///
/// `--quiet` is not consulted here: both forms are machine output, so they go
/// out through [`message::payload`], which is where the flag acts.
fn rendered(json: bool) -> String {
    if json {
        payload_json()
    } else {
        CAPABILITIES.join("\n")
    }
}

/// Run `facelock capabilities`. Infallible by construction: no file, no
/// socket, no camera, so there is no error to return and no exit code but 0.
pub fn run(json: bool) {
    message::payload(&rendered(json));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name this binary has ever emitted. Contract: names are added,
    /// never removed or repurposed. This list is append-only history — do not
    /// edit a name out of it, ever; if a name here is missing from
    /// `CAPABILITIES` the contract has been broken, not the test.
    ///
    /// **When you add a name to `CAPABILITIES`, add it here in the same
    /// commit.** The two lists are identical today and stay identical unless
    /// the contract is violated; this one exists so that a later deletion has
    /// something to fail against.
    const CAPABILITIES_EVER_EMITTED: &[&str] = &[
        "capabilities",
        "config-edit",
        "daemon-restart",
        "devices-json",
        "is-enrolled",
        "is-enrolled-json",
        "pam-allow-sensitive",
        "pam-dry-run",
        "pam-if-present",
        "pam-json",
        "pam-multi-service",
        "pam-status",
        "pam-status-all",
        "quiet",
        "setup-if-present",
        "setup-no-pam",
        "setup-systemd",
        "status-json",
        "tpm-decrypt",
        "tpm-encrypt",
        "tpm-reseal",
    ];

    /// The half of the contract no clap predicate can see: a consumer that
    /// branched on a name must keep working against every later build.
    #[test]
    fn capability_names_are_never_removed() {
        for name in CAPABILITIES_EVER_EMITTED {
            assert!(
                CAPABILITIES.contains(name),
                "`{name}` has been emitted by a released build and is gone: \
                 names are added, never removed"
            );
        }
    }

    /// Strict `<` proves sorted and deduplicated in one pass — the contract
    /// promises the array is both.
    #[test]
    fn capability_list_is_sorted_and_deduplicated() {
        for pair in CAPABILITIES.windows(2) {
            assert!(
                pair[0] < pair[1],
                "CAPABILITIES must be sorted and deduplicated: `{}` precedes `{}`",
                pair[0],
                pair[1]
            );
        }
    }

    /// Lowercase and hyphenated: `[a-z0-9]+(-[a-z0-9]+)*`. Hand-checked rather
    /// than by regex — this crate has no regex dependency and does not want one
    /// for a rule this small.
    #[test]
    fn capability_names_follow_the_naming_rule() {
        for name in CAPABILITIES {
            assert!(!name.is_empty(), "a capability name must not be empty");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "`{name}` must be lowercase alphanumeric and hyphens only"
            );
            assert!(
                !name.starts_with('-') && !name.ends_with('-'),
                "`{name}` must not start or end with a hyphen"
            );
            assert!(!name.contains("--"), "`{name}` must not contain `--`");
        }
    }

    /// Every emitted name has a row in the contract saying what it means.
    ///
    /// `capability_names_are_all_implemented` in `conformance/capabilities.rs`
    /// proves a name is backed by a real surface; this proves the same name
    /// is *documented*,
    /// which is the other half of a promise — a consumer cannot branch on a
    /// name whose meaning is written down nowhere. `include_str!` reaches out
    /// of the crate the way `setup.rs` embeds the systemd unit and D-Bus
    /// policy, so deleting a row breaks the build rather than the contract.
    #[test]
    fn capabilities_are_all_documented_in_contracts() {
        const CONTRACTS: &str = include_str!("../../../../docs/contracts.md");

        for name in CAPABILITIES {
            let row = format!("| `{name}` |");
            assert!(
                CONTRACTS.contains(&row),
                "`{name}` is emitted but has no `{row}` row in docs/contracts.md"
            );
        }
    }

    #[test]
    fn capabilities_json_matches_the_documented_shape() {
        let parsed: serde_json::Value =
            serde_json::from_str(&payload_json()).expect("the payload must be JSON");
        let object = parsed.as_object().expect("the payload must be an object");

        assert_eq!(
            object.len(),
            2,
            "the document has exactly `version` and `capabilities`, got {:?}",
            object.keys().collect::<Vec<_>>()
        );

        // Only that it is this binary's own version. The *shape* of the string
        // is Cargo's problem: `VERSION` is `env!("CARGO_PKG_VERSION")`, which
        // Cargo already refuses to build unless it is valid semver — and a
        // stricter check here would reject the prerelease versions
        // `docs/releasing.md` documents (`v0.2.0-rc1`).
        assert_eq!(
            object["version"].as_str(),
            Some(VERSION),
            "`version` is this binary's own version, as a string"
        );

        let names = object["capabilities"]
            .as_array()
            .expect("`capabilities` is an array");
        assert_eq!(names.len(), CAPABILITIES.len());
        for (emitted, expected) in names.iter().zip(CAPABILITIES) {
            assert_eq!(
                emitted.as_str(),
                Some(*expected),
                "every element is a string, in CAPABILITIES order"
            );
        }
    }

    /// The two rendering modes. `--quiet` is absent on purpose: it is the
    /// payload sink's business now, pinned in `message::mod`'s
    /// `quiet_silences_stdout_except_notice`.
    #[test]
    fn capabilities_output_modes() {
        assert_eq!(rendered(false), CAPABILITIES.join("\n"));
        assert_eq!(rendered(true), payload_json());
    }
}
