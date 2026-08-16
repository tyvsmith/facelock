//! The Messages seam (D10): user-facing text as typed events.
//!
//! A message is a thing the CLI wants to tell a human — an event carrying
//! data, not a string. Rendering happens at the edge, per sink, through the
//! [`Message`] trait:
//!
//! - [`Message::localized`] — gettext-translated text for a human (terminal
//!   via [`Terminal`], desktop notification bodies, `anyhow` errors whose
//!   `Display` lands on stderr via [`fail`]).
//! - [`Message::machine`] — a C-locale, locale-independent event line for the
//!   tracing side channel.
//!
//! `machine()` is a derived `Debug` rendering, which makes it a debugging aid
//! and not a serialization format — renaming a field changes the output with
//! nothing to catch it. Persisted records go through the audit crate's serde
//! path, which owns its own field names and its own compatibility rules; do
//! not route `machine()` into `audit.jsonl` or any other stored artifact.
//!
//! This is how D2 ("logs stay English") holds *by construction*: a message
//! routed through this type cannot reach a log sink in translated form,
//! because the log rendering never consults gettext. The boundary is the
//! sink, not the string — `bail!` text a human reads on stderr localizes,
//! the same event written to audit/syslog/tracing does not.
//!
//! # The vocabulary is split by domain
//!
//! There is no single `Message` enum. Each domain owns its own, and this
//! module owns everything they share — the trait, the sinks, and the gettext
//! plumbing ([`init`]). A feature touching one domain does not touch the
//! others' files, and no list here has to grow when a domain does:
//!
//! | Module | What it says |
//! |---|---|
//! | [`access`] | config load, privilege, which backend answered |
//! | [`face`] | `enroll` / `test` / `remove` / `clear` |
//! | [`status`] | the `facelock status` report's prose |
//! | [`notify`] | notification bodies (rendered `NotifyEvent`s) |
//! | [`setup`] | the wizard spine, step failures, closing summary |
//! | [`device`] | camera and inference-device selection |
//! | [`download`] | model download and verification |
//! | [`system`] | daemon unit and `facelock` group setup |
//! | [`pam`] | `/etc/pam.d/*` configuration |
//!
//! # Adding a message (the conversion pattern)
//!
//! 1. Pick the domain module, and add a variant carrying the data (not the
//!    formatted string): `EnrollComplete { model_id: u32, count: u32, label:
//!    String }`.
//! 2. Add one arm to that domain's `localized`. The English template is the
//!    msgid: keep it on a **single source line** with explicit `\n` escapes
//!    (no `\`-continuations, no raw strings) so `just pot` can extract it,
//!    and use **named placeholders** (`{user}`) filled via [`fill`] so
//!    translators can reorder them. Pre-format numbers that need precision
//!    (`format!("{similarity:.2}")`) before filling — Rust formatting is
//!    locale-independent, so numbers stay stable across locales.
//! 3. Link a sample into that domain's [`Samples`] walk. The compiler asks
//!    for this: the walk is a wildcard-free `match`, so a new variant does
//!    not build until the placeholder sweep can reach it.
//! 4. Replace the `println!`/`eprintln!`/`bail!` site with
//!    `Terminal.info(&msg)` / `Terminal.error(&msg)` / `return Err(fail(msg))`.
//!
//! The English fallback (no `.mo` installed, or the C locale) renders
//! byte-identically to the string the call site used to print; container
//! tests that grep CLI output rely on this.
//!
//! # What must NOT come through here
//!
//! Machine-facing output never localizes: `--json` output, `is-enrolled`'s
//! stdout contract, `audit.jsonl`, syslog/tracing lines, and D-Bus error
//! strings (PAM substring-matches those on the wire — see
//! `pam_code_for_daemon_error` in `pam-facelock`). Route none of them
//! through [`Message::localized`].
//!
//! # Why this lives in `facelock-cli`
//!
//! The two decided constraints (domain map §7): gettext everywhere (D1, via
//! the same ~20-line libc FFI the PAM module uses — `pam_facelock` keeps its
//! own copy and its own text domain), and logs stay English (D2). No non-CLI
//! crate renders user text today, and `facelock-core`'s dependency contract
//! excludes libc — when the daemon D-Bus server moves out of this crate
//! (H7), the seam moves with it or into core alongside that work.

use std::ffi::{CStr, CString};

pub mod access;
pub mod device;
pub mod download;
pub mod face;
pub mod notify;
pub mod pam;
pub mod setup;
pub mod status;
pub mod system;

pub use access::AccessMessage;
pub use device::DeviceMessage;
pub use download::DownloadMessage;
pub use face::FaceMessage;
pub use notify::NotifyMessage;
pub use pam::PamMessage;
pub use setup::SetupMessage;
pub use status::StatusMessage;
pub use system::SystemMessage;

// ---------------------------------------------------------------------------
// Gettext (libc FFI) — the CLI's copy of pam-facelock's wrapper, domain
// "facelock". Kept dependency-free on purpose: gettext ships in glibc.
// ---------------------------------------------------------------------------

/// Text domain for CLI translations (`/usr/share/locale/<lang>/LC_MESSAGES/facelock.mo`).
const GETTEXT_DOMAIN: &CStr = c"facelock";

unsafe extern "C" {
    safe fn dgettext(
        domainname: *const libc::c_char,
        msgid: *const libc::c_char,
    ) -> *const libc::c_char;
    safe fn bindtextdomain(
        domainname: *const libc::c_char,
        dirname: *const libc::c_char,
    ) -> *const libc::c_char;
    safe fn bind_textdomain_codeset(
        domainname: *const libc::c_char,
        codeset: *const libc::c_char,
    ) -> *const libc::c_char;
    // Not `safe`: glibc marks setlocale MT-Unsafe (const:locale) while
    // dgettext and bindtextdomain are MT-safe. It mutates process-global
    // state every later translation reads, so each call carries a SAFETY
    // note about who else could be running.
    fn setlocale(category: libc::c_int, locale: *const libc::c_char) -> *const libc::c_char;
}

/// Initialize localization for this process. Call once, early in `main`.
///
/// Sets LC_MESSAGES (only) from the environment: translations follow the
/// user's locale without letting LC_NUMERIC and friends leak into in-process
/// C libraries (ONNX Runtime parses floats). Output charset is pinned to
/// UTF-8 regardless of LC_CTYPE. `FACELOCK_LOCALEDIR` overrides the catalog
/// directory — translations only, English fallback otherwise, so it grants
/// nothing security-relevant.
pub fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // SAFETY: setlocale is MT-Unsafe against concurrent translation.
        // `init` is documented as "call once, early in `main`" and the `Once`
        // enforces the once — at that point the process is single-threaded,
        // so nothing can be inside dgettext while the locale changes.
        unsafe { setlocale(libc::LC_MESSAGES, c"".as_ptr()) };
        let dir =
            std::env::var("FACELOCK_LOCALEDIR").unwrap_or_else(|_| "/usr/share/locale".to_string());
        if let Ok(c_dir) = CString::new(dir) {
            bindtextdomain(GETTEXT_DOMAIN.as_ptr(), c_dir.as_ptr());
        }
        bind_textdomain_codeset(GETTEXT_DOMAIN.as_ptr(), c"UTF-8".as_ptr());
    });
}

/// Translate a msgid via gettext. Falls back to the original English string
/// if no translation is available or if gettext fails.
///
/// This is the xgettext extraction keyword (`just pot` scans the domain
/// modules for `translate("...")`), so every call must pass a string literal.
fn translate(msgid: &str) -> String {
    let Ok(c_msgid) = CString::new(msgid) else {
        return msgid.to_string();
    };
    let result = dgettext(GETTEXT_DOMAIN.as_ptr(), c_msgid.as_ptr());
    if result.is_null() {
        return msgid.to_string();
    }
    unsafe { CStr::from_ptr(result) }
        .to_str()
        .unwrap_or(msgid)
        .to_string()
}

/// Substitute named placeholders (`{user}`) in a translated template.
///
/// Values are substituted in order; a value that itself contains a later
/// placeholder token would be re-substituted, so keep placeholder names
/// unlikely to occur in data (they are).
fn fill(template: String, args: &[(&str, String)]) -> String {
    let mut out = template;
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

// ---------------------------------------------------------------------------
// The trait every domain implements
// ---------------------------------------------------------------------------

/// Something the CLI wants to tell a human. Data only — rendering is the
/// sink's job.
pub trait Message: std::fmt::Debug {
    /// Render for a human: gettext at the edge, English fallback
    /// byte-identical to the historical inline string.
    fn localized(&self) -> String;

    /// Render for the tracing side channel: the C-locale event line. Derived
    /// from the variant's `Debug` form — variant and field names are the
    /// vocabulary, values print locale-independently, and gettext is never
    /// consulted, which is what makes D2 hold by construction.
    ///
    /// Derived `Debug` is not a serialization format. This output is for
    /// humans reading `RUST_LOG=facelock=debug`; anything persisted (audit
    /// records above all) goes through serde, where the field names are a
    /// declared contract rather than an accident of the type definition.
    fn machine(&self) -> String {
        format!("{self:?}")
    }
}

// ---------------------------------------------------------------------------
// Sinks
// ---------------------------------------------------------------------------

/// Emit the machine rendering on the debug side channel. Every sink does this
/// before rendering for a human, so `RUST_LOG=facelock=debug` shows the stable
/// C-locale event stream next to whatever the user saw.
fn trace(msg: &dyn Message) {
    tracing::debug!(target: "facelock::message", "{}", msg.machine());
}

/// The human sink on this terminal. Localized text to stdout/stderr; the
/// same event goes to tracing at debug level in its machine rendering.
pub struct Terminal;

impl Terminal {
    /// Informational message to stdout.
    pub fn info(&self, msg: &dyn Message) {
        trace(msg);
        println!("{}", msg.localized());
    }

    /// Warning/error message to stderr (the process continues).
    pub fn error(&self, msg: &dyn Message) {
        trace(msg);
        eprintln!("{}", msg.localized());
    }

    /// Localized confirmation, defaulting to no.
    pub fn confirm(&self, msg: &dyn Message) -> anyhow::Result<bool> {
        trace(msg);
        eprint!("{}", prompt_line(msg, "[y/N]"));
        Ok(matches!(read_answer()?.as_str(), "y" | "yes"))
    }

    /// Localized confirmation, defaulting to yes.
    pub fn confirm_default_yes(&self, msg: &dyn Message) -> anyhow::Result<bool> {
        trace(msg);
        eprint!("{}", prompt_line(msg, "[Y/n]"));
        Ok(!matches!(read_answer()?.as_str(), "n" | "no"))
    }
}

/// The exact bytes of an inline prompt: the localized question, the English
/// answer hint, one trailing space, no newline.
fn prompt_line(msg: &dyn Message, hint: &str) -> String {
    format!("{} {hint} ", msg.localized())
}

/// Read one answer from stdin, trimmed and lowercased.
///
/// The hint (`[y/N]`, `[Y/n]`) is appended by the prompting method and the
/// tokens are matched here, so the two halves of the contract sit together.
/// Neither half may enter a catalog: the tokens below are English, so a
/// translated hint would advertise an answer this parser cannot accept.
///
/// This lives in the sink rather than in `ipc_client` — the prompt is written
/// here, so the answer is read here. `ipc_client` imports the message types,
/// so the seam calling back into it was a cycle waiting to bite.
fn read_answer() -> anyhow::Result<String> {
    use anyhow::Context;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("failed to read input")?;
    Ok(input.trim().to_lowercase())
}

/// Localized text for an `anyhow` context layer, traced like any other render.
///
/// `.context(explain(&msg))`, never `.context(msg.localized())`: every sink
/// emits the machine rendering next to the human one, and a context layer
/// that skipped it would show up on stderr while going missing from the
/// `RUST_LOG=facelock=debug` event stream.
pub fn explain(msg: &dyn Message) -> String {
    trace(msg);
    msg.localized()
}

/// An error whose `Display` is destined for a human on stderr (the CLI's
/// `anyhow` exit path) — so it localizes at birth. Never use this for errors
/// that cross the D-Bus wire or land in audit/syslog.
pub fn fail(msg: impl Message) -> anyhow::Error {
    trace(&msg);
    anyhow::anyhow!("{}", msg.localized())
}

// ---------------------------------------------------------------------------
// Test support: the sample walk that feeds the placeholder sweep
// ---------------------------------------------------------------------------

/// A walk over every variant of a domain, one constructed sample each.
///
/// Implementations write [`Samples::next_sample`] as a wildcard-free `match`,
/// which is what keeps the walk honest: a variant added without a sample does
/// not compile.
#[cfg(test)]
pub(crate) trait Samples: Sized {
    /// The first sample, in enum order.
    fn first_sample() -> Self;

    /// The sample after `self`, or `None` at the end of the vocabulary.
    fn next_sample(&self) -> Option<Self>;

    /// Every variant of this domain, exactly once, in enum order.
    fn samples() -> Vec<Self> {
        let mut out = vec![Self::first_sample()];
        while let Some(next) = out[out.len() - 1].next_sample() {
            out.push(next);
            assert!(out.len() < 10_000, "sample walk does not terminate");
        }
        out
    }
}

/// Placeholder-sized string for sample values.
#[cfg(test)]
fn sample_text(v: &str) -> String {
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In the test environment no facelock .mo is installed for the C locale,
    /// so `localized()` is the English fallback — these pins are exactly the
    /// bytes the historical inline strings produced (container tests grep
    /// some of them).
    #[test]
    fn english_fallback_is_byte_identical() {
        assert_eq!(
            FaceMessage::EnrollComplete {
                model_id: 3,
                count: 12,
                label: "2026-08-14-1".into()
            }
            .localized(),
            "\nFace enrolled successfully!\n  Model ID: 3\n  Embeddings: 12\n  Label: 2026-08-14-1"
        );
        assert_eq!(
            FaceMessage::TestNoMatch {
                similarity: 0.5,
                seconds: 1.234
            }
            .localized(),
            "No match (best: 0.50) after 1.2s"
        );
        assert_eq!(
            FaceMessage::TestMatchedModel {
                model_id: 7,
                label: "front".into(),
                similarity: 0.876,
                seconds: 0.5
            }
            .localized(),
            "Matched model #7 'front' (similarity: 0.88) in 0.50s"
        );
        assert_eq!(
            AccessMessage::NoConfigFile {
                path: "/etc/facelock/config.toml".into()
            }
            .localized(),
            "no config file at /etc/facelock/config.toml — run 'sudo facelock setup' to create one"
        );
        assert_eq!(
            AccessMessage::RootRequired {
                hint: "sudo facelock enroll".into()
            }
            .localized(),
            "Root required.\n  Run: sudo facelock enroll"
        );
        assert_eq!(
            FaceMessage::ClearedModels {
                count: 2,
                user: "alice".into()
            }
            .localized(),
            "Removed 2 face model(s) for user 'alice'."
        );
        assert_eq!(
            PamMessage::PamServiceAbsent {
                path: "/etc/pam.d/omarchy-lock-face".into()
            }
            .localized(),
            "PAM service file absent: /etc/pam.d/omarchy-lock-face. Nothing to remove."
        );
    }

    /// The answer hint is appended by the sink and the question alone is the
    /// msgid, so a translator cannot advertise an answer `read_answer` will
    /// not accept. The composed bytes are the ones these prompts always had.
    #[test]
    fn answer_hints_are_appended_not_translated() {
        assert_eq!(
            AccessMessage::SudoReexecPrompt.localized(),
            "Root required. Re-run with sudo?",
            "the [Y/n] hint must not be part of the msgid"
        );
        assert_eq!(
            prompt_line(&AccessMessage::SudoReexecPrompt, "[Y/n]"),
            "Root required. Re-run with sudo? [Y/n] "
        );
        assert_eq!(
            prompt_line(&FaceMessage::ConfirmRunSetupNow, "[y/N]"),
            "Run setup now? [y/N] "
        );
    }

    /// Every message in every domain renders with all placeholders
    /// substituted — a leftover `{name}` means a template/argument mismatch.
    ///
    /// The per-domain walks are compiler-checked for completeness; this list
    /// of domains is the one thing that is not, so a new module goes here.
    #[test]
    fn no_unfilled_placeholders() {
        fn sweep<M: Message + Samples>() {
            for msg in M::samples() {
                let rendered = msg.localized();
                assert!(
                    !rendered.contains('{') && !rendered.contains('}'),
                    "unfilled placeholder in {msg:?}: {rendered:?}"
                );
            }
        }
        sweep::<AccessMessage>();
        sweep::<FaceMessage>();
        sweep::<StatusMessage>();
        sweep::<NotifyMessage>();
        sweep::<SetupMessage>();
        sweep::<DeviceMessage>();
        sweep::<DownloadMessage>();
        sweep::<SystemMessage>();
        sweep::<PamMessage>();
    }

    /// The machine rendering names the variant and its data, and never goes
    /// through gettext — the D2 side of the seam.
    #[test]
    fn machine_rendering_is_the_debug_vocabulary() {
        let msg = FaceMessage::EnrollComplete {
            model_id: 3,
            count: 12,
            label: "x".into(),
        };
        let line = msg.machine();
        assert!(line.starts_with("EnrollComplete"), "{line}");
        assert!(line.contains("model_id: 3"), "{line}");
        assert!(line.contains("count: 12"), "{line}");
    }

    /// Unknown msgids fall back to the input string (no catalog installed).
    #[test]
    fn translate_falls_back_to_msgid() {
        // Via a binding so xgettext (which extracts literal `translate("...")`
        // calls) does not sweep this probe into the catalog.
        let probe = "facelock test msgid that no catalog contains";
        assert_eq!(translate(probe), probe);
    }

    #[test]
    fn fill_substitutes_named_placeholders() {
        assert_eq!(
            fill(
                "a {x} b {y} c {x}".to_string(),
                &[("x", "1".to_string()), ("y", "2".to_string())]
            ),
            "a 1 b 2 c 1"
        );
    }

    /// `fail` produces an error whose Display is the localized rendering.
    #[test]
    fn fail_renders_localized_display() {
        let err = fail(FaceMessage::ModelsRequired);
        assert_eq!(
            format!("{err}"),
            "Models required. Run `facelock setup` to download them."
        );
    }

    /// Real gettext lookup through a fixture catalog: build a minimal `.mo`
    /// in a tempdir, point the domain at it, and check that `localized()`
    /// translates while `machine()` does not — the D2 seam, exercised
    /// end to end.
    ///
    /// Locale plumbing is process-global, so this test is written to be
    /// harmless to concurrent tests: the fixture translates exactly one
    /// msgid ("Run setup now?"), which no other test pins in English, and
    /// everything is restored on exit. If the environment offers no usable
    /// non-C locale (LANGUAGE is ignored under "C"), the lookup assertions
    /// are skipped; the manual recipe in the module docs of `just pot`
    /// covers that case.
    #[test]
    fn mo_catalog_lookup_translates_localized_but_not_machine() {
        let dir = tempfile::tempdir().unwrap();
        let mo_dir = dir.path().join("xx/LC_MESSAGES");
        std::fs::create_dir_all(&mo_dir).unwrap();
        std::fs::write(
            mo_dir.join("facelock.mo"),
            build_mo(&[("Run setup now?", "XX-RUN-SETUP-NOW")]),
        )
        .unwrap();

        // A non-C locale for LC_MESSAGES, else glibc ignores LANGUAGE.
        //
        // SAFETY: setlocale is MT-Unsafe and cargo runs tests on parallel
        // threads, so this is the one call site that cannot appeal to being
        // single-threaded. `LOCALE_MUTATION` serializes locale-mutating tests
        // against each other; what remains is a window of a few lines against
        // plain dgettext calls, which is why the fixture translates a single
        // msgid no other test pins.
        let _locale = LOCALE_MUTATION.lock().unwrap_or_else(|e| e.into_inner());
        let original = unsafe { setlocale(libc::LC_MESSAGES, std::ptr::null()) };
        let original: CString = if original.is_null() {
            c"C".to_owned()
        } else {
            unsafe { CStr::from_ptr(original) }.to_owned()
        };
        // C.UTF-8 is still a C-family locale for gettext purposes: glibc
        // deliberately ignores LANGUAGE there even though setlocale accepts it.
        let usable = [c"en_US.UTF-8", c"en_US.utf8"]
            .into_iter()
            .find(|l| !unsafe { setlocale(libc::LC_MESSAGES, l.as_ptr()) }.is_null());
        if usable.is_none() {
            eprintln!("skipping .mo lookup assertions: no usable non-C locale in this environment");
            return;
        }

        // SAFETY: process-global env mutation in a test; restored below.
        unsafe { std::env::set_var("LANGUAGE", "xx") };
        let c_dir = CString::new(dir.path().to_str().unwrap()).unwrap();
        // A fresh binding also invalidates glibc's per-domain catalog cache.
        bindtextdomain(GETTEXT_DOMAIN.as_ptr(), c_dir.as_ptr());
        bind_textdomain_codeset(GETTEXT_DOMAIN.as_ptr(), c"UTF-8".as_ptr());

        let localized = FaceMessage::ConfirmRunSetupNow.localized();
        let machine = FaceMessage::ConfirmRunSetupNow.machine();
        // Binding keeps this probe msgid out of xgettext extraction.
        let absent_probe = "facelock msgid absent from the fixture catalog";
        let missing = translate(absent_probe);

        // Restore before asserting so a failure cannot leak state.
        unsafe { std::env::remove_var("LANGUAGE") };
        // SAFETY: as above, still holding `_locale`.
        unsafe { setlocale(libc::LC_MESSAGES, original.as_ptr()) };
        bindtextdomain(GETTEXT_DOMAIN.as_ptr(), c"/usr/share/locale".as_ptr());

        assert_eq!(localized, "XX-RUN-SETUP-NOW", "catalog hit must translate");
        assert_eq!(machine, "ConfirmRunSetupNow", "machine rendering must not");
        assert_eq!(
            missing, "facelock msgid absent from the fixture catalog",
            "catalog miss must fall back to the msgid"
        );
    }

    /// Held across every test that calls `setlocale`, so two of them cannot
    /// interleave their save/mutate/restore sequences.
    static LOCALE_MUTATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Minimal GNU MO encoder: header entry plus the given pairs.
    /// Entries must be sorted by msgid (binary-search format, no hash table).
    fn build_mo(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = vec![(
            Vec::new(),
            b"Content-Type: text/plain; charset=UTF-8\n".to_vec(),
        )];
        entries.extend(
            pairs
                .iter()
                .map(|(id, s)| (id.as_bytes().to_vec(), s.as_bytes().to_vec())),
        );
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let n = entries.len() as u32;
        let orig_table = 28u32;
        let trans_table = orig_table + 8 * n;
        let mut data_off = trans_table + 8 * n;

        // msgids first, then msgstrs, in table order.
        let texts: Vec<&[u8]> = entries
            .iter()
            .map(|(id, _)| id.as_slice())
            .chain(entries.iter().map(|(_, s)| s.as_slice()))
            .collect();
        let mut tables = Vec::new();
        let mut data = Vec::new();
        for text in texts {
            tables.extend((text.len() as u32).to_le_bytes());
            tables.extend(data_off.to_le_bytes());
            data.extend(text);
            data.push(0);
            data_off += text.len() as u32 + 1;
        }

        let mut mo = Vec::new();
        mo.extend(0x950412deu32.to_le_bytes()); // magic
        mo.extend(0u32.to_le_bytes()); // revision
        mo.extend(n.to_le_bytes());
        mo.extend(orig_table.to_le_bytes());
        mo.extend(trans_table.to_le_bytes());
        mo.extend(0u32.to_le_bytes()); // hash size
        mo.extend(0u32.to_le_bytes()); // hash offset
        mo.extend(tables);
        mo.extend(data);
        mo
    }
}
