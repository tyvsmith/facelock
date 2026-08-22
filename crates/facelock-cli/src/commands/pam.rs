//! `facelock pam add | remove | status` — the `/etc/pam.d` writer.
//!
//! This is the most dangerous text the CLI writes: a bad line in
//! `/etc/pam.d/sudo` costs a `sudo`, a bad line in `/etc/pam.d/system-auth`
//! costs the machine. The module is built around that.
//!
//! # Why a verb
//!
//! The writer used to be reachable only as `setup --pam [--service X]
//! [--remove] [--yes] [--if-present]` — two `requires = "pam"` modifiers
//! hanging off a flag, which is what a missing verb looks like. Four defects
//! came out of that shape, and they are one refactor rather than four:
//!
//! 1. **Flag-on-flag.** `--service`/`--remove` only meant anything with
//!    `--pam`, so the action lived in a flag and its object in another flag.
//! 2. **One service per process.** `--service` was an `Option<String>`, so a
//!    wrapper wanting three services ran three processes: three root checks,
//!    three module checks, three previews, three copies of the closing hint —
//!    and no atomicity, since a failure on the third left the first two
//!    written.
//! 3. **`--yes` meant two things.** It suppressed the per-file confirmation
//!    *and* unlocked [`SENSITIVE_SERVICES`], so there was no way to say
//!    "unattended, and still refuse `system-auth`". [`PamRequest::no_confirm`]
//!    and [`PamRequest::allow_sensitive`] are now separate, and
//!    **`--no-confirm` never implies `--allow-sensitive`**. The `setup --pam`
//!    alias exposes the same separation: `--yes` suppresses its prompt and
//!    `--allow-sensitive` authorizes a gated write.
//! 4. **A no-op was indistinguishable from an action.** The old writer
//!    returned `Ok(())` for *installed*, *already present* and *declined*
//!    alike, which is why integrations pre-grepped `/etc/pam.d/<service>` for
//!    `pam_facelock.so` — a shell reimplementation of
//!    [`is_facelock_pam_line`]. [`Outcome`] is that answer, and
//!    `pam status --json` is the probe that replaces the grep.
//!
//! # Two-phase, and what that does and does not promise
//!
//! [`plan_writes`] validates **every** requested service — name well-formed,
//! file present (subject to `--if-present`), sensitive-gate, and what the edit
//! would be — before [`apply_add`]/[`apply_remove`] touches anything. A
//! validation failure therefore writes nothing at all, which is what gives a
//! caller's `set -e` loop real all-or-nothing semantics for the failure mode
//! that actually happens: a typo'd or gated service name.
//!
//! It is **not** a transaction. A write-phase I/O error on service N — a full
//! disk, a read-only mount — leaves services 1..N-1 written. Those are
//! reported per service ([`Outcome::Failed`]) and the process exits non-zero;
//! the remaining services are still attempted, because each is an independent
//! file with its own backup and a half-reported plan is harder to recover from
//! than a fully-reported one. Each in-place edit has versioned prepared and
//! committed rollback state under `/var/lib/facelock/pam-backups`.
//!
//! **Continue and report is the policy on every entry point**, not just on the
//! verb: [`apply_all`] is the one phase-two loop, and the four entry points
//! differ only in how they *read* its rows — [`write_in`] as an exit code, the
//! three `setup --pam` aliases as a `Result` through [`first_failure`], once
//! every service has been attempted. The aliases used to stop at the first
//! failure, which made the paragraph above true of one caller and false of
//! three (invisibly, since `setup` passes one service at a time).
//!
//! # Confinement
//!
//! A service name is **one path component** ([`confined`]), rejected before
//! any I/O on every verb. `base.join(service)` is not a confinement
//! primitive: an absolute `service` *replaces* `base` outright.
//!
//! A well-formed name still has to survive the *filesystem*. Every PAM root
//! and service basename is opened directory-relative with `O_NOFOLLOW`; all
//! symlinks and multiply-linked files are refused. Phase one captures the
//! opened inode and hash, and phase two compares both immediately before the
//! atomic rename, then fsyncs the new file and its parent directory.
//!
//! # Vendor directories
//!
//! A service name is not a file in one directory. Linux-PAM reads `/etc/pam.d`
//! first and a compile-time *vendor* directory second — `/usr/lib/pam.d` on
//! every distribution that enables the feature — and packages have moved their
//! configuration there: on current Arch, `polkit` ships
//! `/usr/lib/pam.d/polkit-1` and `/etc/pam.d/polkit-1` does not exist. So
//! [`PamDirs`] is an ordered search path, [`Target::locate`] takes the whole of
//! it, and the first directory holding the name wins.
//!
//! **Only the first directory is ever written to.** The rest are package-owned:
//! an edit there is clobbered by the next upgrade and makes `pacman -Qkk`
//! report a modified file. A service that resolves only in a vendor directory
//! is *copied* into `/etc/pam.d` with the facelock line already in it — one
//! atomic write of the final content — and the copy carries a provenance
//! header saying what it was forked from. That is what the `/etc` layer is
//! for.
//!
//! # Enumeration
//!
//! `status` answers about the services it is *given*, which leaves the same
//! blind spot from the other side: it will report `sudo` fine while a
//! configured `polkit-1` or `omarchy-lock-face` is broken, because nobody
//! named it. `pam status --all` is the enumerating form — [`scan_directories`]
//! walks the resolved directories for files that name `pam_facelock.so` and
//! feeds the names it finds to the same [`status_reports`] the explicit form
//! uses, so the two cannot answer differently about one service.
//!
//! **It parses; it keeps no manifest.** A state file listing what facelock has
//! edited drifts the moment anyone edits `/etc/pam.d` by hand, restores a
//! backup, or removes a package — and then `status` reports fiction with
//! confidence. A directory scan is ground truth.
//!
//! **A directory that could not be listed is reported, not treated as empty.**
//! "Nothing is configured here" and "I could not look here" are different
//! answers, and rendering them the same is what made a broken lock stack and a
//! healthy one look identical. See [`DirState`].
//!
//! # Limits
//!
//! `remove` takes no backup of its own. It removes validated committed
//! Facelock-owned state and the exact legacy adjacent backup name by default;
//! `--keep-backup` preserves both.
//!
//! A replace writes a new inode, so it carries what it is told to carry: mode,
//! owner and the SELinux label. **POSIX ACLs and every other xattr are not
//! carried** — a `setfacl`'d service file loses its ACL on the first `pam
//! add`. No distribution ships one, and reconstructing an arbitrary ACL is not
//! something this should be guessing at, so it is written down rather than
//! attempted.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use dialoguer::Confirm;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::message::{Message, PamMessage, Terminal, fail};
use crate::state_layout::{PAM_BACKUPS_DIR, PAM_BACKUPS_DIR_MODE};

/// The PAM configuration directory every write lands in — the *first* entry of
/// the search path, and the only one this module ever modifies.
///
/// The search path itself is [`PamDirs`], which every engine function takes as
/// a parameter so tests drive the whole writer against a tempdir,
/// unprivileged.
pub const PAM_DIR: &str = "/etc/pam.d";

/// Compiled PAM roots used for machine-wide discovery and package cleanup.
///
/// Unlike the explicit named writer's configurable search path, this list is
/// deliberately independent of `config.toml`: uninstall must find Facelock
/// references even when configuration cannot be loaded.
pub const PAM_SYSTEM_DIRS: &[&str] = &[PAM_DIR, "/usr/lib/pam.d"];

/// Fixed roots scanned by config-independent package cleanup.
///
/// Fedora's `/etc/pam.d/{system,password,...}-auth` entries are generated
/// symlinks into `/etc/authselect`. Cleanup never follows those links; it
/// scans the generated directory independently so an active reference there
/// is still an unmanaged blocker while an unrelated stock link is harmless.
const PAM_CLEANUP_DIRS: &[&str] = &[PAM_DIR, "/usr/lib/pam.d", "/etc/authselect"];

/// The line this command adds and removes. Matching is by module name rather
/// than by these bytes — see [`is_facelock_pam_line`].
pub const PAM_LINE: &str = "auth      sufficient pam_facelock.so";

/// Where the PAM module may be installed, in probe order — first hit wins.
///
/// A list rather than one path because this repository **ships the packaging
/// that puts it elsewhere**: `dist/facelock.spec` installs to
/// `%{_libdir}/security/`, which is `/usr/lib64/security` on x86-64 Fedora and
/// RHEL, so on the distribution whose spec file is in this tree a single
/// hardcoded `/lib/security` made `pam add` refuse with "module not installed"
/// while the module was installed (#170).
///
/// `/lib/security` stays first so the answer on usrmerged Arch — where it is
/// the same file as `/usr/lib/security/pam_facelock.so` — does not change.
/// There is deliberately **no** Debian multiarch triple
/// (`/usr/lib/<triple>/security`): Debian's idiomatic path is
/// `pam-auth-update` rather than a hand-inserted line, which this command does
/// not do, so a probe path for it would suggest support that is not there.
///
/// This is the one list. `crate::health` reads it rather than keeping a second
/// copy, because two lists that drift is the bug class this closes.
pub const PAM_MODULE_PATHS: &[&str] = &[
    "/lib/security/pam_facelock.so",
    "/usr/lib/security/pam_facelock.so",
    "/usr/lib64/security/pam_facelock.so",
];

/// Services whose stacks can lock the machine, or the network, out. Adding
/// face auth here needs `--allow-sensitive` on every CLI surface;
/// **removing** it needs no gate on sensitivity, because removal can only take
/// away a way to authenticate — the confinement rules still apply to it, and
/// to `status`, like any other verb.
///
/// Six of the eight are *shared stacks* — files other service files `include`,
/// so an edit here reaches `su`, `passwd`, `chsh` and the display manager at
/// once — and which name a distribution uses is the only difference between
/// them: `system-auth` and `password-auth` on Fedora/RHEL and Arch,
/// `system-auth-ac` and `password-auth-ac` where RHEL's older `authconfig`
/// wrote the real file and left the unsuffixed name pointing at it,
/// `common-auth` on Debian and Ubuntu, `system-login` on Arch. Gating only the
/// Arch spelling made the gate a coin flip on the operator's distro. `login`
/// (TTY login) and `sshd` (the network path) are the two that lock you out of
/// a specific door rather than all of them.
///
/// The list is matched against the name as typed **and** against the file that
/// name resolves to ([`gate_sensitive`]), because a symlink inside the
/// directory is otherwise an ungated name for a gated file — which is exactly
/// the shape `authconfig` leaves behind.
///
/// Every name here is also in each packager's `FACELOCK_PAM_SERVICES`, pinned
/// by a test, so an uninstall strips a line this gate let through.
pub const SENSITIVE_SERVICES: &[&str] = &[
    "common-auth",
    "login",
    "password-auth",
    "password-auth-ac",
    "sshd",
    "system-auth",
    "system-auth-ac",
    "system-login",
];

/// The service a bare `pam add` / `setup --pam` means.
pub const DEFAULT_PAM_SERVICE: &str = "sudo";

/// Exact suffix of legacy adjacent backups that `remove` still recognizes.
const BACKUP_SUFFIX: &str = ".facelock-backup";

/// On-disk provenance schema understood by this binary.
const PROVENANCE_VERSION: u32 = 1;

/// Bound collision probing so a corrupt or hostile state directory cannot
/// turn backup planning into an unbounded loop.
const MAX_TIMESTAMP_COLLISION_PROBES: usize = 4096;

/// Provenance is an untrusted hint. Cap it before allocation or parsing.
const MAX_RECORD_BYTES: usize = 16 * 1024;

/// PAM rollback bytes are deliberately generous but finite.
const MAX_BACKUP_BYTES: usize = 1024 * 1024;

/// Whole-machine cleanup journals are path-free and bounded before parsing.
const REMOVE_ALL_VERSION: u32 = 2;
const REMOVE_ALL_LEGACY_VERSION: u32 = 1;
const MAX_REMOVE_ALL_TARGETS: usize = 1024;
const MAX_REMOVE_ALL_JOURNAL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProvenanceState {
    Prepared,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IntentRole {
    Prepare,
    Commit,
    Cleanup,
    PamReplace,
    PamRemove,
    VendorCreate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PublicationRole {
    Commit,
    PamReplace,
    PamRemove,
    VendorCreate,
}

impl PublicationRole {
    fn intent_role(self) -> IntentRole {
        match self {
            Self::Commit => IntentRole::Commit,
            Self::PamReplace => IntentRole::PamReplace,
            Self::PamRemove => IntentRole::PamRemove,
            Self::VendorCreate => IntentRole::VendorCreate,
        }
    }
}

/// A durable, path-free declaration of one mutation. Reserved intent,
/// quarantine, and temporary basenames are Facelock-owned only when this
/// strict schema and every derived hash/name relationship validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateIntent {
    version: u32,
    role: IntentRole,
    sequence: u64,
    service: String,
    backup: String,
    original_sha256: String,
    installed_sha256: String,
    record_sha256: Option<String>,
    replacement_record_sha256: Option<String>,
    original_device: Option<u64>,
    original_inode: Option<u64>,
    original_links: Option<u64>,
    original_mode: Option<u32>,
    original_uid: Option<u32>,
    original_gid: Option<u32>,
}

/// Full identity of the inode Facelock prepared for publication. This remains
/// after the base intent is removed so a crash in final cleanup can still
/// authenticate the canonical name without falling back to bytes or shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationBinding {
    version: u32,
    role: PublicationRole,
    sequence: u64,
    service: String,
    backup: String,
    intent_sha256: String,
    device: u64,
    inode: u64,
    links: u64,
    sha256: String,
    mode: u32,
    uid: u32,
    gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrepareCrashPoint {
    Intent,
    Backup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitCrashPoint {
    Intent,
    ReplacementTemp,
    Exchange,
    DisplacedUnlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupCrashPoint {
    Intent,
    BackupQuarantine,
    RecordQuarantine,
    BackupUnlink,
    RecordUnlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoveAllRollbackPoint {
    ReverseExchange,
    TempUnlink,
    BindingUnlink,
    IntentUnlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PamReplaceCrashPoint {
    Intent,
    ReplacementTemp,
    Exchange,
    Finalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PamRemoveCrashPoint {
    Intent,
    ReplacementTemp,
    Exchange,
    Finalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VendorCreateCrashPoint {
    Intent,
    ReplacementTemp,
    Publish,
    Finalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationCleanupPoint {
    IntentUnlink,
    BindingUnlink,
}

/// A provenance record deliberately contains no target path. The service is
/// resolved afresh under the active PAM roots whenever the record is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvenanceRecord {
    version: u32,
    sequence: u64,
    state: ProvenanceState,
    service: String,
    backup: String,
    original_sha256: String,
    installed_sha256: String,
}

#[derive(Debug, Clone)]
struct PreparedBackup {
    root: PathBuf,
    backup: String,
    record: String,
    provenance: ProvenanceRecord,
    backup_identity: Option<FileIdentity>,
    record_identity: Option<FileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveAllJournalTarget {
    service: String,
    backup: String,
    original: FileIdentity,
    installed_sha256: String,
    #[serde(default, deserialize_with = "deserialize_delete_override")]
    delete_override: Option<bool>,
}

fn deserialize_delete_override<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    bool::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveAllJournal {
    version: u32,
    operation: String,
    keep_backup: bool,
    targets: Vec<RemoveAllJournalTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveAllCommittedTarget {
    service: String,
    backup: String,
    installed: FileIdentity,
    #[serde(default, deserialize_with = "deserialize_delete_override")]
    delete_override: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveAllCommit {
    version: u32,
    operation: String,
    journal_sha256: String,
    keep_backup: bool,
    targets: Vec<RemoveAllCommittedTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoveAllPoint {
    Locked,
    Journaled,
    BeforeMutation(usize),
    AfterMutation(usize),
    CommitMarked,
    BeforeOverrideDelete(usize),
    OverrideQuarantined(usize),
    BeforeOverrideFinalValidation(usize),
    OverrideRestored(usize),
    AfterOverrideDelete(usize),
    JournalUnlinked,
    CommitUnlinked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VendorRetirePoint {
    Quarantined,
    BeforeFinalValidation,
    Restored,
    Unlinked,
}

#[derive(Debug, Clone)]
struct PamMutationPlan {
    root: PathBuf,
    operation: String,
    sequence: u64,
    service: String,
    original_sha256: String,
    installed_sha256: String,
}

impl PreparedBackup {
    #[cfg(test)]
    fn backup_name(&self) -> &str {
        &self.backup
    }

    fn backup_path(&self) -> PathBuf {
        self.root.join(&self.backup)
    }

    #[cfg(test)]
    fn record_path(&self) -> PathBuf {
        self.root.join(&self.record)
    }
}

fn intent_name(role: IntentRole, backup: &str) -> String {
    let role = match role {
        IntentRole::Prepare => "prepare",
        IntentRole::Commit => "commit",
        IntentRole::Cleanup => "cleanup",
        IntentRole::PamReplace => "pam-replace",
        IntentRole::PamRemove => "pam-remove",
        IntentRole::VendorCreate => "vendor-create",
    };
    format!(".facelock-intent-{role}-{backup}.json")
}

fn quarantine_name(role: &str, backup: &str) -> String {
    format!(".facelock-quarantine-{role}-{backup}")
}

fn pam_replace_name(backup: &str) -> String {
    format!(".facelock-pam-replace-{backup}")
}

fn pam_remove_name(operation: &str) -> String {
    format!(".facelock-pam-remove-{operation}")
}

fn vendor_retire_name(operation: &str) -> String {
    format!(".facelock-vendor-retire-{operation}")
}

fn vendor_create_name(operation: &str) -> String {
    format!(".facelock-vendor-create-{operation}")
}

fn publication_name(role: PublicationRole, backup: &str) -> String {
    let role = match role {
        PublicationRole::Commit => "commit",
        PublicationRole::PamReplace => "pam-replace",
        PublicationRole::PamRemove => "pam-remove",
        PublicationRole::VendorCreate => "vendor-create",
    };
    format!(".facelock-publication-{role}-{backup}.json")
}

#[derive(Debug, Clone)]
struct BackupStore {
    root: PathBuf,
    expected_owner: (u32, u32),
}

fn expected_state_owner(root: &Path) -> (u32, u32) {
    if root == Path::new(PAM_BACKUPS_DIR) {
        (0, 0)
    } else {
        // Non-system roots exist only for explicitly injected setup/test
        // layouts. Bind them to the invoking identity rather than trusting
        // whatever owner the directory happens to report.
        (unsafe { libc::geteuid() }, unsafe { libc::getegid() })
    }
}

fn state_directory_attributes_match(
    mode: u32,
    uid: u32,
    gid: u32,
    expected_owner: (u32, u32),
) -> bool {
    mode & libc::S_IFMT == libc::S_IFDIR
        && mode & 0o7777 == PAM_BACKUPS_DIR_MODE
        && (uid, gid) == expected_owner
}

fn secure_state_directory(directory: &fs::File, expected_owner: (u32, u32)) -> std::io::Result<()> {
    let metadata = directory.metadata()?;
    if !state_directory_attributes_match(
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
        expected_owner,
    ) {
        apply_owner_then_mode(
            directory,
            expected_owner.0,
            expected_owner.1,
            PAM_BACKUPS_DIR_MODE,
        )?;
    }
    let metadata = directory.metadata()?;
    if !state_directory_attributes_match(
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
        expected_owner,
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "PAM backup state directory owner or mode is not trusted",
        ));
    }
    directory.sync_all()
}

fn create_state_directory(root: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(PAM_BACKUPS_DIR_MODE);
    builder.create(root)
}

/// One complete mutation of PAM configuration and its rollback state.
/// Holding the state-directory flock in this guard prevents recovery or a
/// competing writer from observing a prepared pair between publication and
/// commit.
struct BackupTransaction<'a> {
    store: &'a BackupStore,
    _lock: fs::File,
}

impl BackupTransaction<'_> {
    fn plan(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
    ) -> std::io::Result<PreparedBackup> {
        use std::io::{Error, ErrorKind};

        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        self.plan_at(service, original, installed, since_epoch)
    }

    fn plan_at(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
        since_epoch: std::time::Duration,
    ) -> std::io::Result<PreparedBackup> {
        self.store
            .plan_at_unlocked(service, original, installed, since_epoch)
    }

    fn persist(&self, prepared: &PreparedBackup, original: &[u8]) -> std::io::Result<()> {
        self.store
            .persist_unlocked_with_hook(prepared, original, |_| Ok(()))
    }

    fn plan_mutation(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
    ) -> std::io::Result<PamMutationPlan> {
        let prepared = self.plan(service, original, installed)?;
        Ok(PamMutationPlan {
            root: prepared.root,
            operation: prepared.backup,
            sequence: prepared.provenance.sequence,
            service: prepared.provenance.service,
            original_sha256: prepared.provenance.original_sha256,
            installed_sha256: prepared.provenance.installed_sha256,
        })
    }

    fn replace_pam_with_intent(
        &self,
        prepared: &PreparedBackup,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
    ) -> std::io::Result<()> {
        self.store
            .replace_pam_with_intent_unlocked_hook(prepared, path, expected, content, |_| Ok(()))
    }

    fn replace_pam_with_intent_hook(
        &self,
        prepared: &PreparedBackup,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(PamReplaceCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.store.replace_pam_with_intent_unlocked_hook(
            prepared,
            path,
            expected,
            content,
            after_boundary,
        )
    }

    fn commit(&self, prepared: &PreparedBackup) -> std::io::Result<()> {
        self.store.commit_unlocked(prepared, |_| Ok(()))
    }

    fn remove_pam_with_intent_hook(
        &self,
        mutation: &PamMutationPlan,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(PamRemoveCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.store.remove_pam_with_intent_unlocked_hook(
            mutation,
            path,
            expected,
            content,
            after_boundary,
        )?;
        Ok(())
    }

    fn remove_pam_with_intent(
        &self,
        mutation: &PamMutationPlan,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
    ) -> std::io::Result<()> {
        self.remove_pam_with_intent_hook(mutation, path, expected, content, |_| Ok(()))
    }

    fn remove_pam_with_intent_and_published_hook(
        &self,
        mutation: &PamMutationPlan,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_published: impl FnMut(&FileIdentity) -> std::io::Result<()>,
    ) -> std::io::Result<FileIdentity> {
        self.store.remove_pam_with_intent_unlocked_published_hook(
            mutation,
            path,
            expected,
            content,
            |_| Ok(()),
            after_published,
        )
    }

    fn create_vendor_with_intent_hook(
        &self,
        mutation: &PamMutationPlan,
        target: &Target,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(VendorCreateCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.store.create_vendor_with_intent_unlocked_hook(
            mutation,
            target,
            expected,
            content,
            after_boundary,
        )
    }

    fn create_vendor_with_intent(
        &self,
        mutation: &PamMutationPlan,
        target: &Target,
        expected: &FileIdentity,
        content: &[u8],
    ) -> std::io::Result<()> {
        self.create_vendor_with_intent_hook(mutation, target, expected, content, |_| Ok(()))
    }
}

#[derive(Debug)]
struct ValidIntent {
    name: String,
    intent: StateIntent,
    identity: FileIdentity,
}

#[derive(Debug)]
struct ValidPublication {
    name: String,
    binding: PublicationBinding,
    identity: FileIdentity,
}

#[derive(Debug)]
struct CreatedPublication {
    name: String,
    state_identity: FileIdentity,
    binding: PublicationBinding,
}

impl BackupStore {
    fn open(root: &Path) -> std::io::Result<Self> {
        use std::io::{Error, ErrorKind};

        match fs::symlink_metadata(root) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} is not a directory", root.display()),
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => create_state_directory(root)?,
            Err(error) => return Err(error),
        }

        let expected_owner = expected_state_owner(root);
        let handle = open_directory_nofollow(root)?;
        secure_state_directory(&handle, expected_owner)?;

        Ok(Self {
            root: root.to_path_buf(),
            expected_owner,
        })
    }

    fn create_publication_binding(
        &self,
        role: PublicationRole,
        intent: &StateIntent,
        intent_encoded: &[u8],
        replacement: &FileIdentity,
    ) -> std::io::Result<CreatedPublication> {
        let binding = publication_binding_for(role, intent, intent_encoded, replacement);
        let name = publication_name(role, &intent.backup);
        let encoded = serde_json::to_vec_pretty(&binding)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let state_identity = atomic_state_create(&self.root, &name, &encoded)?;
        Ok(CreatedPublication {
            name,
            state_identity,
            binding,
        })
    }

    fn finish_publication_state(
        &self,
        intent_name: &str,
        intent_identity: &FileIdentity,
        publication: Option<&CreatedPublication>,
    ) -> std::io::Result<()> {
        self.finish_publication_state_with_hook(intent_name, intent_identity, publication, |_| {
            Ok(())
        })
    }

    fn finish_publication_state_with_hook(
        &self,
        intent_name: &str,
        intent_identity: &FileIdentity,
        publication: Option<&CreatedPublication>,
        mut after_boundary: impl FnMut(PublicationCleanupPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        // The full publication identity is the last durable object removed.
        // If the process stops between these two unlinks, recovery can still
        // authenticate the canonical inode from the self-contained binding.
        unlink_state_if_identity_matches(&self.root, intent_name, intent_identity)?;
        after_boundary(PublicationCleanupPoint::IntentUnlink)?;
        if let Some(publication) = publication {
            unlink_state_if_identity_matches(
                &self.root,
                &publication.name,
                &publication.state_identity,
            )?;
            after_boundary(PublicationCleanupPoint::BindingUnlink)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn prepare(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
    ) -> std::io::Result<PreparedBackup> {
        let prepared = self.plan(service, original, installed)?;
        self.persist(&prepared, original)?;
        Ok(prepared)
    }

    #[cfg(test)]
    fn plan(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
    ) -> std::io::Result<PreparedBackup> {
        use std::io::{Error, ErrorKind};

        if confined(service).is_err() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid PAM service name",
            ));
        }
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        self.plan_at(service, original, installed, since_epoch)
    }

    #[cfg(test)]
    fn plan_at(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
        since_epoch: std::time::Duration,
    ) -> std::io::Result<PreparedBackup> {
        let _lock = self.lock_exclusive()?;
        self.plan_at_unlocked(service, original, installed, since_epoch)
    }

    fn plan_at_unlocked(
        &self,
        service: &str,
        original: &[u8],
        installed: &[u8],
        mut since_epoch: std::time::Duration,
    ) -> std::io::Result<PreparedBackup> {
        use std::io::{Error, ErrorKind};

        if confined(service).is_err() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid PAM service name",
            ));
        }
        let sequence = self.next_sequence()?;
        let backup = (0..MAX_TIMESTAMP_COLLISION_PROBES)
            .find_map(|_| {
                let candidate = format!(
                    "{service}.{}-{:09}",
                    since_epoch.as_secs(),
                    since_epoch.subsec_nanos()
                );
                let record = format!("{candidate}.json");
                let occupied = fs::symlink_metadata(self.root.join(&candidate)).is_ok()
                    || fs::symlink_metadata(self.root.join(record)).is_ok();
                if !occupied {
                    return Some(candidate);
                }
                since_epoch = since_epoch.checked_add(std::time::Duration::from_nanos(1))?;
                None
            })
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::AlreadyExists,
                    "PAM backup timestamp collision probe limit exhausted",
                )
            })?;
        let record = format!("{backup}.json");
        let provenance = ProvenanceRecord {
            version: PROVENANCE_VERSION,
            sequence,
            state: ProvenanceState::Prepared,
            service: service.to_string(),
            backup: backup.clone(),
            original_sha256: sha256_hex(original),
            installed_sha256: sha256_hex(installed),
        };

        Ok(PreparedBackup {
            root: self.root.clone(),
            backup,
            record,
            provenance,
            backup_identity: None,
            record_identity: None,
        })
    }

    #[cfg(test)]
    fn persist(&self, prepared: &PreparedBackup, original: &[u8]) -> std::io::Result<()> {
        self.persist_with_hook(prepared, original, |_| Ok(()))
    }

    #[cfg(test)]
    fn persist_with_hook(
        &self,
        prepared: &PreparedBackup,
        original: &[u8],
        after_boundary: impl FnMut(PrepareCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.persist_unlocked_with_hook(prepared, original, after_boundary)
    }

    fn persist_unlocked_with_hook(
        &self,
        prepared: &PreparedBackup,
        original: &[u8],
        mut after_boundary: impl FnMut(PrepareCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};

        if prepared.root != self.root || sha256_hex(original) != prepared.provenance.original_sha256
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "prepared backup does not match this store or original",
            ));
        }
        if original.len() > MAX_BACKUP_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "PAM backup exceeds the state size limit",
            ));
        }
        if self.sequence_in_use(prepared.provenance.sequence)? {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "PAM backup sequence was allocated concurrently",
            ));
        }
        let encoded = serde_json::to_vec_pretty(&prepared.provenance)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        if encoded.len() > MAX_RECORD_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "PAM provenance exceeds the state size limit",
            ));
        }
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::Prepare,
            sequence: prepared.provenance.sequence,
            service: prepared.provenance.service.clone(),
            backup: prepared.backup.clone(),
            original_sha256: prepared.provenance.original_sha256.clone(),
            installed_sha256: prepared.provenance.installed_sha256.clone(),
            record_sha256: Some(sha256_hex(&encoded)),
            replacement_record_sha256: None,
            original_device: None,
            original_inode: None,
            original_links: None,
            original_mode: None,
            original_uid: None,
            original_gid: None,
        };
        let intent_name = intent_name(IntentRole::Prepare, &prepared.backup);
        let intent_encoded = serde_json::to_vec_pretty(&intent)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent_identity = atomic_state_create(&self.root, &intent_name, &intent_encoded)?;
        after_boundary(PrepareCrashPoint::Intent)?;

        let backup_identity = match atomic_state_create(&self.root, &prepared.backup, original) {
            Ok(identity) => identity,
            Err(error) => {
                if !is_ambiguous_publication(&error) {
                    let _ = unlink_state_if_identity_matches(
                        &self.root,
                        &intent_name,
                        &intent_identity,
                    );
                }
                return Err(error);
            }
        };
        after_boundary(PrepareCrashPoint::Backup)?;
        if let Err(error) = atomic_state_create(&self.root, &prepared.record, &encoded) {
            if is_ambiguous_publication(&error) {
                return Err(error);
            }
            let _ =
                unlink_state_if_identity_matches(&self.root, &prepared.backup, &backup_identity);
            let _ = unlink_state_if_identity_matches(&self.root, &intent_name, &intent_identity);
            let _ = sync_directory(&self.root);
            return Err(error);
        }
        unlink_state_if_identity_matches(&self.root, &intent_name, &intent_identity)?;
        Ok(())
    }

    #[cfg(test)]
    fn commit(&self, prepared: &PreparedBackup) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.commit_unlocked(prepared, |_| Ok(()))
    }

    #[cfg(test)]
    fn commit_with_hook(
        &self,
        prepared: &PreparedBackup,
        after_boundary: impl FnMut(CommitCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.commit_unlocked(prepared, after_boundary)
    }

    fn commit_unlocked(
        &self,
        prepared: &PreparedBackup,
        mut after_boundary: impl FnMut(CommitCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};

        if prepared.root != self.root {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "prepared backup belongs to another store",
            ));
        }
        let mut provenance = prepared.provenance.clone();
        provenance.state = ProvenanceState::Committed;
        let replacement = serde_json::to_vec_pretty(&provenance)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let prepared_encoded = serde_json::to_vec_pretty(&prepared.provenance)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::Commit,
            sequence: prepared.provenance.sequence,
            service: prepared.provenance.service.clone(),
            backup: prepared.backup.clone(),
            original_sha256: prepared.provenance.original_sha256.clone(),
            installed_sha256: prepared.provenance.installed_sha256.clone(),
            record_sha256: Some(sha256_hex(&prepared_encoded)),
            replacement_record_sha256: Some(sha256_hex(&replacement)),
            original_device: None,
            original_inode: None,
            original_links: None,
            original_mode: None,
            original_uid: None,
            original_gid: None,
        };
        let intent_name = intent_name(IntentRole::Commit, &prepared.backup);
        let intent_encoded = serde_json::to_vec_pretty(&intent)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent_identity = atomic_state_create(&self.root, &intent_name, &intent_encoded)?;
        after_boundary(CommitCrashPoint::Intent)?;

        let directory = open_directory_nofollow(&self.root)?;
        let current = open_regular_at(&directory, OsStr::new(&prepared.record))?;
        let current_identity = identity_of_open_bounded(&current, MAX_RECORD_BYTES)?;
        if !state_identity_matches(
            self.expected_owner,
            &current_identity,
            intent.record_sha256.as_deref().unwrap_or_default(),
        ) {
            let _ = unlink_state_if_identity_matches(&self.root, &intent_name, &intent_identity);
            return Err(Error::other("prepared provenance changed before commit"));
        }
        let exchange_name = format!("{}.json", quarantine_name("commit", &prepared.backup));
        let replacement_identity =
            match atomic_state_create(&self.root, &exchange_name, &replacement) {
                Ok(identity) => identity,
                Err(error) => {
                    if is_ambiguous_publication(&error) {
                        return Err(error);
                    }
                    let _ = unlink_state_if_identity_matches(
                        &self.root,
                        &intent_name,
                        &intent_identity,
                    );
                    return Err(error);
                }
            };
        let publication = match self.create_publication_binding(
            PublicationRole::Commit,
            &intent,
            &intent_encoded,
            &replacement_identity,
        ) {
            Ok(publication) => publication,
            Err(error) => {
                if is_ambiguous_publication(&error) {
                    return Err(error);
                }
                cleanup_unpublished_temp(
                    &self.root,
                    &exchange_name,
                    &replacement_identity,
                    MAX_RECORD_BYTES,
                    "unpublished committed provenance became ambiguous",
                )?;
                unlink_state_if_identity_matches(&self.root, &intent_name, &intent_identity)?;
                return Err(error);
            }
        };
        after_boundary(CommitCrashPoint::ReplacementTemp)?;
        rename_exchange_at(&directory, &exchange_name, &prepared.record)?;
        after_boundary(CommitCrashPoint::Exchange)?;

        let published = open_regular_at(&directory, OsStr::new(&prepared.record))
            .and_then(|file| identity_of_open_bounded(&file, MAX_RECORD_BYTES))
            .map_err(|_| ambiguous_publication("committed provenance became ambiguous"))?;
        if !identity_matches(&replacement_identity, &published) {
            return Err(ambiguous_publication(
                "committed provenance changed before final validation",
            ));
        }

        let displaced = open_regular_at(&directory, OsStr::new(&exchange_name))?;
        let displaced_identity = identity_of_open_bounded(&displaced, MAX_RECORD_BYTES)?;
        if !identity_matches(&current_identity, &displaced_identity) {
            rename_exchange_at(&directory, &exchange_name, &prepared.record)?;
            if let Ok(restored) = open_regular_at(&directory, OsStr::new(&exchange_name))
                && identity_matches(
                    &replacement_identity,
                    &identity_of_open_bounded(&restored, MAX_RECORD_BYTES)?,
                )
            {
                unlink_state_if_identity_matches(
                    &self.root,
                    &exchange_name,
                    &replacement_identity,
                )?;
            }
            let _ =
                self.finish_publication_state(&intent_name, &intent_identity, Some(&publication));
            return Err(Error::other("prepared provenance changed during commit"));
        }
        let published = open_regular_at(&directory, OsStr::new(&prepared.record))
            .and_then(|file| identity_of_open_bounded(&file, MAX_RECORD_BYTES))
            .map_err(|_| ambiguous_publication("committed provenance became ambiguous"))?;
        if !identity_matches(&replacement_identity, &published) {
            return Err(ambiguous_publication(
                "committed provenance changed before displaced cleanup",
            ));
        }
        unlink_state_if_identity_matches(&self.root, &exchange_name, &displaced_identity)?;
        after_boundary(CommitCrashPoint::DisplacedUnlink)?;
        let published = open_regular_at(&directory, OsStr::new(&prepared.record))
            .and_then(|file| identity_of_open_bounded(&file, MAX_RECORD_BYTES))
            .map_err(|_| ambiguous_publication("committed provenance became ambiguous"))?;
        if !identity_matches(&replacement_identity, &published) {
            return Err(ambiguous_publication(
                "committed provenance changed before intent cleanup",
            ));
        }
        self.finish_publication_state(&intent_name, &intent_identity, Some(&publication))
    }

    #[cfg(test)]
    fn replace_pam_with_intent_hook(
        &self,
        prepared: &PreparedBackup,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(PamReplaceCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.replace_pam_with_intent_unlocked_hook(
            prepared,
            path,
            expected,
            content,
            after_boundary,
        )
    }

    fn replace_pam_with_intent_unlocked_hook(
        &self,
        prepared: &PreparedBackup,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(PamReplaceCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use std::cell::RefCell;
        use std::io::{Error, ErrorKind};

        if prepared.root != self.root
            || expected.sha256 != prepared.provenance.original_sha256
            || sha256_hex(content) != prepared.provenance.installed_sha256
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "PAM replacement does not match prepared provenance",
            ));
        }
        let prepared_encoded = serde_json::to_vec_pretty(&prepared.provenance)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::PamReplace,
            sequence: prepared.provenance.sequence,
            service: prepared.provenance.service.clone(),
            backup: prepared.backup.clone(),
            original_sha256: expected.sha256.clone(),
            installed_sha256: sha256_hex(content),
            record_sha256: Some(sha256_hex(&prepared_encoded)),
            replacement_record_sha256: None,
            original_device: Some(expected.device),
            original_inode: Some(expected.inode),
            original_links: Some(expected.links),
            original_mode: Some(expected.mode),
            original_uid: Some(expected.uid),
            original_gid: Some(expected.gid),
        };
        let intent_name = intent_name(IntentRole::PamReplace, &prepared.backup);
        let intent_encoded = serde_json::to_vec_pretty(&intent)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent_identity = atomic_state_create(&self.root, &intent_name, &intent_encoded)?;
        let hook = RefCell::new(after_boundary);
        let publication = RefCell::new(None);
        hook.borrow_mut()(PamReplaceCrashPoint::Intent)?;
        let temp_name = pam_replace_name(&prepared.backup);
        let mut result = replace_existing_verified_with_hooks(
            path,
            expected,
            content,
            Some(&temp_name),
            |replacement| {
                let created = match self.create_publication_binding(
                    PublicationRole::PamReplace,
                    &intent,
                    &intent_encoded,
                    replacement,
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        if is_ambiguous_publication(&error) {
                            return Err(error);
                        }
                        cleanup_unpublished_temp_below(
                            path,
                            &temp_name,
                            replacement,
                            MAX_BACKUP_BYTES,
                            "unpublished PAM replacement temp became ambiguous",
                        )?;
                        return Err(error);
                    }
                };
                *publication.borrow_mut() = Some(created);
                hook.borrow_mut()(PamReplaceCrashPoint::ReplacementTemp)
            },
            || {},
            || hook.borrow_mut()(PamReplaceCrashPoint::Exchange),
        );
        if result.is_ok() {
            result = hook.borrow_mut()(PamReplaceCrashPoint::Finalize).and_then(|()| {
                publication
                    .borrow()
                    .as_ref()
                    .ok_or_else(|| ambiguous_publication("PAM publication identity is missing"))
                    .and_then(|publication| {
                        validate_published_path(
                            path,
                            &publication.binding,
                            MAX_BACKUP_BYTES,
                            "published PAM service changed before intent cleanup",
                        )
                    })
            });
        }
        if result.as_ref().is_err_and(|error| {
            error.kind() == ErrorKind::Interrupted || is_ambiguous_publication(error)
        }) {
            return result;
        }
        let cleanup = self.finish_publication_state(
            &intent_name,
            &intent_identity,
            publication.borrow().as_ref(),
        );
        if let Err(cleanup_error) = cleanup {
            return match result {
                Ok(()) => Err(cleanup_error),
                Err(error) => Err(Error::other(format!(
                    "{error}; also failed to clean PAM replacement intent: {cleanup_error}"
                ))),
            };
        }
        result
    }

    fn remove_pam_with_intent_unlocked_hook(
        &self,
        mutation: &PamMutationPlan,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(PamRemoveCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<FileIdentity> {
        self.remove_pam_with_intent_unlocked_published_hook(
            mutation,
            path,
            expected,
            content,
            after_boundary,
            |_| Ok(()),
        )
    }

    fn remove_pam_with_intent_unlocked_published_hook(
        &self,
        mutation: &PamMutationPlan,
        path: &Path,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(PamRemoveCrashPoint) -> std::io::Result<()>,
        mut after_published: impl FnMut(&FileIdentity) -> std::io::Result<()>,
    ) -> std::io::Result<FileIdentity> {
        use std::cell::RefCell;
        use std::io::{Error, ErrorKind};

        if mutation.root != self.root
            || expected.sha256 != mutation.original_sha256
            || sha256_hex(content) != mutation.installed_sha256
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "PAM removal does not match its transaction plan",
            ));
        }
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::PamRemove,
            sequence: mutation.sequence,
            service: mutation.service.clone(),
            backup: mutation.operation.clone(),
            original_sha256: expected.sha256.clone(),
            installed_sha256: sha256_hex(content),
            record_sha256: None,
            replacement_record_sha256: None,
            original_device: Some(expected.device),
            original_inode: Some(expected.inode),
            original_links: Some(expected.links),
            original_mode: Some(expected.mode),
            original_uid: Some(expected.uid),
            original_gid: Some(expected.gid),
        };
        let intent_name = intent_name(IntentRole::PamRemove, &mutation.operation);
        let intent_encoded = serde_json::to_vec_pretty(&intent)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent_identity = atomic_state_create(&self.root, &intent_name, &intent_encoded)?;
        let hook = RefCell::new(after_boundary);
        let publication = RefCell::new(None);
        hook.borrow_mut()(PamRemoveCrashPoint::Intent)?;
        let temp_name = pam_remove_name(&mutation.operation);
        let mut result = replace_existing_verified_with_hooks(
            path,
            expected,
            content,
            Some(&temp_name),
            |replacement| {
                let created = match self.create_publication_binding(
                    PublicationRole::PamRemove,
                    &intent,
                    &intent_encoded,
                    replacement,
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        if is_ambiguous_publication(&error) {
                            return Err(error);
                        }
                        cleanup_unpublished_temp_below(
                            path,
                            &temp_name,
                            replacement,
                            MAX_BACKUP_BYTES,
                            "unpublished PAM removal temp became ambiguous",
                        )?;
                        return Err(error);
                    }
                };
                *publication.borrow_mut() = Some(created);
                hook.borrow_mut()(PamRemoveCrashPoint::ReplacementTemp)
            },
            || {},
            || hook.borrow_mut()(PamRemoveCrashPoint::Exchange),
        );
        if result.is_ok() {
            result = hook.borrow_mut()(PamRemoveCrashPoint::Finalize).and_then(|()| {
                publication
                    .borrow()
                    .as_ref()
                    .ok_or_else(|| ambiguous_publication("PAM publication identity is missing"))
                    .and_then(|publication| {
                        validate_published_path(
                            path,
                            &publication.binding,
                            MAX_BACKUP_BYTES,
                            "published PAM service changed before intent cleanup",
                        )
                    })
            });
        }
        if result.is_ok() {
            result = publication
                .borrow()
                .as_ref()
                .map(|publication| remove_all_binding_identity(&publication.binding))
                .ok_or_else(|| ambiguous_publication("PAM publication identity is missing"))
                .and_then(|identity| after_published(&identity));
        }
        if result.as_ref().is_err_and(|error| {
            error.kind() == ErrorKind::Interrupted || is_ambiguous_publication(error)
        }) && let Err(error) = result
        {
            return Err(error);
        }
        let cleanup = self.finish_publication_state(
            &intent_name,
            &intent_identity,
            publication.borrow().as_ref(),
        );
        if let Err(cleanup_error) = cleanup {
            return match result {
                Ok(()) => Err(cleanup_error),
                Err(error) => Err(Error::other(format!(
                    "{error}; also failed to clean PAM removal intent: {cleanup_error}"
                ))),
            };
        }
        match result {
            Ok(()) => publication
                .borrow()
                .as_ref()
                .map(|publication| remove_all_binding_identity(&publication.binding))
                .ok_or_else(|| ambiguous_publication("PAM publication identity is missing")),
            Err(error) => Err(error),
        }
    }

    fn create_vendor_with_intent_unlocked_hook(
        &self,
        mutation: &PamMutationPlan,
        target: &Target,
        expected: &FileIdentity,
        content: &[u8],
        after_boundary: impl FnMut(VendorCreateCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use std::cell::RefCell;
        use std::io::{Error, ErrorKind};

        if mutation.root != self.root
            || mutation.service != target.service
            || expected.sha256 != mutation.original_sha256
            || sha256_hex(content) != mutation.installed_sha256
            || !matches!(target.origin, Origin::Vendor { .. })
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "vendor creation does not match its transaction plan",
            ));
        }
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::VendorCreate,
            sequence: mutation.sequence,
            service: mutation.service.clone(),
            backup: mutation.operation.clone(),
            original_sha256: expected.sha256.clone(),
            installed_sha256: sha256_hex(content),
            record_sha256: None,
            replacement_record_sha256: None,
            original_device: None,
            original_inode: None,
            original_links: None,
            original_mode: Some(expected.mode),
            original_uid: Some(expected.uid),
            original_gid: Some(expected.gid),
        };
        let intent_name = intent_name(IntentRole::VendorCreate, &mutation.operation);
        let intent_encoded = serde_json::to_vec_pretty(&intent)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let intent_identity = atomic_state_create(&self.root, &intent_name, &intent_encoded)?;
        let hook = RefCell::new(after_boundary);
        let publication = RefCell::new(None);
        hook.borrow_mut()(VendorCreateCrashPoint::Intent)?;
        let temp_name = vendor_create_name(&mutation.operation);
        let mut result = create_override_verified_with_hooks(
            target,
            expected,
            content,
            Some(&temp_name),
            |replacement| {
                let created = match self.create_publication_binding(
                    PublicationRole::VendorCreate,
                    &intent,
                    &intent_encoded,
                    replacement,
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        if is_ambiguous_publication(&error) {
                            return Err(error);
                        }
                        cleanup_unpublished_temp_below(
                            target.write_path(),
                            &temp_name,
                            replacement,
                            MAX_BACKUP_BYTES,
                            "unpublished vendor override temp became ambiguous",
                        )?;
                        return Err(error);
                    }
                };
                *publication.borrow_mut() = Some(created);
                hook.borrow_mut()(VendorCreateCrashPoint::ReplacementTemp)
            },
            || hook.borrow_mut()(VendorCreateCrashPoint::Publish),
            |_, _| {},
        );
        if result.is_ok() {
            result = hook.borrow_mut()(VendorCreateCrashPoint::Finalize).and_then(|()| {
                publication
                    .borrow()
                    .as_ref()
                    .ok_or_else(|| ambiguous_publication("vendor publication identity is missing"))
                    .and_then(|publication| {
                        validate_published_path(
                            target.write_path(),
                            &publication.binding,
                            MAX_BACKUP_BYTES,
                            "published vendor override changed before intent cleanup",
                        )
                    })
            });
        }
        if result.as_ref().is_err_and(|error| {
            error.kind() == ErrorKind::Interrupted || is_ambiguous_publication(error)
        }) {
            return result;
        }
        let cleanup = self.finish_publication_state(
            &intent_name,
            &intent_identity,
            publication.borrow().as_ref(),
        );
        if let Err(cleanup_error) = cleanup {
            return match result {
                Ok(()) => Err(cleanup_error),
                Err(error) => Err(Error::other(format!(
                    "{error}; also failed to clean vendor creation intent: {cleanup_error}"
                ))),
            };
        }
        result
    }

    fn open_existing(root: &Path) -> std::io::Result<Option<Self>> {
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.file_type().is_dir() => {
                let expected_owner = expected_state_owner(root);
                let directory = open_directory_nofollow(root)?;
                secure_state_directory(&directory, expected_owner)?;
                Ok(Some(Self {
                    root: root.to_path_buf(),
                    expected_owner,
                }))
            }
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a directory", root.display()),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn open_existing_read_only(root: &Path) -> std::io::Result<Option<Self>> {
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.file_type().is_dir() => {
                let expected_owner = expected_state_owner(root);
                let directory = open_directory_nofollow(root)?;
                let metadata = directory.metadata()?;
                if !state_directory_attributes_match(
                    metadata.mode(),
                    metadata.uid(),
                    metadata.gid(),
                    expected_owner,
                ) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "PAM backup state directory owner or mode is not trusted",
                    ));
                }
                Ok(Some(Self {
                    root: root.to_path_buf(),
                    expected_owner,
                }))
            }
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a directory", root.display()),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn lock_exclusive(&self) -> std::io::Result<fs::File> {
        let directory = open_directory_nofollow(&self.root)?;
        // SAFETY: `directory` is a live descriptor. Closing the returned file
        // releases this process-local serialization lock.
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        secure_state_directory(&directory, self.expected_owner)?;
        Ok(directory)
    }

    fn transaction<'a>(&'a self, dirs: &PamDirs) -> std::io::Result<BackupTransaction<'a>> {
        let lock = self.lock_exclusive()?;
        recover_remove_all_locked(self, dirs)?;
        self.recover_unlocked(dirs)?;
        Ok(BackupTransaction {
            store: self,
            _lock: lock,
        })
    }

    fn next_sequence(&self) -> std::io::Result<u64> {
        self.scanned_sequences()?
            .into_iter()
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "PAM backup sequence exhausted",
                )
            })
    }

    fn sequence_in_use(&self, sequence: u64) -> std::io::Result<bool> {
        if sequence == 0 {
            return Ok(true);
        }
        Ok(self
            .scanned_sequences()?
            .into_iter()
            .any(|existing| existing == sequence))
    }

    fn scanned_sequences(&self) -> std::io::Result<Vec<u64>> {
        Ok(self
            .scanned_record_pairs()?
            .into_iter()
            .map(|record| record.provenance.sequence)
            .collect())
    }

    fn scanned_record_pairs(&self) -> std::io::Result<Vec<PreparedBackup>> {
        let directory = open_directory_nofollow(&self.root)?;
        let owned_state_file = |metadata: &fs::Metadata| {
            metadata.mode() & 0o7777 == 0o600
                && (metadata.uid(), metadata.gid()) == self.expected_owner
        };
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.ends_with(".json") {
                continue;
            }
            let file = match open_regular_at(&directory, OsStr::new(&name)) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let record_metadata = file.metadata()?;
            if !owned_state_file(&record_metadata) {
                continue;
            }
            let Ok(encoded) = read_open_bounded(&file, MAX_RECORD_BYTES) else {
                continue;
            };
            let record_identity = identity_for_bytes(&record_metadata, &encoded);
            let Ok(provenance) = serde_json::from_slice::<ProvenanceRecord>(&encoded) else {
                continue;
            };
            if !valid_provenance_record(&name, &provenance) {
                continue;
            }
            let backup = match open_regular_at(&directory, OsStr::new(&provenance.backup)) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let backup_metadata = backup.metadata()?;
            if !owned_state_file(&backup_metadata) {
                continue;
            }
            let Ok(original) = read_open_bounded(&backup, MAX_BACKUP_BYTES) else {
                continue;
            };
            if sha256_hex(&original) != provenance.original_sha256 {
                continue;
            }
            let backup_identity = identity_for_bytes(&backup_metadata, &original);
            records.push(PreparedBackup {
                root: self.root.clone(),
                backup: provenance.backup.clone(),
                record: name,
                provenance,
                backup_identity: Some(backup_identity),
                record_identity: Some(record_identity),
            });
        }
        Ok(records)
    }

    fn validated_records(&self, service: &str) -> std::io::Result<Vec<PreparedBackup>> {
        let mut records = self.scanned_record_pairs()?;
        let mut counts = std::collections::HashMap::new();
        for record in &records {
            *counts.entry(record.provenance.sequence).or_insert(0usize) += 1;
        }
        records.retain(|record| {
            record.provenance.service == service && counts[&record.provenance.sequence] == 1
        });
        records.sort_by_key(|record| std::cmp::Reverse(record.provenance.sequence));
        Ok(records)
    }

    fn latest_committed(&self, service: &str) -> std::io::Result<Option<PathBuf>> {
        Ok(self
            .validated_committed_records(service)?
            .into_iter()
            .next()
            .map(|record| record.backup_path()))
    }

    fn validated_committed_records(&self, service: &str) -> std::io::Result<Vec<PreparedBackup>> {
        Ok(self
            .validated_records(service)?
            .into_iter()
            .filter(|record| record.provenance.state == ProvenanceState::Committed)
            .collect())
    }

    fn cleanup(&self, service: &str) -> std::io::Result<()> {
        self.cleanup_with_hook(service, |_| {})
    }

    fn cleanup_with_hook(
        &self,
        service: &str,
        mut before_recheck: impl FnMut(&PreparedBackup),
    ) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        let directory = open_directory_nofollow(&self.root)?;
        for prepared in self.validated_committed_records(service)? {
            before_recheck(&prepared);
            self.cleanup_one_at(&directory, &prepared, |_| Ok(()))?;
        }
        directory.sync_all()
    }

    #[cfg(test)]
    fn cleanup_with_crash_hook(
        &self,
        service: &str,
        mut after_boundary: impl FnMut(CleanupCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        let directory = open_directory_nofollow(&self.root)?;
        for prepared in self.validated_committed_records(service)? {
            self.cleanup_one_at(&directory, &prepared, &mut after_boundary)?;
        }
        directory.sync_all()
    }

    fn cleanup_one_at(
        &self,
        directory: &fs::File,
        prepared: &PreparedBackup,
        mut after_boundary: impl FnMut(CleanupCrashPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let backup = open_regular_at(directory, OsStr::new(&prepared.backup))?;
        let record = open_regular_at(directory, OsStr::new(&prepared.record))?;
        let expected_backup = prepared.backup_identity.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cleanup backup has no captured identity",
            )
        })?;
        let expected_record = prepared.record_identity.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cleanup record has no captured identity",
            )
        })?;
        if !identity_matches(
            expected_backup,
            &identity_of_open_bounded(&backup, MAX_BACKUP_BYTES)?,
        ) || !identity_matches(
            expected_record,
            &identity_of_open_bounded(&record, MAX_RECORD_BYTES)?,
        ) {
            return Err(std::io::Error::other(
                "PAM backup state changed before cleanup",
            ));
        }
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::Cleanup,
            sequence: prepared.provenance.sequence,
            service: prepared.provenance.service.clone(),
            backup: prepared.backup.clone(),
            original_sha256: expected_backup.sha256.clone(),
            installed_sha256: prepared.provenance.installed_sha256.clone(),
            record_sha256: Some(expected_record.sha256.clone()),
            replacement_record_sha256: None,
            original_device: None,
            original_inode: None,
            original_links: None,
            original_mode: None,
            original_uid: None,
            original_gid: None,
        };
        let intent_name = intent_name(IntentRole::Cleanup, &prepared.backup);
        let intent_encoded = serde_json::to_vec_pretty(&intent)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let intent_identity = atomic_state_create(&self.root, &intent_name, &intent_encoded)?;
        after_boundary(CleanupCrashPoint::Intent)?;

        let backup_quarantine = quarantine_name("backup", &prepared.backup);
        let record_quarantine = format!("{}.json", quarantine_name("record", &prepared.backup));
        rename_noreplace_at(directory, &prepared.backup, &backup_quarantine)?;
        let quarantined_backup = open_regular_at(directory, OsStr::new(&backup_quarantine))?;
        let quarantined_backup_identity =
            identity_of_open_bounded(&quarantined_backup, MAX_BACKUP_BYTES)?;
        if !identity_matches(expected_backup, &quarantined_backup_identity) {
            let _ = rename_noreplace_at(directory, &backup_quarantine, &prepared.backup);
            return Err(std::io::Error::other(
                "PAM backup changed during quarantine",
            ));
        }
        after_boundary(CleanupCrashPoint::BackupQuarantine)?;

        if let Err(error) = rename_noreplace_at(directory, &prepared.record, &record_quarantine) {
            let _ = rename_noreplace_at(directory, &backup_quarantine, &prepared.backup);
            return Err(error);
        }
        let quarantined_record = open_regular_at(directory, OsStr::new(&record_quarantine))?;
        let quarantined_record_identity =
            identity_of_open_bounded(&quarantined_record, MAX_RECORD_BYTES)?;
        if !identity_matches(expected_record, &quarantined_record_identity) {
            let _ = rename_noreplace_at(directory, &record_quarantine, &prepared.record);
            let _ = rename_noreplace_at(directory, &backup_quarantine, &prepared.backup);
            return Err(std::io::Error::other(
                "PAM provenance changed during quarantine",
            ));
        }
        after_boundary(CleanupCrashPoint::RecordQuarantine)?;

        unlink_state_if_identity_matches(
            &self.root,
            &backup_quarantine,
            &quarantined_backup_identity,
        )?;
        after_boundary(CleanupCrashPoint::BackupUnlink)?;
        unlink_state_if_identity_matches(
            &self.root,
            &record_quarantine,
            &quarantined_record_identity,
        )?;
        after_boundary(CleanupCrashPoint::RecordUnlink)?;
        unlink_state_if_identity_matches(&self.root, &intent_name, &intent_identity)
    }

    fn validated_intents(&self) -> std::io::Result<Vec<ValidIntent>> {
        let directory = open_directory_nofollow(&self.root)?;
        let mut intents = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.starts_with(".facelock-intent-") || !name.ends_with(".json") {
                continue;
            }
            let file = match open_regular_at(&directory, OsStr::new(&name)) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let metadata = file.metadata()?;
            if metadata.mode() & 0o7777 != 0o600
                || (metadata.uid(), metadata.gid()) != self.expected_owner
            {
                continue;
            }
            let Ok(encoded) = read_open_bounded(&file, MAX_RECORD_BYTES) else {
                continue;
            };
            let Ok(intent) = serde_json::from_slice::<StateIntent>(&encoded) else {
                continue;
            };
            if name != intent_name(intent.role, &intent.backup) || !valid_state_intent(&intent) {
                continue;
            }
            intents.push(ValidIntent {
                name,
                intent,
                identity: identity_for_bytes(&metadata, &encoded),
            });
        }
        Ok(intents)
    }

    fn validated_publications(&self) -> std::io::Result<Vec<ValidPublication>> {
        let directory = open_directory_nofollow(&self.root)?;
        let mut publications = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.starts_with(".facelock-publication-") || !name.ends_with(".json") {
                continue;
            }
            let file = match open_regular_at(&directory, OsStr::new(&name)) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let metadata = file.metadata()?;
            if metadata.mode() & 0o7777 != 0o600
                || (metadata.uid(), metadata.gid()) != self.expected_owner
            {
                continue;
            }
            let Ok(encoded) = read_open_bounded(&file, MAX_RECORD_BYTES) else {
                continue;
            };
            let Ok(binding) = serde_json::from_slice::<PublicationBinding>(&encoded) else {
                continue;
            };
            if name != publication_name(binding.role, &binding.backup)
                || !valid_publication_binding(&binding)
            {
                continue;
            }
            publications.push(ValidPublication {
                name,
                binding,
                identity: identity_for_bytes(&metadata, &encoded),
            });
        }
        Ok(publications)
    }

    fn publication_matches_intent(publication: &PublicationBinding, intent: &ValidIntent) -> bool {
        publication.role.intent_role() == intent.intent.role
            && publication.sequence == intent.intent.sequence
            && publication.service == intent.intent.service
            && publication.backup == intent.intent.backup
            && publication.intent_sha256 == intent.identity.sha256
            && match publication.role {
                PublicationRole::Commit => intent
                    .intent
                    .replacement_record_sha256
                    .as_deref()
                    .is_some_and(|hash| hash == publication.sha256),
                PublicationRole::PamReplace
                | PublicationRole::PamRemove
                | PublicationRole::VendorCreate => {
                    intent.intent.installed_sha256 == publication.sha256
                }
            }
    }

    fn finish_recovered_publication(
        &self,
        intent: Option<&ValidIntent>,
        publication: &ValidPublication,
    ) -> std::io::Result<()> {
        if let Some(intent) = intent {
            unlink_state_if_identity_matches(&self.root, &intent.name, &intent.identity)?;
        }
        unlink_state_if_identity_matches(&self.root, &publication.name, &publication.identity)
    }

    fn recover_publications(&self, dirs: &PamDirs) -> std::io::Result<()> {
        let intents = self.validated_intents()?;
        let state_directory = open_directory_nofollow(&self.root)?;
        for publication in self.validated_publications()? {
            let intent = intents
                .iter()
                .find(|intent| Self::publication_matches_intent(&publication.binding, intent));
            let exact_intent_name = intent_name(
                publication.binding.role.intent_role(),
                &publication.binding.backup,
            );
            if intent.is_none() && entry_exists_at(&state_directory, &exact_intent_name)? {
                // Only an absent exact base-intent name establishes an orphan
                // binding. A malformed, linked, metadata-drifted, or merely
                // mismatching entry is untrusted but still blocks destructive
                // orphan recovery.
                continue;
            }
            match publication.binding.role {
                PublicationRole::Commit => self.recover_commit_publication(&publication, intent)?,
                PublicationRole::PamReplace => self.recover_pam_publication(
                    dirs,
                    &publication,
                    intent,
                    &pam_replace_name(&publication.binding.backup),
                )?,
                PublicationRole::PamRemove => self.recover_pam_publication(
                    dirs,
                    &publication,
                    intent,
                    &pam_remove_name(&publication.binding.backup),
                )?,
                PublicationRole::VendorCreate => {
                    self.recover_vendor_publication(dirs, &publication, intent)?
                }
            }
        }
        Ok(())
    }

    fn recover_commit_publication(
        &self,
        publication: &ValidPublication,
        intent: Option<&ValidIntent>,
    ) -> std::io::Result<()> {
        let directory = open_directory_nofollow(&self.root)?;
        let record = format!("{}.json", publication.binding.backup);
        let exchange = format!(
            "{}.json",
            quarantine_name("commit", &publication.binding.backup)
        );
        let canonical = open_identity_at(&directory, &record, MAX_RECORD_BYTES)?;
        let temp = open_identity_at(&directory, &exchange, MAX_RECORD_BYTES)?;
        let canonical_is_published = canonical
            .as_ref()
            .is_some_and(|identity| binding_identity_matches(&publication.binding, identity));
        let temp_is_published = temp
            .as_ref()
            .is_some_and(|identity| binding_identity_matches(&publication.binding, identity));
        let prepared_hash = intent
            .and_then(|intent| intent.intent.record_sha256.as_deref())
            .unwrap_or_default();
        let canonical_is_prepared = canonical.as_ref().is_some_and(|identity| {
            state_identity_matches(self.expected_owner, identity, prepared_hash)
        });
        let temp_is_prepared = temp.as_ref().is_some_and(|identity| {
            state_identity_matches(self.expected_owner, identity, prepared_hash)
        });

        match (intent, canonical.as_ref(), temp.as_ref()) {
            (Some(_), Some(_), Some(temp_identity))
                if canonical_is_prepared && temp_is_published =>
            {
                unlink_state_if_identity_matches(&self.root, &exchange, temp_identity)?;
            }
            (Some(_), Some(_), Some(temp_identity))
                if canonical_is_published && temp_is_prepared =>
            {
                unlink_state_if_identity_matches(&self.root, &exchange, temp_identity)?;
            }
            (Some(_), Some(_), None) if canonical_is_published => {}
            (None, Some(_), None) if canonical_is_published => {}
            _ => return Ok(()),
        }
        self.finish_recovered_publication(intent, publication)?;
        directory.sync_all()
    }

    fn recover_pam_publication(
        &self,
        dirs: &PamDirs,
        publication: &ValidPublication,
        intent: Option<&ValidIntent>,
        temp_name: &str,
    ) -> std::io::Result<()> {
        let directory = open_directory_nofollow(dirs.overrides())?;
        let canonical =
            open_identity_at(&directory, &publication.binding.service, MAX_BACKUP_BYTES)?;
        let temp = open_identity_at(&directory, temp_name, MAX_BACKUP_BYTES)?;
        let installed =
            |identity: &FileIdentity| binding_identity_matches(&publication.binding, identity);
        let original = |identity: &FileIdentity| {
            intent.is_some_and(|intent| exact_original_intent_identity(&intent.intent, identity))
        };

        match (intent, canonical.as_ref(), temp.as_ref()) {
            (Some(_), Some(current), Some(replacement))
                if original(current) && installed(replacement) =>
            {
                unlink_at_if_identity_matches(
                    &directory,
                    temp_name,
                    replacement,
                    MAX_BACKUP_BYTES,
                )?;
            }
            (Some(_), Some(current), Some(displaced))
                if installed(current) && original(displaced) =>
            {
                unlink_at_if_identity_matches(&directory, temp_name, displaced, MAX_BACKUP_BYTES)?;
            }
            (Some(_), Some(current), None) if installed(current) => {}
            (None, Some(current), None) if installed(current) => {}
            (Some(_), None, None) if publication.binding.role == PublicationRole::PamRemove => {}
            (None, None, None) if publication.binding.role == PublicationRole::PamRemove => {}
            _ => return Ok(()),
        }
        directory.sync_all()?;

        if publication.binding.role == PublicationRole::PamRemove {
            let quarantine = vendor_retire_name(&publication.binding.backup);
            let quarantine_exists = entry_exists_at(&directory, &quarantine)?;
            let should_retire = if quarantine_exists {
                true
            } else {
                match read_regular_at_bounded(
                    &directory,
                    &publication.binding.service,
                    MAX_BACKUP_BYTES,
                )? {
                    Some((content, current)) if installed(&current) => {
                        current_vendor_override_matches(
                            dirs,
                            &publication.binding.service,
                            &content,
                            &current,
                            true,
                        )
                        .unwrap_or(false)
                    }
                    _ => false,
                }
            };
            if should_retire {
                let expected = remove_all_binding_identity(&publication.binding);
                if let Err(error) = retire_vendor_override_with_hook(
                    dirs,
                    &publication.binding.service,
                    &publication.binding.backup,
                    &expected,
                    |_| Ok(()),
                ) && (is_ambiguous_publication(&error)
                    || error.kind() == std::io::ErrorKind::Interrupted)
                {
                    return Err(error);
                }
                // A deterministic vendor mismatch restores the exact
                // quarantined inode to the canonical name. The removal
                // publication is complete even though retirement was
                // declined, so its state evidence may now be finalized.
            }
        }
        self.finish_recovered_publication(intent, publication)
    }

    fn recover_vendor_publication(
        &self,
        dirs: &PamDirs,
        publication: &ValidPublication,
        intent: Option<&ValidIntent>,
    ) -> std::io::Result<()> {
        let directory = open_directory_nofollow(dirs.overrides())?;
        let canonical =
            open_identity_at(&directory, &publication.binding.service, MAX_BACKUP_BYTES)?;
        let temp_name = vendor_create_name(&publication.binding.backup);
        let temp = open_identity_at(&directory, &temp_name, MAX_BACKUP_BYTES)?;
        match (intent, canonical.as_ref(), temp.as_ref()) {
            (Some(_), None, Some(replacement))
                if binding_identity_matches(&publication.binding, replacement) =>
            {
                unlink_at_if_identity_matches(
                    &directory,
                    &temp_name,
                    replacement,
                    MAX_BACKUP_BYTES,
                )?;
            }
            (Some(_), Some(installed), None)
                if binding_identity_matches(&publication.binding, installed) => {}
            (None, Some(installed), None)
                if binding_identity_matches(&publication.binding, installed) => {}
            _ => return Ok(()),
        }
        directory.sync_all()?;
        self.finish_recovered_publication(intent, publication)
    }

    fn matching_state_entry(
        &self,
        directory: &fs::File,
        name: &str,
        expected_sha256: &str,
        limit: usize,
    ) -> std::io::Result<Option<FileIdentity>> {
        let file = match open_regular_at(directory, OsStr::new(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let identity = identity_of_open_bounded(&file, limit)?;
        if !state_identity_matches(self.expected_owner, &identity, expected_sha256) {
            return Err(std::io::Error::other(
                "reserved state entry does not match its durable intent",
            ));
        }
        Ok(Some(identity))
    }

    fn recover_prepare_intent(
        &self,
        directory: &fs::File,
        valid: &ValidIntent,
    ) -> std::io::Result<()> {
        let intent = &valid.intent;
        let Some(record_hash) = intent.record_sha256.as_deref() else {
            return Ok(());
        };
        let record = format!("{}.json", intent.backup);
        let backup = self.matching_state_entry(
            directory,
            &intent.backup,
            &intent.original_sha256,
            MAX_BACKUP_BYTES,
        );
        let provenance =
            self.matching_state_entry(directory, &record, record_hash, MAX_RECORD_BYTES);
        let (Ok(backup), Ok(provenance)) = (backup, provenance) else {
            // An entry exists in the reserved operation's namespace but does
            // not match the intent. It is ambiguous and is preserved.
            return Ok(());
        };

        match (backup, provenance) {
            (Some(_), Some(_)) => {}
            (Some(identity), None) => {
                unlink_state_if_identity_matches(&self.root, &intent.backup, &identity)?;
            }
            (None, Some(identity)) => {
                unlink_state_if_identity_matches(&self.root, &record, &identity)?;
            }
            (None, None) => {}
        }
        unlink_state_if_identity_matches(&self.root, &valid.name, &valid.identity)?;
        directory.sync_all()
    }

    fn recover_intents(&self, dirs: &PamDirs) -> std::io::Result<()> {
        let directory = open_directory_nofollow(&self.root)?;
        for valid in self.validated_intents()? {
            match valid.intent.role {
                IntentRole::Prepare => self.recover_prepare_intent(&directory, &valid)?,
                IntentRole::Commit => self.recover_commit_intent(&directory, &valid)?,
                IntentRole::Cleanup => self.recover_cleanup_intent(&directory, &valid)?,
                IntentRole::PamReplace => self.recover_pam_replace_intent(dirs, &valid)?,
                IntentRole::PamRemove => self.recover_pam_remove_intent(dirs, &valid)?,
                IntentRole::VendorCreate => self.recover_vendor_create_intent(dirs, &valid)?,
            }
        }
        Ok(())
    }

    fn recover_commit_intent(
        &self,
        directory: &fs::File,
        valid: &ValidIntent,
    ) -> std::io::Result<()> {
        let intent = &valid.intent;
        let Some(record_hash) = intent.record_sha256.as_deref() else {
            return Ok(());
        };
        let Some(_) = intent.replacement_record_sha256.as_deref() else {
            return Ok(());
        };
        let record = format!("{}.json", intent.backup);
        let exchange = format!("{}.json", quarantine_name("commit", &intent.backup));
        let canonical_prepared = self
            .matching_state_entry(directory, &record, record_hash, MAX_RECORD_BYTES)
            .ok()
            .flatten();
        let exchange_entry = open_identity_at(directory, &exchange, MAX_RECORD_BYTES)?;

        if canonical_prepared.is_some() && exchange_entry.is_none() {
            // Crash before a named replacement and its full identity binding
            // existed. Once a binding has existed, it is recovered first and
            // shape/hash alone never authenticates a committed inode.
        } else {
            return Ok(());
        }
        unlink_state_if_identity_matches(&self.root, &valid.name, &valid.identity)?;
        directory.sync_all()
    }

    fn recover_cleanup_intent(
        &self,
        directory: &fs::File,
        valid: &ValidIntent,
    ) -> std::io::Result<()> {
        let intent = &valid.intent;
        let Some(record_hash) = intent.record_sha256.as_deref() else {
            return Ok(());
        };
        let record = format!("{}.json", intent.backup);
        let backup_quarantine = quarantine_name("backup", &intent.backup);
        let record_quarantine = format!("{}.json", quarantine_name("record", &intent.backup));
        let backup = match self.resume_quarantine(
            directory,
            &intent.backup,
            &backup_quarantine,
            &intent.original_sha256,
            MAX_BACKUP_BYTES,
        ) {
            Ok(identity) => identity,
            Err(_) => return Ok(()),
        };
        let record = match self.resume_quarantine(
            directory,
            &record,
            &record_quarantine,
            record_hash,
            MAX_RECORD_BYTES,
        ) {
            Ok(identity) => identity,
            Err(_) => return Ok(()),
        };
        if let Some(identity) = backup {
            unlink_state_if_identity_matches(&self.root, &backup_quarantine, &identity)?;
        }
        if let Some(identity) = record {
            unlink_state_if_identity_matches(&self.root, &record_quarantine, &identity)?;
        }
        unlink_state_if_identity_matches(&self.root, &valid.name, &valid.identity)?;
        directory.sync_all()
    }

    fn resume_quarantine(
        &self,
        directory: &fs::File,
        canonical: &str,
        quarantine: &str,
        expected_sha256: &str,
        limit: usize,
    ) -> std::io::Result<Option<FileIdentity>> {
        let canonical_identity =
            self.matching_state_entry(directory, canonical, expected_sha256, limit)?;
        let quarantine_identity =
            self.matching_state_entry(directory, quarantine, expected_sha256, limit)?;
        match (canonical_identity, quarantine_identity) {
            (Some(expected), None) => {
                rename_noreplace_at(directory, canonical, quarantine)?;
                let moved = self
                    .matching_state_entry(directory, quarantine, expected_sha256, limit)?
                    .ok_or_else(|| std::io::Error::other("quarantined state disappeared"))?;
                if !identity_matches(&expected, &moved) {
                    let _ = rename_noreplace_at(directory, quarantine, canonical);
                    return Err(std::io::Error::other(
                        "state entry changed during recovery quarantine",
                    ));
                }
                Ok(Some(moved))
            }
            (None, Some(identity)) => Ok(Some(identity)),
            (None, None) => Ok(None),
            (Some(_), Some(_)) => Err(std::io::Error::other(
                "both canonical and quarantine state entries exist",
            )),
        }
    }

    fn recover_pam_replace_intent(
        &self,
        dirs: &PamDirs,
        valid: &ValidIntent,
    ) -> std::io::Result<()> {
        let intent = &valid.intent;
        let Some(record_hash) = intent.record_sha256.as_deref() else {
            return Ok(());
        };
        let record = format!("{}.json", intent.backup);
        let state_directory = open_directory_nofollow(&self.root)?;
        if self
            .matching_state_entry(&state_directory, &record, record_hash, MAX_RECORD_BYTES)
            .ok()
            .flatten()
            .is_none()
        {
            return Ok(());
        }
        self.recover_existing_pam_mutation_intent(dirs, valid, &pam_replace_name(&intent.backup))
    }

    fn recover_pam_remove_intent(
        &self,
        dirs: &PamDirs,
        valid: &ValidIntent,
    ) -> std::io::Result<()> {
        self.recover_existing_pam_mutation_intent(
            dirs,
            valid,
            &pam_remove_name(&valid.intent.backup),
        )
    }

    fn recover_existing_pam_mutation_intent(
        &self,
        dirs: &PamDirs,
        valid: &ValidIntent,
        temp_name: &str,
    ) -> std::io::Result<()> {
        let intent = &valid.intent;
        let directory = open_directory_nofollow(dirs.overrides())?;
        let canonical = match open_regular_at(&directory, OsStr::new(&intent.service)) {
            Ok(file) => Some(identity_of_open_bounded(&file, MAX_BACKUP_BYTES)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Ok(()),
        };
        let temp = match open_regular_at(&directory, OsStr::new(temp_name)) {
            Ok(file) => Some(identity_of_open_bounded(&file, MAX_BACKUP_BYTES)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Ok(()),
        };
        match (canonical.as_ref(), temp.as_ref()) {
            (Some(current), None) if exact_original_intent_identity(intent, current) => {}
            _ => {
                // A named/published replacement without its strict identity
                // binding is ambiguous. Preserve both names and the intent.
                return Ok(());
            }
        }
        directory.sync_all()?;
        unlink_state_if_identity_matches(&self.root, &valid.name, &valid.identity)
    }

    fn recover_vendor_create_intent(
        &self,
        dirs: &PamDirs,
        valid: &ValidIntent,
    ) -> std::io::Result<()> {
        let intent = &valid.intent;
        let directory = open_directory_nofollow(dirs.overrides())?;
        let canonical = match open_regular_at(&directory, OsStr::new(&intent.service)) {
            Ok(file) => Some(identity_of_open_bounded(&file, MAX_BACKUP_BYTES)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Ok(()),
        };
        let temp_name = vendor_create_name(&intent.backup);
        let temp = match open_regular_at(&directory, OsStr::new(&temp_name)) {
            Ok(file) => Some(identity_of_open_bounded(&file, MAX_BACKUP_BYTES)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Ok(()),
        };
        match (canonical.as_ref(), temp.as_ref()) {
            (None, None) => {}
            _ => {
                // A created inode is never authenticated by bytes/metadata
                // alone once publication binding is part of the protocol.
                return Ok(());
            }
        }
        directory.sync_all()?;
        unlink_state_if_identity_matches(&self.root, &valid.name, &valid.identity)
    }

    fn recover_owned_temps(&self) -> std::io::Result<()> {
        let directory = open_directory_nofollow(&self.root)?;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some((destination, expected_hash)) = owned_temp_parts(&name) else {
                continue;
            };
            let file = match open_regular_at(&directory, OsStr::new(&name)) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let metadata = file.metadata()?;
            if metadata.mode() & 0o7777 != 0o600
                || (metadata.uid(), metadata.gid()) != self.expected_owner
            {
                continue;
            }
            let Ok(content) = read_open_bounded(&file, MAX_BACKUP_BYTES) else {
                continue;
            };
            let identity = identity_for_bytes(&metadata, &content);
            if identity.sha256 != expected_hash
                || !valid_owned_temp_destination(destination, &content)
            {
                continue;
            }
            unlink_state_if_identity_matches(&self.root, &name, &identity)?;
        }
        directory.sync_all()
    }

    fn recover(&self, dirs: &PamDirs) -> std::io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.recover_unlocked(dirs)
    }

    fn recover_unlocked(&self, dirs: &PamDirs) -> std::io::Result<()> {
        self.recover_owned_temps()?;
        self.recover_publications(dirs)?;
        self.recover_intents(dirs)?;
        let directory = open_directory_nofollow(&self.root)?;
        let mut hinted_services = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.ends_with(".json") {
                continue;
            }
            let file = match open_regular_at(&directory, OsStr::new(&name)) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let Ok(encoded) = read_open_bounded(&file, MAX_RECORD_BYTES) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<ProvenanceRecord>(&encoded) else {
                continue;
            };
            if valid_provenance_record(&name, &record) && !hinted_services.contains(&record.service)
            {
                hinted_services.push(record.service);
            }
        }

        for service in hinted_services {
            for prepared in self.validated_records(&service)? {
                if prepared.provenance.state != ProvenanceState::Prepared {
                    continue;
                }
                // A remaining replacement intent means recovery could not
                // classify the PAM-side names unambiguously. Preserve the
                // rollback pair too; name presence can block destructive
                // recovery but never establishes Facelock ownership.
                if fs::symlink_metadata(
                    self.root
                        .join(intent_name(IntentRole::PamReplace, &prepared.backup)),
                )
                .is_ok()
                {
                    continue;
                }
                // Add only backs up an existing local override, so recovery
                // re-resolves the service beneath the write root, not under a
                // path from the record and not by falling through to vendor.
                let target = dirs.overrides().join(&service);
                let Ok((content, _)) = read_regular_nofollow(&target) else {
                    continue;
                };
                let current = sha256_hex(&content);
                if current == prepared.provenance.installed_sha256 {
                    self.commit_unlocked(&prepared, |_| Ok(()))?;
                } else if current == prepared.provenance.original_sha256 {
                    self.cleanup_one_at(&directory, &prepared, |_| Ok(()))?;
                    directory.sync_all()?;
                }
            }
        }
        Ok(())
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_provenance_record(name: &str, record: &ProvenanceRecord) -> bool {
    let Some(backup) = name.strip_suffix(".json") else {
        return false;
    };
    record.version == PROVENANCE_VERSION
        && record.sequence != 0
        && confined(&record.service).is_ok()
        && Path::new(&record.backup).file_name() == Some(OsStr::new(&record.backup))
        && record.backup == backup
        && valid_backup_name(&record.service, backup)
        && valid_sha256(&record.original_sha256)
        && valid_sha256(&record.installed_sha256)
}

fn valid_state_intent(intent: &StateIntent) -> bool {
    let identity = (
        intent.original_device,
        intent.original_inode,
        intent.original_links,
        intent.original_mode,
        intent.original_uid,
        intent.original_gid,
    );
    let no_identity = identity == (None, None, None, None, None, None);
    let exact_identity = matches!(
        identity,
        (Some(_), Some(_), Some(1), Some(mode), Some(_), Some(_))
            if mode & libc::S_IFMT == libc::S_IFREG
    );
    let expected_metadata = matches!(
        identity,
        (None, None, None, Some(mode), Some(_), Some(_))
            if mode & libc::S_IFMT == libc::S_IFREG
    );
    let record_hash = intent.record_sha256.as_deref().is_some_and(valid_sha256);
    let replacement_hash = intent
        .replacement_record_sha256
        .as_deref()
        .is_some_and(valid_sha256);
    let role_fields = match intent.role {
        IntentRole::Prepare | IntentRole::Cleanup => {
            record_hash && intent.replacement_record_sha256.is_none() && no_identity
        }
        IntentRole::Commit => record_hash && replacement_hash && no_identity,
        IntentRole::PamReplace => {
            record_hash && intent.replacement_record_sha256.is_none() && exact_identity
        }
        IntentRole::PamRemove => {
            intent.record_sha256.is_none()
                && intent.replacement_record_sha256.is_none()
                && exact_identity
        }
        IntentRole::VendorCreate => {
            intent.record_sha256.is_none()
                && intent.replacement_record_sha256.is_none()
                && expected_metadata
        }
    };
    intent.version == PROVENANCE_VERSION
        && intent.sequence != 0
        && confined(&intent.service).is_ok()
        && valid_backup_name(&intent.service, &intent.backup)
        && valid_sha256(&intent.original_sha256)
        && valid_sha256(&intent.installed_sha256)
        && role_fields
}

fn valid_publication_binding(binding: &PublicationBinding) -> bool {
    binding.version == PROVENANCE_VERSION
        && binding.sequence != 0
        && confined(&binding.service).is_ok()
        && valid_backup_name(&binding.service, &binding.backup)
        && valid_sha256(&binding.intent_sha256)
        && valid_sha256(&binding.sha256)
        && binding.links == 1
        && binding.mode & libc::S_IFMT == libc::S_IFREG
}

fn publication_binding_for(
    role: PublicationRole,
    intent: &StateIntent,
    intent_encoded: &[u8],
    replacement: &FileIdentity,
) -> PublicationBinding {
    PublicationBinding {
        version: PROVENANCE_VERSION,
        role,
        sequence: intent.sequence,
        service: intent.service.clone(),
        backup: intent.backup.clone(),
        intent_sha256: sha256_hex(intent_encoded),
        device: replacement.device,
        inode: replacement.inode,
        links: replacement.links,
        sha256: replacement.sha256.clone(),
        mode: replacement.mode,
        uid: replacement.uid,
        gid: replacement.gid,
    }
}

fn binding_identity_matches(binding: &PublicationBinding, actual: &FileIdentity) -> bool {
    (
        binding.device,
        binding.inode,
        binding.links,
        binding.sha256.as_str(),
        binding.mode,
        binding.uid,
        binding.gid,
    ) == (
        actual.device,
        actual.inode,
        actual.links,
        actual.sha256.as_str(),
        actual.mode,
        actual.uid,
        actual.gid,
    )
}

fn owned_temp_parts(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(".facelock-tmp-")?;
    let (destination_hash_pid, nanoseconds) = rest.rsplit_once('-')?;
    let (destination_hash, pid) = destination_hash_pid.rsplit_once('-')?;
    let (destination, hash) = destination_hash.rsplit_once('-')?;
    (valid_sha256(hash)
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !nanoseconds.is_empty()
        && nanoseconds.bytes().all(|byte| byte.is_ascii_digit())
        && !destination.is_empty())
    .then_some((destination, hash))
}

fn backup_service(name: &str) -> Option<&str> {
    let (service, _) = name.rsplit_once('.')?;
    (confined(service).is_ok() && valid_backup_name(service, name)).then_some(service)
}

fn valid_owned_temp_destination(destination: &str, content: &[u8]) -> bool {
    if backup_service(destination).is_some() {
        return true;
    }
    if let Some(backup) = destination.strip_suffix(".json")
        && backup_service(backup).is_some()
        && let Ok(record) = serde_json::from_slice::<ProvenanceRecord>(content)
    {
        return valid_provenance_record(destination, &record);
    }
    if destination.starts_with(".facelock-intent-")
        && let Ok(intent) = serde_json::from_slice::<StateIntent>(content)
    {
        return destination == intent_name(intent.role, &intent.backup)
            && valid_state_intent(&intent);
    }
    if destination.starts_with(".facelock-publication-")
        && let Ok(binding) = serde_json::from_slice::<PublicationBinding>(content)
    {
        return destination == publication_name(binding.role, &binding.backup)
            && valid_publication_binding(&binding);
    }
    if let Some(backup) = destination
        .strip_prefix(".facelock-quarantine-commit-")
        .and_then(|name| name.strip_suffix(".json"))
        && backup_service(backup).is_some()
        && let Ok(record) = serde_json::from_slice::<ProvenanceRecord>(content)
    {
        return valid_provenance_record(&format!("{backup}.json"), &record)
            && record.state == ProvenanceState::Committed;
    }
    false
}

fn valid_backup_name(service: &str, name: &str) -> bool {
    let Some(timestamp) = name.strip_prefix(&format!("{service}.")) else {
        return false;
    };
    let Some((seconds, nanoseconds)) = timestamp.split_once('-') else {
        return false;
    };
    !seconds.is_empty()
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && seconds.parse::<u64>().is_ok()
        && nanoseconds.len() == 9
        && nanoseconds.bytes().all(|byte| byte.is_ascii_digit())
        && nanoseconds
            .parse::<u32>()
            .is_ok_and(|value| value < 1_000_000_000)
}

fn atomic_state_create(root: &Path, name: &str, content: &[u8]) -> std::io::Result<FileIdentity> {
    atomic_state_publish(root, name, content)
}

fn rename_exchange_at(directory: &fs::File, left: &str, right: &str) -> std::io::Result<()> {
    let left = CString::new(left.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state name contains NUL")
    })?;
    let right = CString::new(right.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state name contains NUL")
    })?;
    // SAFETY: both are validated/derived basenames beneath the live state fd.
    if unsafe {
        libc::renameat2(
            directory.as_raw_fd(),
            left.as_ptr(),
            directory.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    directory.sync_all()
}

fn rename_noreplace_at(
    directory: &fs::File,
    source: &str,
    destination: &str,
) -> std::io::Result<()> {
    let source_name = source;
    let destination_name = destination;
    let source = CString::new(source.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state name contains NUL")
    })?;
    let destination = CString::new(destination.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state name contains NUL")
    })?;
    // SAFETY: both are derived basenames beneath the same live state fd.
    if unsafe {
        libc::renameat2(
            directory.as_raw_fd(),
            source.as_ptr(),
            directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    sync_rename_noreplace_parent(directory, source_name, destination_name)
}

#[cfg(test)]
type RenameNoreplaceSyncTestHook = Box<dyn FnMut(&str, &str) -> std::io::Result<()>>;

#[cfg(test)]
thread_local! {
    static RENAME_NOREPLACE_SYNC_TEST_HOOK: std::cell::RefCell<
        Option<RenameNoreplaceSyncTestHook>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn install_rename_noreplace_sync_test_hook(
    hook: impl FnMut(&str, &str) -> std::io::Result<()> + 'static,
) {
    RENAME_NOREPLACE_SYNC_TEST_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn clear_rename_noreplace_sync_test_hook() {
    RENAME_NOREPLACE_SYNC_TEST_HOOK.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
fn run_rename_noreplace_sync_test_hook(source: &str, destination: &str) -> std::io::Result<()> {
    RENAME_NOREPLACE_SYNC_TEST_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(source, destination),
        None => Ok(()),
    })
}

fn sync_rename_noreplace_parent(
    directory: &fs::File,
    source: &str,
    destination: &str,
) -> std::io::Result<()> {
    #[cfg(test)]
    run_rename_noreplace_sync_test_hook(source, destination)?;
    #[cfg(not(test))]
    let _ = (source, destination);
    directory.sync_all()
}

#[cfg(test)]
type StatePublicationSyncTestHook = Box<dyn FnMut(&str) -> std::io::Result<()>>;

#[cfg(test)]
thread_local! {
    static STATE_PUBLICATION_SYNC_TEST_HOOK: std::cell::RefCell<
        Option<StatePublicationSyncTestHook>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn install_state_publication_sync_test_hook(
    hook: impl FnMut(&str) -> std::io::Result<()> + 'static,
) {
    STATE_PUBLICATION_SYNC_TEST_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn clear_state_publication_sync_test_hook() {
    STATE_PUBLICATION_SYNC_TEST_HOOK.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
fn run_state_publication_sync_test_hook(name: &str) -> std::io::Result<()> {
    STATE_PUBLICATION_SYNC_TEST_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(name),
        None => Ok(()),
    })
}

fn sync_state_publication_parent(directory: &fs::File, name: &str) -> std::io::Result<()> {
    #[cfg(test)]
    run_state_publication_sync_test_hook(name)?;
    #[cfg(not(test))]
    let _ = name;
    directory.sync_all()
}

fn atomic_state_publish(root: &Path, name: &str, content: &[u8]) -> std::io::Result<FileIdentity> {
    use std::io::{Error, ErrorKind};

    if Path::new(name).file_name() != Some(OsStr::new(name)) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "state name is not a basename",
        ));
    }
    let temp_name = format!(
        ".facelock-tmp-{name}-{}-{}-{}",
        sha256_hex(content),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    );
    let directory = open_directory_nofollow(root)?;
    let c_temp = CString::new(temp_name.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "state temp contains NUL"))?;
    let c_destination = CString::new(name.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "state destination contains NUL"))?;
    let mut owned_temp = None;
    let mut published = false;
    let written = (|| -> std::io::Result<FileIdentity> {
        // SAFETY: both the live directory descriptor and C basename outlive
        // the call. O_EXCL and O_NOFOLLOW establish ownership without
        // traversing an attacker-selected state path.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c_temp.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned one new owned descriptor.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        let prepared = (|| -> std::io::Result<()> {
            file.write_all(content)?;
            let current = file.metadata()?;
            let owner = if unsafe { libc::geteuid() } == 0 {
                (0, 0)
            } else {
                (current.uid(), current.gid())
            };
            apply_owner_then_mode(&file, owner.0, owner.1, 0o600)?;
            file.sync_all()
        })();
        if let Err(error) = prepared {
            owned_temp = identity_of_open(&file).ok();
            return Err(error);
        }
        let identity = identity_of_open(&file)?;
        owned_temp = Some(identity.clone());
        // SAFETY: both basenames remain beneath the same live state dirfd.
        // NOREPLACE is the atomic ownership boundary: an existing state name
        // is untouched.
        if unsafe {
            libc::renameat2(
                directory.as_raw_fd(),
                c_temp.as_ptr(),
                directory.as_raw_fd(),
                c_destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        published = true;
        let synced = sync_state_publication_parent(&directory, name);
        if synced.is_err() {
            return Err(ambiguous_publication(
                "state publication parent sync failed after rename",
            ));
        }
        Ok(identity)
    })();
    if written.is_err()
        && !published
        && let Some(identity) = owned_temp.as_ref()
    {
        let _ = unlink_at_if_identity_matches(&directory, &temp_name, identity, MAX_BACKUP_BYTES);
        let _ = directory.sync_all();
    }
    written
}

fn unlink_state_if_identity_matches(
    root: &Path,
    name: &str,
    expected: &FileIdentity,
) -> std::io::Result<()> {
    let directory = open_directory_nofollow(root)?;
    unlink_at_if_identity_matches(
        &directory,
        name,
        expected,
        if name.ends_with(".json") {
            MAX_RECORD_BYTES
        } else {
            MAX_BACKUP_BYTES
        },
    )?;
    directory.sync_all()
}

fn unlink_at_if_identity_matches(
    directory: &fs::File,
    name: &str,
    expected: &FileIdentity,
    limit: usize,
) -> std::io::Result<()> {
    let file = open_regular_at(directory, OsStr::new(name))?;
    if !identity_matches(expected, &identity_of_open_bounded(&file, limit)?) {
        return Err(std::io::Error::other(
            "state entry changed before owned cleanup",
        ));
    }
    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state name contains NUL")
    })?;
    // SAFETY: exact basename below an open state directory, after identity
    // verification of the still-open descriptor.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn cleanup_unpublished_temp_below(
    destination: &Path,
    name: &str,
    expected: &FileIdentity,
    limit: usize,
    ambiguous_message: &'static str,
) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| ambiguous_publication(ambiguous_message))?;
    cleanup_unpublished_temp(parent, name, expected, limit, ambiguous_message)
}

fn cleanup_unpublished_temp(
    directory_path: &Path,
    name: &str,
    expected: &FileIdentity,
    limit: usize,
    ambiguous_message: &'static str,
) -> std::io::Result<()> {
    let directory = open_directory_nofollow(directory_path)
        .map_err(|_| ambiguous_publication(ambiguous_message))?;
    cleanup_unpublished_temp_at(&directory, name, expected, limit, ambiguous_message)
}

fn cleanup_unpublished_temp_at(
    directory: &fs::File,
    name: &str,
    expected: &FileIdentity,
    limit: usize,
    ambiguous_message: &'static str,
) -> std::io::Result<()> {
    unlink_at_if_identity_matches(directory, name, expected, limit)
        .map_err(|_| ambiguous_publication(ambiguous_message))?;
    directory
        .sync_all()
        .map_err(|_| ambiguous_publication(ambiguous_message))
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    device: u64,
    inode: u64,
    links: u64,
    sha256: String,
    mode: u32,
    uid: u32,
    gid: u32,
}

fn open_directory_nofollow(path: &Path) -> std::io::Result<fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `path` is NUL-terminated and outlives the call. The returned fd
    // is checked and transferred exactly once into `File`.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `open` returned a new owned descriptor.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

/// Enumerate names through an already-open directory descriptor.
///
/// `std::fs::read_dir` resolves a pathname again. Cleanup instead needs the
/// directory whose identity was accepted by `open_directory_nofollow`, even
/// if that pathname is renamed while discovery is running.
fn directory_entry_names(directory: &fs::File) -> std::io::Result<Vec<OsString>> {
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: `fdopendir` returned this unique stream and `Drop` runs
            // once. `closedir` also closes the duplicated descriptor.
            unsafe { libc::closedir(self.0) };
        }
    }

    // SAFETY: `directory` remains live. The duplicate is transferred to
    // `fdopendir`, so enumeration cannot change or close the caller's fd.
    let duplicated = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `duplicated` is a new directory descriptor owned by this call.
    let stream = unsafe { libc::fdopendir(duplicated) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: ownership was not transferred when `fdopendir` failed.
        unsafe { libc::close(duplicated) };
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: Linux exposes thread-local errno here. Clearing it lets a
        // null `readdir` distinguish end-of-stream from an actual error.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: `stream` remains open and is used by this thread only.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(0) {
                break;
            }
            return Err(error);
        }
        // SAFETY: `readdir` returns one live dirent whose d_name is
        // NUL-terminated until the next call. Copy it before continuing.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(OsString::from_vec(bytes.to_vec()));
    }
    Ok(names)
}

fn entry_exists_at(directory: &fs::File, name: &str) -> std::io::Result<bool> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state name contains NUL")
    })?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the directory fd and C basename remain live; metadata points to
    // writable storage for one stat. NOFOLLOW makes even a dangling exact
    // intent symlink count as present without traversing it.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn metadata_at_nofollow(directory: &fs::File, name: &OsStr) -> std::io::Result<libc::stat> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file name contains NUL")
    })?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the live directory descriptor and C basename identify one
    // directory entry; NOFOLLOW observes the entry itself and the output
    // points to writable storage for one stat value.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized the complete stat value.
    Ok(unsafe { metadata.assume_init() })
}

fn read_link_at(directory: &fs::File, name: &OsStr) -> std::io::Result<PathBuf> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file name contains NUL")
    })?;
    let mut target = vec![0_u8; libc::PATH_MAX as usize + 1];
    // SAFETY: `name` and the output buffer remain live for the call. readlinkat
    // reads the link bytes themselves and never traverses the final component.
    let length = unsafe {
        libc::readlinkat(
            directory.as_raw_fd(),
            name.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if length < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let length = length as usize;
    if length == target.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PAM symlink target is too long",
        ));
    }
    target.truncate(length);
    Ok(PathBuf::from(OsString::from_vec(target)))
}

fn symlink_is_covered_by_later_root(
    directory: &fs::File,
    service: &str,
    later_roots: &[PathBuf],
) -> std::io::Result<bool> {
    let target = read_link_at(directory, OsStr::new(service))?;
    Ok(target.is_absolute() && later_roots.iter().any(|root| target == root.join(service)))
}

fn open_regular_at(directory: &fs::File, name: &OsStr) -> std::io::Result<fs::File> {
    use std::io::{Error, ErrorKind};

    let name = CString::new(name.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "file name contains NUL"))?;
    // SAFETY: `name` is NUL-terminated, and the borrowed directory fd remains
    // open for the call. O_NOFOLLOW makes the final component non-traversable.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "PAM service is not a regular file",
        ));
    }
    if metadata.nlink() != 1 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("PAM service has {} hard links", metadata.nlink()),
        ));
    }
    Ok(file)
}

fn read_regular_nofollow(path: &Path) -> std::io::Result<(Vec<u8>, FileIdentity)> {
    use std::io::{Error, ErrorKind};

    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "PAM path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "PAM path has no file name"))?;
    let directory = open_directory_nofollow(parent)?;
    let file = open_regular_at(&directory, name)?;
    let metadata = file.metadata()?;
    let content = read_open_bounded(&file, MAX_BACKUP_BYTES)?;
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        links: metadata.nlink(),
        sha256: sha256_hex(&content),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    };
    Ok((content, identity))
}

fn identity_matches(expected: &FileIdentity, actual: &FileIdentity) -> bool {
    (
        expected.device,
        expected.inode,
        expected.links,
        expected.sha256.as_str(),
        expected.mode,
        expected.uid,
        expected.gid,
    ) == (
        actual.device,
        actual.inode,
        actual.links,
        actual.sha256.as_str(),
        actual.mode,
        actual.uid,
        actual.gid,
    )
}

fn exact_original_intent_identity(intent: &StateIntent, identity: &FileIdentity) -> bool {
    identity.sha256 == intent.original_sha256
        && Some(identity.device) == intent.original_device
        && Some(identity.inode) == intent.original_inode
        && Some(identity.links) == intent.original_links
        && Some(identity.mode) == intent.original_mode
        && Some(identity.uid) == intent.original_uid
        && Some(identity.gid) == intent.original_gid
}

fn open_identity_at(
    directory: &fs::File,
    name: &str,
    limit: usize,
) -> std::io::Result<Option<FileIdentity>> {
    match open_regular_at(directory, OsStr::new(name)) {
        Ok(file) => identity_of_open_bounded(&file, limit).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn validate_published_path(
    path: &Path,
    binding: &PublicationBinding,
    limit: usize,
    message: &'static str,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("published path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("published path has no file name"))?;
    let directory = open_directory_nofollow(parent)?;
    let published = open_regular_at(&directory, name)
        .and_then(|file| identity_of_open_bounded(&file, limit))
        .map_err(|_| ambiguous_publication(message))?;
    if !binding_identity_matches(binding, &published) {
        return Err(ambiguous_publication(message));
    }
    Ok(())
}

#[derive(Debug)]
struct AmbiguousPublication(&'static str);

impl std::fmt::Display for AmbiguousPublication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for AmbiguousPublication {}

fn ambiguous_publication(message: &'static str) -> std::io::Error {
    std::io::Error::other(AmbiguousPublication(message))
}

fn is_ambiguous_publication(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<AmbiguousPublication>())
        .is_some()
}

fn state_identity_matches(
    expected_owner: (u32, u32),
    identity: &FileIdentity,
    expected_sha256: &str,
) -> bool {
    identity.sha256 == expected_sha256
        && identity.links == 1
        && identity.mode & 0o7777 == 0o600
        && (identity.uid, identity.gid) == expected_owner
}

fn identity_of_open(file: &fs::File) -> std::io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    let content = read_open_bounded(file, usize::MAX)?;
    Ok(identity_for_bytes(&metadata, &content))
}

fn identity_of_open_bounded(file: &fs::File, limit: usize) -> std::io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    let content = read_open_bounded(file, limit)?;
    Ok(identity_for_bytes(&metadata, &content))
}

fn read_open_bounded(file: &fs::File, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Seek};

    let mut readable = file.try_clone()?;
    readable.rewind()?;
    let metadata = readable.metadata()?;
    if metadata.len() > limit as u64 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "state file exceeds size limit",
        ));
    }
    let mut content = Vec::with_capacity(metadata.len() as usize);
    readable
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() > limit {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "state file exceeds size limit",
        ));
    }
    Ok(content)
}

fn identity_for_bytes(metadata: &fs::Metadata, content: &[u8]) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        links: metadata.nlink(),
        sha256: sha256_hex(content),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    }
}

#[cfg(test)]
type TempCreationTestHook = Box<dyn FnOnce(&fs::File) -> std::io::Result<()>>;

#[cfg(test)]
thread_local! {
    static TEMP_CREATION_TEST_HOOK: std::cell::RefCell<
        Option<TempCreationTestHook>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn install_temp_creation_test_hook(hook: impl FnOnce(&fs::File) -> std::io::Result<()> + 'static) {
    TEMP_CREATION_TEST_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_temp_creation_test_hook(file: &fs::File) -> std::io::Result<()> {
    TEMP_CREATION_TEST_HOOK.with(|slot| match slot.borrow_mut().take() {
        Some(hook) => hook(file),
        None => Ok(()),
    })
}

#[derive(Debug)]
struct CreatedTemp {
    name: CString,
    identity: FileIdentity,
}

fn create_temp_at_named_with_context_hook(
    directory: &fs::File,
    destination: &OsStr,
    content: &[u8],
    model: &FileIdentity,
    selinux_source: Option<&fs::File>,
    fixed_name: Option<&str>,
    context_hook: impl FnOnce(Option<&fs::File>, &fs::File),
) -> std::io::Result<CreatedTemp> {
    use std::io::{Error, ErrorKind};

    let name = fixed_name.map(str::to_owned).unwrap_or_else(|| {
        format!(
            ".{}.facelock-{}-{}",
            destination.to_string_lossy(),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        )
    });
    let c_name = CString::new(name)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "temporary name contains NUL"))?;
    // SAFETY: the directory and C string remain live. The returned fd is
    // checked and transferred once into `File`.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            model.mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let mut created_identity = None;
    let result = (|| -> std::io::Result<()> {
        file.write_all(content)?;
        apply_owner_then_mode(&file, model.uid, model.gid, model.mode)?;
        context_hook(selinux_source, &file);
        file.sync_all()?;
        let identity = identity_of_open(&file)?;
        if identity.links != 1
            || identity.sha256 != sha256_hex(content)
            || identity.mode != model.mode
            || identity.uid != model.uid
            || identity.gid != model.gid
        {
            return Err(Error::other(
                "created PAM temp does not match its requested content and metadata",
            ));
        }
        created_identity = Some(identity);
        #[cfg(test)]
        run_temp_creation_test_hook(&file)?;
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup_identity = created_identity.or_else(|| identity_of_open(&file).ok());
        let Some(cleanup_identity) = cleanup_identity else {
            return Err(ambiguous_publication(
                "created PAM temp identity became ambiguous after an error",
            ));
        };
        let name = c_name.to_str().map_err(|_| {
            ambiguous_publication("created PAM temp name became ambiguous after an error")
        })?;
        cleanup_unpublished_temp_at(
            directory,
            name,
            &cleanup_identity,
            MAX_BACKUP_BYTES,
            "created PAM temp became ambiguous during error cleanup",
        )?;
        return Err(error);
    }
    Ok(CreatedTemp {
        name: c_name,
        identity: created_identity.ok_or_else(|| {
            ambiguous_publication("created PAM temp identity is missing after creation")
        })?,
    })
}

#[cfg(test)]
fn replace_existing_verified_with_hook(
    path: &Path,
    expected: &FileIdentity,
    content: &[u8],
    after_temp: impl FnOnce(),
) -> std::io::Result<()> {
    replace_existing_verified_with_hooks(
        path,
        expected,
        content,
        None,
        |_| {
            after_temp();
            Ok(())
        },
        || {},
        || Ok(()),
    )
}

#[cfg(test)]
fn replace_existing_verified_with_publish_hook(
    path: &Path,
    expected: &FileIdentity,
    content: &[u8],
    before_exchange: impl FnOnce(),
) -> std::io::Result<()> {
    replace_existing_verified_with_hooks(
        path,
        expected,
        content,
        None,
        |_| Ok(()),
        before_exchange,
        || Ok(()),
    )
}

fn replace_existing_verified_with_hooks(
    path: &Path,
    expected: &FileIdentity,
    content: &[u8],
    fixed_temp: Option<&str>,
    after_temp: impl FnOnce(&FileIdentity) -> std::io::Result<()>,
    before_exchange: impl FnOnce(),
    after_exchange: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "PAM path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "PAM path has no file name"))?;
    let directory = open_directory_nofollow(parent)?;
    let source = open_regular_at(&directory, name)?;
    if !identity_matches(expected, &identity_of_open(&source)?) {
        return Err(Error::other("PAM service changed after it was planned"));
    }

    let created = create_temp_at_named_with_context_hook(
        &directory,
        name,
        content,
        expected,
        Some(&source),
        fixed_temp,
        |source, destination| {
            if let Some(source) = source {
                copy_selinux_context_fd(source, destination);
            }
        },
    )?;
    let temp = created.name;
    let replacement = open_regular_at(&directory, OsStr::from_bytes(temp.as_bytes()))
        .map_err(|_| ambiguous_publication("created PAM temp became ambiguous before binding"))?;
    let replacement_identity = identity_of_open(&replacement)
        .map_err(|_| ambiguous_publication("created PAM temp became ambiguous before binding"))?;
    if !identity_matches(&created.identity, &replacement_identity) {
        return Err(ambiguous_publication(
            "created PAM temp changed before publication binding",
        ));
    }
    after_temp(&replacement_identity)?;
    let publish_check = open_regular_at(&directory, name)
        .and_then(|source| identity_of_open(&source))
        .and_then(|actual| {
            identity_matches(expected, &actual)
                .then_some(())
                .ok_or_else(|| Error::other("PAM service changed after it was planned"))
        });
    if let Err(error) = publish_check {
        let temp_name = temp
            .to_str()
            .map_err(|_| ambiguous_publication("unpublished PAM temp name became ambiguous"))?;
        cleanup_unpublished_temp_at(
            &directory,
            temp_name,
            &replacement_identity,
            MAX_BACKUP_BYTES,
            "unpublished PAM temp became ambiguous after source drift",
        )?;
        return Err(error);
    }
    let destination = CString::new(name.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "file name contains NUL"))?;
    before_exchange();
    // SAFETY: both basenames are confined to the same live directory fd.
    // EXCHANGE makes the exact inode displaced by publication available at
    // `temp` for post-publication validation and lossless rollback.
    let exchanged = unsafe {
        libc::renameat2(
            directory.as_raw_fd(),
            temp.as_ptr(),
            directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if exchanged != 0 {
        let error = std::io::Error::last_os_error();
        let temp_name = temp
            .to_str()
            .map_err(|_| ambiguous_publication("unpublished PAM temp name became ambiguous"))?;
        cleanup_unpublished_temp_at(
            &directory,
            temp_name,
            &replacement_identity,
            MAX_BACKUP_BYTES,
            "unpublished PAM temp became ambiguous after exchange failure",
        )?;
        return Err(error);
    }
    after_exchange()?;

    let published = open_regular_at(&directory, name)
        .and_then(|file| identity_of_open(&file))
        .map_err(|_| ambiguous_publication("published PAM service became ambiguous"))?;
    if !identity_matches(&replacement_identity, &published) {
        return Err(ambiguous_publication(
            "published PAM service changed before final validation",
        ));
    }

    let displaced = open_regular_at(&directory, OsStr::from_bytes(temp.as_bytes()));
    let displaced_identity = displaced
        .as_ref()
        .ok()
        .and_then(|file| identity_of_open(file).ok());
    if !displaced_identity
        .as_ref()
        .is_some_and(|actual| identity_matches(expected, actual))
    {
        // SAFETY: the same two live dirfd-relative basenames just exchanged.
        // A second atomic exchange restores the intervening canonical entry.
        if unsafe {
            libc::renameat2(
                directory.as_raw_fd(),
                temp.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        } != 0
        {
            return Err(Error::other(format!(
                "PAM service changed after it was planned and rollback failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        if let Ok(restored_temp) = open_regular_at(&directory, OsStr::from_bytes(temp.as_bytes()))
            && identity_matches(&replacement_identity, &identity_of_open(&restored_temp)?)
        {
            // SAFETY: verified replacement temp below the live directory.
            unsafe { libc::unlinkat(directory.as_raw_fd(), temp.as_ptr(), 0) };
        }
        directory.sync_all()?;
        return Err(Error::other("PAM service changed after it was planned"));
    }

    let published = open_regular_at(&directory, name)
        .and_then(|file| identity_of_open(&file))
        .map_err(|_| ambiguous_publication("published PAM service became ambiguous"))?;
    if !identity_matches(&replacement_identity, &published) {
        return Err(ambiguous_publication(
            "published PAM service changed before displaced cleanup",
        ));
    }

    // `temp` is the verified inode actually displaced by the exchange. Keep
    // its checked descriptor live through unlink so no path from provenance
    // or a second directory traversal participates.
    // SAFETY: confined temp basename under the live directory.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), temp.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    directory.sync_all()
}

#[cfg(test)]
fn create_override_verified_with_context_hook(
    target: &Target,
    expected: &FileIdentity,
    content: &[u8],
    context_hook: impl FnOnce(Option<&fs::File>, &fs::File),
) -> std::io::Result<()> {
    create_override_verified_with_hooks(
        target,
        expected,
        content,
        None,
        |_| Ok(()),
        || Ok(()),
        context_hook,
    )
}

fn create_override_verified_with_hooks(
    target: &Target,
    expected: &FileIdentity,
    content: &[u8],
    fixed_temp: Option<&str>,
    after_temp: impl FnOnce(&FileIdentity) -> std::io::Result<()>,
    after_publish: impl FnOnce() -> std::io::Result<()>,
    context_hook: impl FnOnce(Option<&fs::File>, &fs::File),
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let source_parent = target
        .path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "vendor path has no parent"))?;
    let source_name = target
        .path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "vendor path has no file name"))?;
    let source_directory = open_directory_nofollow(source_parent)?;
    let source = open_regular_at(&source_directory, source_name)?;
    if !identity_matches(expected, &identity_of_open(&source)?) {
        return Err(Error::other(
            "vendor PAM service changed after it was planned",
        ));
    }

    let destination = target.write_path();
    let destination_parent = destination
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "override path has no parent"))?;
    let destination_name = destination
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "override path has no file name"))?;
    let destination_directory = open_directory_nofollow(destination_parent)?;
    let created = create_temp_at_named_with_context_hook(
        &destination_directory,
        destination_name,
        content,
        expected,
        None,
        fixed_temp,
        context_hook,
    )?;
    let temp = created.name;
    let replacement = open_regular_at(&destination_directory, OsStr::from_bytes(temp.as_bytes()))
        .map_err(|_| {
        ambiguous_publication("created vendor temp became ambiguous before binding")
    })?;
    let replacement_identity = identity_of_open(&replacement).map_err(|_| {
        ambiguous_publication("created vendor temp became ambiguous before binding")
    })?;
    if !identity_matches(&created.identity, &replacement_identity) {
        return Err(ambiguous_publication(
            "created vendor temp changed before publication binding",
        ));
    }
    after_temp(&replacement_identity)?;
    let publish_check = open_regular_at(&source_directory, source_name)
        .and_then(|source| identity_of_open(&source))
        .and_then(|actual| {
            identity_matches(expected, &actual)
                .then_some(())
                .ok_or_else(|| Error::other("vendor PAM service changed after it was planned"))
        });
    if let Err(error) = publish_check {
        let temp_name = temp
            .to_str()
            .map_err(|_| ambiguous_publication("unpublished vendor temp name became ambiguous"))?;
        cleanup_unpublished_temp_at(
            &destination_directory,
            temp_name,
            &replacement_identity,
            MAX_BACKUP_BYTES,
            "unpublished vendor temp became ambiguous after source drift",
        )?;
        return Err(error);
    }
    let destination_name = CString::new(destination_name.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "override name contains NUL"))?;
    // SAFETY: both names are basenames under one live directory. NOREPLACE is
    // the atomic check that an administrator-created override did not appear
    // between planning and publication.
    let renamed = unsafe {
        libc::renameat2(
            destination_directory.as_raw_fd(),
            temp.as_ptr(),
            destination_directory.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if renamed != 0 {
        let error = std::io::Error::last_os_error();
        let temp_name = temp
            .to_str()
            .map_err(|_| ambiguous_publication("unpublished vendor temp name became ambiguous"))?;
        cleanup_unpublished_temp_at(
            &destination_directory,
            temp_name,
            &replacement_identity,
            MAX_BACKUP_BYTES,
            "unpublished vendor temp became ambiguous after publication failure",
        )?;
        return Err(error);
    }
    after_publish()?;
    let published = open_regular_at(
        &destination_directory,
        OsStr::from_bytes(destination_name.as_bytes()),
    )
    .and_then(|file| identity_of_open(&file))
    .map_err(|_| ambiguous_publication("published vendor override became ambiguous"))?;
    if !identity_matches(&replacement_identity, &published) {
        return Err(ambiguous_publication(
            "published vendor override changed before final validation",
        ));
    }
    destination_directory.sync_all()
}

fn copy_selinux_context_fd(from: &fs::File, to: &fs::File) {
    const SELINUX_XATTR: &std::ffi::CStr = c"security.selinux";
    let mut context = [0u8; 256];
    // SAFETY: descriptors remain open and the buffer size is exact.
    let read = unsafe {
        libc::fgetxattr(
            from.as_raw_fd(),
            SELINUX_XATTR.as_ptr(),
            context.as_mut_ptr().cast(),
            context.len(),
        )
    };
    if read <= 0 {
        return;
    }
    // SAFETY: `read` initialized this many bytes and both descriptors remain
    // open for the call.
    if unsafe {
        libc::fsetxattr(
            to.as_raw_fd(),
            SELINUX_XATTR.as_ptr(),
            context.as_ptr().cast(),
            read as usize,
            0,
        )
    } != 0
    {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "could not carry the SELinux context onto the new PAM service file"
        );
    }
}

/// The `--json` `error` value for a service name confinement rejected.
///
/// A fixed C-locale string, deliberately **not** the rendered
/// [`PamMessage::PamInvalidServiceName`]: that goes through
/// [`crate::message::Message::localized`], and `--json` output must never
/// localize (see the "What must NOT come through here" list in
/// `crate::message`). `message::init` sets `LC_MESSAGES` from the environment,
/// so routing the localized text here would make a documented machine field
/// change with the operator's locale. The human still gets the localized
/// message, on stderr.
const INVALID_SERVICE_NAME: &str = "invalid service name";

/// The legacy `--json` `error` token for any symlinked service file. Fixed
/// C-locale text for the same reason
/// as [`INVALID_SERVICE_NAME`]; the human gets the localized message, which
/// names the target, on stderr.
///
/// **It is a fixed name for the class, not a rendering of the directory that
/// was violated.** It spells [`PAM_DIR`] because that is the directory the
/// case is about in practice and because a documented constant may not vary
/// with the machine — an entry in a vendor directory pointing outside *that*
/// directory reports this same token, and the human message
/// ([`Rejected::message`], which carries the base) is what names the real
/// one.
const SYMLINKED_OUT_OF_DIR: &str = "symlinked outside /etc/pam.d";

/// The `--json` `error` value for a service file with more than one name.
/// Fixed C-locale text, as above.
const HARD_LINKED: &str = "hard-linked service file";

// ---------------------------------------------------------------------------
// The search path
// ---------------------------------------------------------------------------

/// Where a service name is looked up, in order, and where a write may land.
///
/// A newtype rather than a bare slice for one reason: **the first entry is the
/// override directory**, the only one this module writes to, and that is an
/// invariant worth a constructor. An empty list would leave no directory to
/// write to at all, so [`PamDirs::new`] substitutes the defaults for one —
/// `config_dirs = []` is a mistake, not a request to disable the writer.
///
/// Every engine function takes it as a parameter, which is what lets the whole
/// writer be driven against a tempdir pair by an unprivileged test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PamDirs {
    dirs: Vec<PathBuf>,
    backup_dir: PathBuf,
}

impl PamDirs {
    /// The search path, or [`PamConfig`]'s default when the list cannot be
    /// used.
    ///
    /// Three ways it cannot be. **Empty** is a mistake rather than a request to
    /// disable the writer — there would be no directory left to write to. **A
    /// first entry that is also a later entry** (spelled twice, or reached
    /// through a symlink) collapses the override layer onto a read-only one,
    /// which is how "never write to a vendor directory" would stop being true
    /// without anyone editing this file. **Any non-absolute entry** poisons
    /// the whole list: a relative first
    /// entry would resolve the write target against the invoking shell's
    /// working directory, so `cd /tmp && sudo facelock pam add` would edit
    /// `/tmp/<dir>/sudo` and report it as though it were the real thing. Only
    /// root can write `/etc/facelock/config.toml`, so this is a mistake to
    /// catch rather than an attack to defend against, and catching it is the
    /// same policy a broken config gets: fall back to the default and say so.
    ///
    /// The whole list is rejected rather than the offending entry filtered
    /// out, because a list with a hole in it is not the search order anyone
    /// wrote down.
    ///
    /// [`PamConfig`]: facelock_core::config::PamConfig
    pub(crate) fn new(dirs: Vec<PathBuf>) -> Self {
        if dirs.is_empty() {
            return Self::default();
        }
        if let Some(relative) = dirs.iter().find(|dir| !dir.is_absolute()) {
            tracing::warn!(
                entry = %relative.display(),
                "ignoring [pam] config_dirs: every entry must be an absolute path"
            );
            return Self::default();
        }
        // The write directory may not *be* one of the read-only ones, however
        // it is spelled. `Origin::Local` is decided by comparing the current
        // base against the first entry, which is a comparison of paths, so
        // `["/etc/pam.d", "/etc/pam.d"]` — or a first entry that is a symlink
        // onto a later one — would make the vendor layer and the override
        // layer the same directory and quietly turn "never write to a vendor
        // directory" into a write to one. Canonicalized because that is the
        // question: two names for one directory are one directory.
        let canonical: Vec<PathBuf> = dirs
            .iter()
            .map(|dir| fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
            .collect();
        if let Some(alias) = canonical[1..].iter().find(|dir| **dir == canonical[0]) {
            tracing::warn!(
                first = %dirs[0].display(),
                alias = %alias.display(),
                "ignoring [pam] config_dirs: the override directory is also a search directory"
            );
            return Self::default();
        }
        PamDirs {
            dirs,
            backup_dir: PathBuf::from(PAM_BACKUPS_DIR),
        }
    }

    /// The machine's search path: `[pam] config_dirs` when the config file
    /// parses, the defaults when it does not.
    ///
    /// This is the module's own read of the config, and the exception is
    /// deliberate: `main` dispatches `pam` *ahead* of the process-wide parse
    /// precisely so a missing or broken config cannot be the thing that stops
    /// an operator editing `/etc/pam.d` (see the dispatch comment in
    /// `main.rs`). A config that does not parse therefore yields the default
    /// list rather than an error — the same policy `is-enrolled` has for its
    /// unprivileged path. The path itself is resolved by
    /// `facelock_core::paths::config_path`, which ignores `FACELOCK_CONFIG` in
    /// a privileged process, so the environment cannot redirect where a root
    /// `pam add` writes.
    pub(crate) fn system() -> Self {
        crate::resolved::ConfigLoad::read()
            .config()
            .map_or_else(Self::default, Self::from_config)
    }

    /// Fixed, compiled roots for config-independent machine-wide cleanup.
    fn system_cleanup() -> Self {
        PamDirs {
            dirs: PAM_CLEANUP_DIRS.iter().map(PathBuf::from).collect(),
            backup_dir: PathBuf::from(PAM_BACKUPS_DIR),
        }
    }

    /// The search path a config that has *already been parsed* names.
    ///
    /// `facelock status` holds a [`crate::resolved::ConfigLoad`] before it
    /// probes anything, so it reads `[pam] config_dirs` from that rather than
    /// opening the file a second time through [`PamDirs::system`] — which is
    /// also what keeps `health.rs` out of the canonical-config-read pin in
    /// `resolved.rs`.
    pub(crate) fn from_config(config: &facelock_core::Config) -> Self {
        Self::new(
            config
                .pam
                .config_dirs
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<PathBuf>>(),
        )
    }

    /// The directory writes land in: the first entry.
    fn overrides(&self) -> &Path {
        // `new` and `default` both guarantee a non-empty list; the fallback
        // keeps the guarantee total rather than trusting it.
        self.dirs.first().map_or(Path::new(PAM_DIR), |dir| dir)
    }

    /// The directories, in search order.
    fn iter(&self) -> impl Iterator<Item = &Path> {
        self.dirs.iter().map(|dir| dir.as_path())
    }

    fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    /// The whole search path for a message that names where it looked.
    pub(crate) fn display(&self) -> String {
        self.dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<String>>()
            .join(", ")
    }
}

/// A one-directory search path: what every test that predates vendor
/// resolution means by "the PAM directory", and still the shape of a machine
/// with no vendor `pam.d`. Shared with `setup.rs`'s tests, which drive the
/// wizard's step 9 against a tempdir the same way.
#[cfg(test)]
pub(crate) fn only(dir: impl AsRef<Path>) -> PamDirs {
    PamDirs::from(dir.as_ref())
}

impl Default for PamDirs {
    fn default() -> Self {
        PamDirs {
            dirs: PAM_SYSTEM_DIRS.iter().map(PathBuf::from).collect(),
            backup_dir: PathBuf::from(PAM_BACKUPS_DIR),
        }
    }
}

/// One directory, which is what a test — and the setup wizard's tempdir-driven
/// step 9 — means by "the PAM directory".
impl From<&Path> for PamDirs {
    fn from(dir: &Path) -> Self {
        PamDirs {
            dirs: vec![dir.to_path_buf()],
            backup_dir: dir.join(".facelock-pam-backups"),
        }
    }
}

// ---------------------------------------------------------------------------
// Exit codes
// ---------------------------------------------------------------------------

/// `pam status`: every requested service carries the facelock line.
const STATUS_PRESENT: i32 = 0;
/// `pam status`: a requested service file exists without the line.
const STATUS_MISSING: i32 = 1;
/// `pam status`: a requested service file is absent, unreadable, or misnamed.
const STATUS_ERROR: i32 = 2;

/// `pam add` / `pam remove`: every service reached its requested state.
const WRITE_OK: i32 = 0;
/// `pam add` / `pam remove`: at least one service could not be written.
const WRITE_FAILED: i32 = 1;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// Which verb ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PamAction {
    Add,
    Remove,
    /// The default, so that a [`PamRequest`] built field by field reads and
    /// never writes until something names a verb on purpose.
    #[default]
    Status,
}

impl PamAction {
    /// The word this action reports as `"command"` in `--json`. Part of the
    /// output contract, so it is spelled here once rather than at the call
    /// site.
    fn word(self) -> &'static str {
        match self {
            PamAction::Add => "add",
            PamAction::Remove => "remove",
            PamAction::Status => "status",
        }
    }
}

/// A resolved `facelock pam` invocation.
///
/// Plain data, like [`crate::commands::setup::SetupArgs`]: the clap types stay
/// in the binary (`args.rs`), so this is what the library sees and what tests
/// construct. Fields that do not apply to an action are ignored by it —
/// `allow_sensitive` by `remove` and `status`, `dry_run` by `status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PamRequest {
    pub action: PamAction,
    /// Requested services, in the order given. Empty means
    /// [`DEFAULT_PAM_SERVICE`].
    pub services: Vec<String>,
    /// Enumerating forms: report (`status`) or clean (`remove`) every service
    /// on the applicable search path that carries the facelock line instead
    /// of acting on named services.
    ///
    /// A flag rather than a new meaning for a bare `pam status`, because that
    /// invocation's exit code is 0/1/2 *about `sudo`* today and an integrator
    /// may already branch on it; making it 0/1/2 about whatever happens to be
    /// on the machine would change an answer without changing a command line.
    /// It is mutually exclusive with `services` — enumerating and naming are
    /// two different questions, and a request that did both would have to pick
    /// one silently.
    pub all: bool,
    /// Suppress prompts. **Never** unlocks [`SENSITIVE_SERVICES`].
    ///
    /// `--json` implies it (the conversion in the binary's `args.rs` sets it):
    /// a prompt on stderr in front of a document a `jq` pipeline is waiting
    /// for is a hang, and the machine caller is by definition unattended.
    pub no_confirm: bool,
    /// Accept the risk of editing a [`SENSITIVE_SERVICES`] entry.
    pub allow_sensitive: bool,
    /// Treat a missing service file as success rather than an error.
    pub if_present: bool,
    /// Report the resolved plan and write nothing.
    pub dry_run: bool,
    /// Preserve Facelock-owned and legacy backups during `remove`.
    pub keep_backup: bool,
    /// Emit one JSON document on stdout instead of human text.
    pub json: bool,
}

/// Which write is running. [`PamAction::Status`] is not a write, and this is
/// where that stops being something every function downstream has to re-check.
///
/// Past the one conversion in [`WriteAction::of`], a plan, an apply and a
/// report cannot be handed a request that only asked to read — so the dead
/// arms that used to answer "what does an add do with a status?" are not
/// omissions here, they are unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteAction {
    Add,
    Remove,
}

impl WriteAction {
    /// The write this action asks for, or `None` for `status`.
    fn of(action: PamAction) -> Option<Self> {
        match action {
            PamAction::Add => Some(WriteAction::Add),
            PamAction::Remove => Some(WriteAction::Remove),
            PamAction::Status => None,
        }
    }
}

/// One write, as everything the two phases need that is not the target list.
///
/// It exists so [`plan_writes`] and [`apply_all`] take *one* value describing
/// the run instead of the same facts spread over three parameters in two
/// orders. That spread was the live defect: `install_for_setup(services,
/// no_confirm, allow_sensitive)` and `install_one_in(base, service,
/// allow_sensitive, no_confirm)` both type-check when swapped, and the swap
/// silently unlocks the sensitive-service gate. Named fields, one construction
/// per entry point.
struct WriteRequest<'a> {
    action: WriteAction,
    request: &'a PamRequest,
    /// The flag that unlocks [`SENSITIVE_SERVICES`] **on this surface**:
    /// `--allow-sensitive` on both the verb and the `setup --pam` alias. The
    /// refusal has to name the flag the caller can actually reach.
    remedy: &'a str,
}

// ---------------------------------------------------------------------------
// Outcomes — the `--json` vocabulary
// ---------------------------------------------------------------------------

/// What happened to one service.
///
/// The `action` string of a `--json` service object. **These words are a
/// stability contract**: existing words keep their meaning and new ones may be
/// added, so a consumer must tolerate a word it does not know rather than
/// treat it as an error. They are spelled in one place on purpose — the whole
/// vocabulary is visible in [`Outcome::word`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// `add`: the line was written (or, under `--dry-run`, would be).
    Installed,
    /// `add`: the service resolved only in a vendor directory, so an
    /// `/etc/pam.d` copy carrying the line was created from it (or would be).
    ///
    /// A new word rather than `installed`, because what happened to the
    /// machine is different: a file that did not exist now shadows a
    /// package-owned one and will not track its updates.
    Overridden,
    /// `remove`: the line was deleted (or would be).
    Removed,
    /// The service resolves only in a vendor directory and nothing was
    /// written. What `remove` reports (there is nothing of facelock's to take
    /// out of a file it never wrote) and what `status` reports for a service
    /// that has no `/etc/pam.d` copy and no facelock line.
    ///
    /// **Not `absent`.** The service file exists; it is the local override
    /// that does not, and `add` would create one. Overloading `absent` would
    /// change the meaning of a word integrators already branch on.
    VendorOnly,
    /// The service was already in the requested state.
    Unchanged,
    /// The service file does not exist.
    ///
    /// It is a success on `add` and `remove` — the verb asked for a state the
    /// file cannot be in, and `--if-present` said that is fine — and on
    /// `status` it is an error (exit 2) unless `--if-present` was given there
    /// too, because "is this service configured?" has no answer for a service
    /// that is not installed.
    Absent,
    /// The operator answered no at the per-file confirmation.
    Declined,
    /// The write failed. Carries the error for the `"error"` field.
    ///
    /// That field is a diagnostic, not a contract: `Failed`, `CleanupFailed`
    /// and `Unknown` interpolate an `io::Error`, whose text comes from the C
    /// library's `strerror` and therefore follows `LC_MESSAGES` like any other
    /// OS string. A consumer branches on `action`; it must not match on
    /// `error`.
    Failed(String),
    /// `remove`: the PAM service reached its requested state, but the default
    /// cleanup of Facelock-owned rollback state did not complete. This remains
    /// a write failure: callers must retry or use `--keep-backup` explicitly.
    CleanupFailed(String),
    /// `status`: the file exists and carries a facelock line.
    Present,
    /// `status`: the file exists and carries no facelock line.
    Missing,
    /// `status`: the file exists but could not be read.
    Unknown(String),
}

impl Outcome {
    fn word(&self) -> &'static str {
        match self {
            Outcome::Installed => "installed",
            Outcome::Overridden => "overridden",
            Outcome::VendorOnly => "vendor-only",
            Outcome::Removed => "removed",
            Outcome::Unchanged => "unchanged",
            Outcome::Absent => "absent",
            Outcome::Declined => "declined",
            Outcome::Failed(_) => "failed",
            Outcome::CleanupFailed(_) => "cleanup-failed",
            Outcome::Present => "present",
            Outcome::Missing => "missing",
            Outcome::Unknown(_) => "unknown",
        }
    }

    /// The `"error"` field, when this outcome carries one.
    fn error(&self) -> Option<&str> {
        match self {
            Outcome::Failed(error) | Outcome::CleanupFailed(error) | Outcome::Unknown(error) => {
                Some(error)
            }
            _ => None,
        }
    }
}

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceReport {
    service: String,
    /// The file this row is about, or `None` when the *name* was rejected and
    /// no path was ever resolved. Reporting `/etc/pam.d/../escape` there named
    /// a path nothing went near, which reads as a path that was acted on.
    path: Option<String>,
    outcome: Outcome,
    /// The newest validated committed backup path, when one exists after the
    /// operation. Legacy adjacent backups remain a reporting fallback. `null`
    /// otherwise — including for every `--dry-run` service, which writes no
    /// backup.
    backup: Option<String>,
    /// The vendor file this row's `/etc` entry hides, when it hides one.
    ///
    /// The fact `status` needs to say "configured, and this copy will not
    /// track the package's updates" — and the reason it is on the row rather
    /// than derived by the reporter is that [`Target::locate`] is where the
    /// search path is walked, so anywhere else would be a second walk that
    /// could disagree with the first. `None` on every machine with no vendor
    /// directory, which is why the `--json` key is omitted rather than
    /// emitted as `null`.
    shadows: Option<String>,
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// What a service is planned to become.
///
/// [`Plan::Rewrite`] carries the bytes it was derived from, so "there is an
/// edit to make" and "here is what it is being made from" cannot disagree.
/// **Which** edit is deliberately not recorded: the verb is
/// [`WriteRequest::action`], and a plan that named one too was a second copy
/// of it — the copy each `apply` function then had to check against its own,
/// with an unreachable "this plan is for the other verb" failure on the end of
/// both. Nor is the insertion point: it is a scan of `content`
/// ([`insertion_hint`]), and memoizing it made a third fact that could go
/// stale against the bytes beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// The file needs rewriting, from these bytes.
    Rewrite { content: Vec<u8> },
    /// The service resolved only in a vendor directory and `add` will create
    /// the local override from these bytes — the vendor file's, which the copy
    /// carries the facelock line and a provenance header on top of.
    ///
    /// A plan of its own rather than a flag beside [`Plan::Rewrite`]: the two
    /// write different files and print different things, and which one is
    /// happening is decided once, in [`plan_writes`], rather than re-derived
    /// by each applier from the target's origin.
    Override { content: Vec<u8> },
    /// `remove` found the exact Facelock-emitted local copy of a vendor
    /// service. The module line is removed through the normal crash-safe
    /// replacement first; the now-redundant override is then retired so the
    /// package-owned service becomes authoritative again.
    DeleteOverride { content: Vec<u8> },
    /// An exact Facelock header names one configured later-root candidate, but
    /// that source is currently absent and the local copy already has no
    /// Facelock rule. Keep it and report the missing source explicitly.
    RetainVendorOverride { vendor: PathBuf },
    /// The service resolves only in a vendor directory and this verb does not
    /// write there. `remove`'s answer, and a no-op.
    VendorOnly,
    /// Already in the requested state.
    NoChange,
    /// No service file, and `--if-present` said that is fine.
    Absent,
}

/// Which directory of the search path a name was found in.
///
/// The one fact that decides whether a write is an edit or a copy, kept beside
/// the path rather than re-derived by comparing prefixes at each use site.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// Found in the override directory, so the file is edited in place. Every
    /// service on a machine with no vendor `pam.d` is this.
    Local,
    /// Found only in a vendor directory. Carries the override path a copy
    /// would be written to, since that — not [`Target::path`] — is where any
    /// write for this service lands.
    Vendor { override_path: PathBuf },
    /// Found in no directory at all. Carries every path tried, because the
    /// refusal has to name them: "not found in `/etc/pam.d`" is a misleading
    /// half-answer once there is more than one place to look.
    Nowhere { tried: Vec<PathBuf> },
}

/// A validated service: which file the name resolves to, where it was found,
/// and what is planned for it. Backup state is derived separately from the
/// confined service name.
///
/// [`Target::locate`] is the only place a service name becomes a path — the
/// join, the confinement rule, the symlink rule and the search order are
/// applied there and nowhere else — so `status` cannot answer about a
/// different file than `add` would write.
#[derive(Debug, Clone)]
struct Target {
    service: String,
    /// The file the name resolved to: the override file when one exists, the
    /// vendor file when only that does, and the path the override *would* have
    /// when nothing exists anywhere.
    path: PathBuf,
    origin: Origin,
    /// The vendor file an [`Origin::Local`] hit hides, if it hides one.
    ///
    /// Not folded into [`Origin`]: `Local` is the fact that decides *how to
    /// write* — in place, never a copy — and that decision is the same whether
    /// or not a package also ships the name. What shadowing decides is what to
    /// *say*, and only `status` says it.
    shadowed: Option<PathBuf>,
    identity: Option<FileIdentity>,
    plan: Plan,
}

impl Target {
    /// Resolve one service against `dirs`, or say why it cannot be.
    ///
    /// The plan starts as [`Plan::NoChange`], which is the truth for a caller
    /// that is not going to write — `status` uses this and leaves it alone.
    /// [`plan_writes`] fills it in.
    fn locate(dirs: &PamDirs, service: &str) -> Result<Self, Rejected> {
        if confined(service).is_err() {
            return Err(Rejected::Name);
        }
        let (path, origin) = resolve_service_path(dirs, service)?;
        Ok(Target {
            service: service.to_string(),
            shadowed: shadowed_vendor(dirs, service, &origin),
            path,
            origin,
            identity: None,
            plan: Plan::NoChange,
        })
    }

    /// The file a write for this target lands in: the resolved file for an
    /// in-place edit, the override path for a vendor copy. Never a vendor
    /// directory.
    fn write_path(&self) -> &Path {
        write_target(&self.path, &self.origin)
    }

    fn path_string(&self) -> String {
        self.path.display().to_string()
    }

    /// The `path` field of this target's report row: the file the operation
    /// acted on. That is the resolved file for every plan but
    /// [`Plan::Override`], whose subject is the override it creates rather
    /// than the vendor file it read.
    fn reported_path(&self) -> String {
        match self.plan {
            Plan::Override { .. } => self.write_path().display().to_string(),
            _ => self.path_string(),
        }
    }

    /// Whether the service file exists anywhere on the search path.
    fn exists(&self) -> bool {
        !matches!(self.origin, Origin::Nowhere { .. })
    }

    /// Every path the resolver looked at, for the not-found refusal. A service
    /// that *was* found and then vanished — deleted between the resolve and
    /// the read — reports the one path it was found at, which is the only one
    /// that would tell the operator anything.
    fn tried_paths(&self) -> String {
        match &self.origin {
            Origin::Nowhere { tried } => tried
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<String>>()
                .join(", "),
            Origin::Local | Origin::Vendor { .. } => self.path_string(),
        }
    }

    /// The backup path if it exists on disk, for the report's `backup` field.
    fn existing_backup(&self) -> Option<String> {
        existing_backup_for(self.write_path())
    }

    /// The row's `shadows` field: the vendor file this entry hides.
    fn shadows_string(&self) -> Option<String> {
        self.shadowed
            .as_ref()
            .map(|vendor| vendor.display().to_string())
    }
}

/// The package-owned file an `/etc` entry hides, if the same name also exists
/// further down the search path.
///
/// Only an [`Origin::Local`] hit can shadow anything: `Vendor` *is* the
/// package's file, and `Nowhere` is no file at all. The probe is one `lstat`
/// per remaining directory and it deliberately does **not** look inside the
/// files — an entry that exists is enough to hide the name from Linux-PAM,
/// whatever it contains.
fn shadowed_vendor(dirs: &PamDirs, service: &str, origin: &Origin) -> Option<PathBuf> {
    if !matches!(origin, Origin::Local) {
        return None;
    }
    dirs.iter()
        .skip(1)
        .map(|base| base.join(service))
        .find(|path| fs::symlink_metadata(path).is_ok())
}

/// Where a write lands, as a free function so [`Target::locate`] can derive the
/// backup path before the `Target` exists.
fn write_target<'a>(path: &'a Path, origin: &'a Origin) -> &'a Path {
    match origin {
        Origin::Vendor { override_path } => override_path,
        Origin::Local | Origin::Nowhere { .. } => path,
    }
}

/// Why a service could not be resolved.
///
/// Both phases reject the same three things and report them differently —
/// [`plan_writes`] as an `Err` that stops the whole run, [`status_reports`] as
/// an `unknown` row beside the services it could answer about — so the reason
/// is a value here rather than a rendered message at each site. Every piece
/// that differs between them (the fixed machine reason, the localized text,
/// whether a `path` was resolved, whether a backup is worth probing for) hangs
/// off it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rejected {
    /// The name is not one path component, so nothing was resolved at all.
    Name,
    /// The entry is a symlink. `target` is diagnostic link text only; it is
    /// never resolved or opened.
    OutOfBase {
        link: PathBuf,
        target: PathBuf,
        base: PathBuf,
    },
    /// The entry is a regular file reachable by more than one name. Which
    /// other names is exactly what a link count does not say, so the edit
    /// cannot be shown to stay inside the directory.
    HardLinked { link: PathBuf, links: u64 },
}

impl Rejected {
    /// The `--json` `error` value: fixed C-locale text, never the localized
    /// message. See [`INVALID_SERVICE_NAME`].
    fn reason(&self) -> &'static str {
        match self {
            Rejected::Name => INVALID_SERVICE_NAME,
            Rejected::OutOfBase { .. } => SYMLINKED_OUT_OF_DIR,
            Rejected::HardLinked { .. } => HARD_LINKED,
        }
    }

    /// The localized refusal, for a human on stderr.
    fn message(&self, service: &str) -> PamMessage {
        match self {
            Rejected::Name => PamMessage::PamInvalidServiceName {
                service: service.to_string(),
            },
            Rejected::OutOfBase { link, target, base } => PamMessage::PamServiceSymlinkedOutside {
                path: link.display().to_string(),
                target: target.display().to_string(),
                dir: base.display().to_string(),
            },
            Rejected::HardLinked { link, links } => PamMessage::PamServiceHardLinked {
                path: link.display().to_string(),
                links: links.to_string(),
            },
        }
    }

    /// The entry this is about, when there is one: a real, confined path that
    /// was `lstat`ed. `None` for a rejected *name*, where nothing was resolved.
    fn link(&self) -> Option<&Path> {
        match self {
            Rejected::Name => None,
            Rejected::OutOfBase { link, .. } | Rejected::HardLinked { link, .. } => {
                Some(link.as_path())
            }
        }
    }

    /// The row's `path`. `None` for a rejected name: naming the path it
    /// *would* have resolved to reads as one that was acted on.
    fn path(&self) -> Option<String> {
        Some(self.link()?.display().to_string())
    }

    /// The row's `backup`. Probed for a rejected entry, because a facelock
    /// version that wrote through it left one there and it is what a recovery
    /// needs; not probed for a rejected name, which would `stat`
    /// `/etc/pam.d/../escape.facelock-backup` for a name the whole point of
    /// [`confined`] is to not act on.
    fn backup(&self) -> Option<String> {
        existing_backup_for(self.link()?)
    }
}

/// The `.facelock-backup` beside `path`, if one is on disk. Both report paths
/// derive the field the same way, so they cannot disagree about what `backup`
/// means.
fn existing_backup_for(path: &Path) -> Option<String> {
    let backup = backup_path(path);
    fs::symlink_metadata(&backup)
        .ok()
        .filter(|meta| meta.file_type().is_file() && meta.nlink() == 1)
        .map(|_| backup.display().to_string())
}

fn remove_legacy_backup(dirs: &PamDirs, service: &str) -> std::io::Result<()> {
    remove_legacy_backup_with_hook(dirs, service, || {})
}

fn remove_legacy_backup_with_hook(
    dirs: &PamDirs,
    service: &str,
    before_recheck: impl FnOnce(),
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    confined(service).map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid service"))?;
    let directory = open_directory_nofollow(dirs.overrides())?;
    let name = format!("{service}{BACKUP_SUFFIX}");
    let initial = match open_regular_at(&directory, OsStr::new(&name)) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let expected = identity_of_open(&initial)?;
    before_recheck();
    let current = open_regular_at(&directory, OsStr::new(&name))?;
    if !identity_matches(&expected, &identity_of_open(&current)?) {
        return Err(Error::other("legacy PAM backup changed before cleanup"));
    }
    let name = CString::new(name)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "legacy name contains NUL"))?;
    // SAFETY: the exact confined legacy basename is removed relative to the
    // already-open override root; no recorded path participates.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    directory.sync_all()
}

fn cleanup_backups(dirs: &PamDirs, service: &str) -> std::io::Result<()> {
    if let Some(store) = BackupStore::open_existing(dirs.backup_dir())? {
        store.cleanup(service)?;
    }
    remove_legacy_backup(dirs, service)
}

fn reported_backup(dirs: &PamDirs, target: &Target) -> Option<String> {
    BackupStore::open_existing(dirs.backup_dir())
        .ok()
        .flatten()
        .and_then(|store| store.latest_committed(&target.service).ok().flatten())
        .map(|path| path.display().to_string())
        .or_else(|| target.existing_backup())
}

/// A PAM service name is **one path component** under [`PAM_DIR`].
///
/// Rejected before any I/O, on `add`, `remove` and `status` alike: empty,
/// containing `/`, equal to `.` or `..`, or carrying an interior NUL. This is
/// the check the old writer did not have — `pam_install_in` did a bare
/// `base.join(service)`, and an absolute `service` *replaces* `base`, so
/// `--service /etc/shadow` resolved to `/etc/shadow`; `pam_remove_in` stripped
/// a leading `/` and nothing else, which left `..` intact.
fn confined(service: &str) -> anyhow::Result<()> {
    let mut components = Path::new(service).components();
    // A trailing slash is dropped by `components`, so the round-trip
    // comparison is what rejects `sudo/` as well as `a/b` and `/etc/shadow`.
    let single_component = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(name)), None) if name == OsStr::new(service)
    );
    if single_component && !service.contains('\0') {
        return Ok(());
    }
    Err(fail(PamMessage::PamInvalidServiceName {
        service: service.to_string(),
    }))
}

/// The file a validated `service` names on the search path, and which
/// directory it came from — or the reason it cannot be written through.
///
/// **First hit wins, and a hit that is refused is still a hit.** Each
/// directory is asked in turn; the first one holding an entry for the name
/// answers, whether the answer is a path or a refusal. Falling through to the
/// next directory after a refusal would let a vendor file silently take over
/// from an `/etc` entry this declined to follow, which is the opposite of what
/// the refusal is for.
///
/// `confined` checks the *name*; this checks what the filesystem does with it,
/// and there are two ways for a well-formed name to reach a file this cannot
/// account for.
///
/// **Symlinks.** Every one is refused, including links whose text appears to
/// remain inside `base`. Planning and applying both open the service basename
/// relative to an already-open PAM root with `O_NOFOLLOW`; neither a resolved
/// absolute target nor a target from provenance is ever opened.
///
/// **Hard links.** A symlink can be followed to somewhere and checked. A
/// second *hard* link cannot: `nlink > 1` says another name for this inode
/// exists and says nothing about where, so an edit here silently changes a
/// file this cannot name — the confinement rule's whole subject. The atomic
/// replace does not retire the rule, as this comment once said it would: a
/// rename writes a *new* inode, so it does not corrupt the other name, it
/// silently leaves it holding the old content — an operator who asked for one
/// file to carry the line gets one of its names carrying it and the rest not,
/// which is a worse answer than a refusal. A `/etc` that has been through a
/// deduplicating backup or `jdupes -L` can trip this without any adversary
/// involved, so the message says how to break the link.
fn resolve_service_path(dirs: &PamDirs, service: &str) -> Result<(PathBuf, Origin), Rejected> {
    let overrides = dirs.overrides();
    let mut tried = Vec::new();

    for base in dirs.iter() {
        let entry = base.join(service);
        let Some(resolved) = resolve_in(base, &entry) else {
            tried.push(entry);
            continue;
        };
        let path = resolved?;
        let origin = if base == overrides {
            Origin::Local
        } else {
            Origin::Vendor {
                override_path: overrides.join(service),
            }
        };
        return Ok((path, origin));
    }

    // Nothing anywhere. The path is where an override would go, which is what
    // every message about a service that is not installed should name; `tried`
    // is what the not-found refusal lists.
    Ok((overrides.join(service), Origin::Nowhere { tried }))
}

/// One directory's answer for one entry: `None` when there is nothing there,
/// otherwise the file to act on or the reason not to.
fn resolve_in(base: &Path, entry: &Path) -> Option<Result<PathBuf, Rejected>> {
    let metadata = match fs::symlink_metadata(entry) {
        Ok(metadata) => metadata,
        // Nothing here. This is the *only* reason the search moves on: an
        // absence is a fact about the directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        // An entry that exists but cannot be examined — `EACCES` on a
        // hardened `/etc/pam.d`, `EIO` or `ESTALE` on a network mount — is not
        // an absence, and treating it as one turns an honest failure into a
        // confident wrong answer: the service would resolve to the *vendor*
        // copy and `status` would report `vendor-only` for a service the
        // override configures. So the search stops here and the entry is
        // returned unexamined; the read that follows reports the error, which
        // is what the single-directory writer did with the same input
        // (`status` → `unknown`, exit 2; `add` → the read error).
        Err(_) => return Some(Ok(entry.to_path_buf())),
    };

    if !metadata.file_type().is_symlink() {
        return Some(hard_link_checked(entry.to_path_buf()));
    }

    // Provenance and plans name a service, never an arbitrary resolved path.
    // Refuse every symlink so the later openat(O_NOFOLLOW) re-resolution asks
    // about exactly that basename under exactly this PAM root.
    Some(Err(Rejected::OutOfBase {
        target: fs::read_link(entry).unwrap_or_else(|_| entry.to_path_buf()),
        link: entry.to_path_buf(),
        base: base.to_path_buf(),
    }))
}

/// `path`, or the refusal if the file it names has more than one name.
///
/// One implementation for both ways a name reaches a file — the entry itself
/// and the target of an in-directory symlink — because the rule is about the
/// *inode*, and under an atomic replace a second name is not a theoretical
/// concern: `rename` writes a new inode, so an edit made through one name
/// leaves every other name holding the old content. A `remove` that reported
/// `removed` while a hard-linked `password-auth-ac` kept the facelock line
/// would be a fail-open on the shared auth stack, reported as success.
///
/// A path this cannot `stat` is passed through: the read that follows reports
/// the error, which is the same answer every other unexaminable entry gets.
fn hard_link_checked(path: PathBuf) -> Result<PathBuf, Rejected> {
    use std::os::unix::fs::MetadataExt;

    let Ok(metadata) = fs::metadata(&path) else {
        return Ok(path);
    };
    // `is_file` matters: a directory's link count is its subdirectory count,
    // and a directory where a service file should be is a read error reported
    // as one, not a link fault.
    if metadata.is_file() && metadata.nlink() > 1 {
        return Err(Rejected::HardLinked {
            links: metadata.nlink(),
            link: path,
        });
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// What happened to one directory of the search path.
///
/// The distinction this type exists for is [`DirState::Unreadable`] against
/// [`DirState::Scanned`] with nothing in it: a directory that could not be
/// listed has told us **nothing**, and folding that into "no services here"
/// is how a broken lock stack and a healthy one came to render identically.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirState {
    /// Listed. Whatever it holds is in the scan's names.
    Scanned,
    /// Not there. A directory that does not exist demonstrably holds no
    /// service files, so this is a fact rather than the absence of one — and
    /// it is **not** an error: the default search path names a vendor
    /// directory many machines simply do not have, so treating its absence as
    /// unanswerable would make every one of them report exit 2 forever.
    Absent,
    /// There, and not listable — a permission, mount or I/O failure. The
    /// answer for this directory is unknown, so the scan's answer is
    /// incomplete and its exit code says so.
    Unreadable(String),
}

impl DirState {
    /// The word this state reports in `--json`.
    fn word(&self) -> &'static str {
        match self {
            DirState::Scanned => "scanned",
            DirState::Absent => "absent",
            DirState::Unreadable(_) => "unreadable",
        }
    }

    fn error(&self) -> Option<&str> {
        match self {
            DirState::Unreadable(error) => Some(error),
            DirState::Scanned | DirState::Absent => None,
        }
    }
}

/// One directory of the search path, and what came of listing it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryScan {
    path: PathBuf,
    state: DirState,
}

/// The result of walking the whole search path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scan {
    /// Every service **name** worth reporting on, sorted and de-duplicated.
    /// Names, not paths: which file a name resolves to is [`Target::locate`]'s
    /// answer and must not be decided twice.
    names: Vec<String>,
    /// Every directory searched, in search order, whether or not it yielded
    /// anything. In the document so a reader can see what was looked at
    /// rather than infer it.
    directories: Vec<DirectoryScan>,
}

impl Scan {
    /// The directories that stop this scan being a complete answer.
    fn unreadable(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.directories
            .iter()
            .filter_map(|dir| dir.state.error().map(|error| (dir.path.as_path(), error)))
    }

    /// The directories that produced an answer — listed, or proven not to
    /// exist. What "nothing is configured" may be said *about*: a directory
    /// this could not open supports no such claim.
    fn answered(&self) -> Vec<String> {
        self.directories
            .iter()
            .filter(|dir| dir.state.error().is_none())
            .map(|dir| dir.path.display().to_string())
            .collect()
    }
}

/// Names that live in a `pam.d` directory and are not services.
///
/// Every one of these can carry the facelock line — `sudo.facelock-backup` is
/// a byte copy of a configured file, and a `.pacsave` is the configuration a
/// removed package left behind — and none of them is a service Linux-PAM will
/// ever be asked for. Reporting them as configured services would be the
/// report being confidently wrong, which is the failure mode enumeration
/// exists to remove rather than add.
const NON_SERVICE_SUFFIXES: &[&str] = &[
    BACKUP_SUFFIX,
    ".pacnew",
    ".pacsave",
    ".pacorig",
    ".rpmnew",
    ".rpmsave",
    ".rpmorig",
    ".dpkg-old",
    ".dpkg-new",
    ".dpkg-dist",
    ".pam-old",
    "~",
];

/// Whether a directory entry's name is worth resolving as a service.
///
/// Dotfiles are excluded as a class: this module's own in-flight temp file is
/// `.<service>.facelock-<pid>-<nanos>`, and nothing else that starts with a
/// dot is a service anyone authenticates against.
fn is_service_name(name: &str) -> bool {
    !name.starts_with('.')
        && !NON_SERVICE_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
        && confined(name).is_ok()
}

/// Whether this file is one the scan should report on: it carries the facelock
/// line, or it could not be read to find out.
///
/// The second half is the point. Omitting a file this could not read would
/// report "not configured" for a machine that is configured — the exact
/// confident wrongness enumeration is for — so an unreadable entry is carried
/// into the report, where [`status_reports`] turns it into an `unknown` row
/// with the errno and exit 2. A file that vanished between the listing and the
/// read is not carried: it is gone, which is an answer.
///
/// **Only a regular file is read, and the metadata that decides it is the
/// *followed* one.** `fs::read` on a FIFO blocks until a writer appears,
/// which is forever on a `/etc/pam.d` nobody is writing to — and this scan is
/// what `facelock status` runs, so the command whose whole job is to report a
/// broken machine would hang on one. A socket or a device node is the same
/// class. The `is_dir` check at the call site cannot cover it: that one is
/// `lstat`, so a symlink to a directory or to a FIFO survives it. A
/// non-regular entry in a `pam.d` directory is not a service file under any
/// reading, so skipping it loses nothing.
///
/// **"Not a regular file" and "could not be examined" are different answers**,
/// and only the first is a skip. A `stat` that *fails* — a symlink into a
/// directory this may not traverse, a symlink loop, a dead network mount —
/// says nothing about what is there, so the entry falls through to the read,
/// which reports it. Treating a failed `stat` as "not a regular file" made an
/// entry the named form calls `unknown` vanish from `--all` entirely, which is
/// the same rule `resolve_in` already refuses to apply one layer down. It
/// cannot reintroduce the hang: the hang needs an `open` that blocks, and an
/// entry whose `stat` cannot resolve the path cannot `open` through it either.
fn worth_reporting(path: &Path) -> bool {
    match fs::metadata(path) {
        // Stat-ed, and it is not something a PAM stack could ever read.
        Ok(metadata) if !metadata.is_file() => return false,
        // Gone between the listing and now. Absence is an answer.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        // A regular file, or an entry this could not examine. Both go to the
        // read, which distinguishes them.
        _ => {}
    }
    match fs::read(path) {
        Ok(content) => PamDocument::new(&content).has_facelock_rule(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Walk the search path for every service that names `pam_facelock.so`.
///
/// Names are collected across **all** directories and resolved afterwards, so
/// a vendor file carrying the line puts its name in the report even when
/// `/etc/pam.d` shadows it — and the row then says `missing`, because the file
/// Linux-PAM actually reads has no line in it. Dropping the name instead would
/// hide precisely the machine an operator cannot otherwise explain.
fn scan_directories(dirs: &PamDirs) -> Scan {
    let mut names: Vec<String> = Vec::new();
    let mut directories = Vec::new();

    for base in dirs.iter() {
        let state = match fs::read_dir(base) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    // A subdirectory is not a service file. `file_type` here
                    // does not follow symlinks, so a link is kept and handed
                    // to `Target::locate`, which is what owns the rule about
                    // where a link may point.
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        continue;
                    }
                    // A name this cannot spell is a name it cannot resolve.
                    // `to_string_lossy` would substitute U+FFFD and hand
                    // `Target::locate` a name no file has, which reports a
                    // configured service as `absent` — a path that does not
                    // exist, presented as the answer. Skipped and logged
                    // instead: a PAM service name is looked up by a byte
                    // string PAM itself takes from a config file, and nothing
                    // that is not UTF-8 is a service this can act on.
                    let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                        tracing::warn!(
                            path = %entry.path().display(),
                            "skipping a PAM service file whose name is not valid UTF-8"
                        );
                        continue;
                    };
                    if !is_service_name(&name) || !worth_reporting(&entry.path()) {
                        continue;
                    }
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
                DirState::Scanned
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DirState::Absent,
            Err(error) => DirState::Unreadable(error.to_string()),
        };
        directories.push(DirectoryScan {
            path: base.to_path_buf(),
            state,
        });
    }

    // Sorted so two runs on one machine print the same report: `read_dir`
    // yields whatever order the filesystem happens to hold.
    names.sort();
    Scan { names, directories }
}

/// One service that carries the facelock line, as `facelock status` sees it.
///
/// Lives here rather than in `crate::health` because it is this module's
/// answer: the scan, the resolution and the read all belong to the writer, and
/// a second implementation in the health probe is the drift this replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredService {
    pub service: String,
    /// The file the name resolved to — the one Linux-PAM reads.
    pub path: String,
    /// The vendor file this `/etc` copy hides, when it hides one. A service
    /// configured this way will not track the package's updates.
    pub shadows: Option<String>,
}

/// A place the scan could not get an answer from: a directory it could not
/// list, or a service file it could not read.
///
/// Kept separate from the configured list rather than merged into it as a
/// falsy entry — "not checked" is not "not configured", and a report that
/// cannot tell them apart is the one this gap replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotChecked {
    pub path: String,
    pub error: String,
}

/// What `facelock status` reports in one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredScan {
    pub services: Vec<ConfiguredService>,
    pub not_checked: Vec<NotChecked>,
}

/// Everything on this machine that carries the facelock line, for
/// `facelock status`'s summary.
///
/// **The same scan `pam status --all` runs**, down to the row builder, so the
/// two commands cannot disagree about one service. The sink is silent because
/// `status` renders a report and the writer's per-service lines interleaved
/// into it would be output from a probe, not from the renderer.
pub(crate) fn configured_scan(dirs: &PamDirs) -> ConfiguredScan {
    let sink = Sink::silent();
    let scan = scan_directories(dirs);
    let reports = status_reports(dirs, &scan.names, &sink);

    let services = reports
        .iter()
        .filter(|report| report.outcome == Outcome::Present)
        .map(|report| ConfiguredService {
            service: report.service.clone(),
            path: report.path.clone().unwrap_or_default(),
            shadows: report.shadows.clone(),
        })
        .collect();

    // Both kinds of "could not tell", in one list: a directory that would not
    // list and a service file that would not read are the same fact about the
    // report — part of it is missing — and the summary line has to be able to
    // say so without knowing which.
    let not_checked = scan
        .unreadable()
        .map(|(path, error)| NotChecked {
            path: path.display().to_string(),
            error: error.to_string(),
        })
        .chain(reports.iter().filter_map(|report| {
            match &report.outcome {
                Outcome::Unknown(error) => Some(NotChecked {
                    path: report
                        .path
                        .clone()
                        .unwrap_or_else(|| report.service.clone()),
                    error: error.clone(),
                }),
                _ => None,
            }
        }))
        .collect();

    ConfiguredScan {
        services,
        not_checked,
    }
}

/// Refuse a [`SENSITIVE_SERVICES`] entry unless the caller accepted the risk.
///
/// Called twice per service, on the name as typed and again on the file that
/// name resolved to: the gate protects the *file*, and a symlink
/// `alias -> system-auth` inside `/etc/pam.d` is a way to reach a gated file
/// under a name the first check waves through. Only `add` is gated —
/// see [`SENSITIVE_SERVICES`].
fn gate_sensitive(name: &str, write: &WriteRequest) -> anyhow::Result<()> {
    if write.action != WriteAction::Add
        || write.request.allow_sensitive
        || !SENSITIVE_SERVICES.contains(&name)
    {
        return Ok(());
    }
    Err(fail(PamMessage::PamSensitiveRefused {
        service: name.to_string(),
        remedy: write.remedy.to_string(),
    }))
}

/// The last component of a resolved path, for the gate to test. A path with no
/// final component cannot be a service file, and the empty string is in no
/// list, so it falls through to the read that reports it.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Requested services, defaulted and de-duplicated, in the order given.
///
/// De-duplication is not cosmetic: `--service sudo --service sudo` would
/// otherwise emit two report rows for one file, the second of them
/// `unchanged` because the first had just written it.
fn requested_services(services: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for service in services {
        if !out.iter().any(|seen| seen == service) {
            out.push(service.clone());
        }
    }
    if out.is_empty() {
        out.push(DEFAULT_PAM_SERVICE.to_string());
    }
    out
}

/// Phase one: validate and read every requested service, or fail having
/// written nothing.
///
/// Every rejection here is an `Err`, not a report row, and the caller performs
/// no writes on `Err` — that is the whole point of the phase. Errors render on
/// stderr as text and never as a JSON document, the same contract
/// `is-enrolled` has for its unanswerable case.
///
/// The service list is resolved here rather than passed in: a caller that
/// handed in one list while the request named another had two lists to keep
/// in step, and `install_one_in` did exactly that — it acted on one service
/// while its request said "none, so `sudo`".
fn plan_writes(dirs: &PamDirs, write: &WriteRequest) -> anyhow::Result<Vec<Target>> {
    let services = requested_services(&write.request.services);
    let mut targets = Vec::with_capacity(services.len());

    for service in &services {
        // On the name as typed, before any I/O.
        gate_sensitive(service, write)?;

        let located = Target::locate(dirs, service).map_err(|why| fail(why.message(service)))?;
        // ...and again on the file the name reached — in whichever directory
        // that was. A link `alias -> system-auth` inside the directory is a
        // gated file behind an ungated name, and so is a vendor
        // `/usr/lib/pam.d/system-auth` reached under an ungated alias. The
        // gate is on the file.
        gate_sensitive(&file_name_of(&located.path), write)?;

        let display = located.path_string();

        let (content, identity) = match read_regular_nofollow(&located.path) {
            Ok((content, identity)) => (Some(content), Some(identity)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !write.request.if_present {
                    // Every path tried, not just the override directory's:
                    // "not found in /etc/pam.d" is a misleading half-answer on
                    // a machine that also has a vendor directory.
                    return Err(fail(PamMessage::PamFileNotFound {
                        paths: located.tried_paths(),
                    }));
                }
                (None, None)
            }
            // `--if-present` converts a missing file into a no-op and nothing
            // else: a permission or I/O failure stays fatal.
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!("failed to read {display}")));
            }
        };

        let plan = match (content, &located.origin) {
            (None, _) => Plan::Absent,
            // A service whose only copy is package-owned. `remove` has
            // nothing to do — it never wrote there — and says so rather than
            // reporting a file it did not touch as `unchanged`.
            (Some(_), Origin::Vendor { .. }) if write.action == WriteAction::Remove => {
                Plan::VendorOnly
            }
            // ...and `add` creates the local override instead of editing the
            // package's file, unless the vendor file already carries the line,
            // in which case there is nothing an override would add.
            (Some(content), Origin::Vendor { override_path }) => {
                if PamDocument::new(&content).has_facelock_rule() {
                    Plan::NoChange
                } else {
                    // Phase one, so the copy's destination is validated before
                    // anything is written — including under `--dry-run`, which
                    // is a preview of the real run and must not promise a write
                    // into a directory that cannot take one. The in-place edit
                    // has no equivalent check because its destination is the
                    // file it just read.
                    writable_override_dir(override_path)?;
                    Plan::Override { content }
                }
            }
            // The same question for both verbs — "does the line exist?" — and
            // opposite answers about whether that means work to do.
            (Some(content), Origin::Local | Origin::Nowhere { .. }) => {
                let present = PamDocument::new(&content).has_facelock_rule();
                if write.action == WriteAction::Remove {
                    let disposition = identity
                        .as_ref()
                        .map_or(VendorOverrideDisposition::NotFacelock, |identity| {
                            classify_vendor_override(dirs, &located, &content, identity)
                        });
                    match disposition {
                        VendorOverrideDisposition::Unchanged => Plan::DeleteOverride { content },
                        VendorOverrideDisposition::SourceAbsent(vendor) if !present => {
                            Plan::RetainVendorOverride { vendor }
                        }
                        _ if present => Plan::Rewrite { content },
                        _ => Plan::NoChange,
                    }
                } else if present {
                    Plan::NoChange
                } else {
                    Plan::Rewrite { content }
                }
            }
        };

        targets.push(Target {
            plan,
            identity,
            ..located
        });
    }

    Ok(targets)
}

/// Refuse in phase one if the override a vendor-only service needs cannot be
/// created — the directory is missing, or not writable by this process.
///
/// `faccessat` with `AT_EACCESS` rather than a mode check: what matters is
/// whether *this* process may write there, which mode bits alone do not answer
/// (root ignores them, a read-only mount overrides them).
fn writable_override_dir(override_path: &Path) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = override_path.parent().unwrap_or(Path::new("/"));
    let refuse = |error: std::io::Error| {
        fail(PamMessage::PamOverrideDirUnwritable {
            dir: dir.display().to_string(),
            path: override_path.display().to_string(),
            error: error.to_string(),
        })
    };

    let c_dir = CString::new(dir.as_os_str().as_bytes()).map_err(|_| {
        refuse(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains embedded NUL",
        ))
    })?;
    // SAFETY: `c_dir` is a NUL-terminated C string that outlives the call, and
    // `faccessat` only reads it.
    let ok = unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            c_dir.as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        )
    } == 0;
    if ok {
        return Ok(());
    }
    Err(refuse(std::io::Error::last_os_error()))
}

fn backup_path(path: &Path) -> PathBuf {
    // Built by appending to the string rather than with `set_extension`, which
    // would replace a service name's existing suffix.
    PathBuf::from(format!("{}{BACKUP_SUFFIX}", path.display()))
}

/// A PAM service file viewed as bytes and logical rules.
///
/// Linux-PAM permits horizontal whitespace after a continuation backslash,
/// and a `#` ends the semantic rule even when an earlier physical line asked
/// to continue. The parser below mirrors those boundaries while every edit
/// still copies the untouched raw spans byte for byte.
struct PamDocument<'a> {
    bytes: &'a [u8],
}

impl<'a> PamDocument<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn logical_rules(&self) -> LogicalRules<'a> {
        LogicalRules {
            bytes: self.bytes,
            next: 0,
        }
    }

    fn has_facelock_rule(&self) -> bool {
        self.logical_rules()
            .any(|rule| is_facelock_rule(rule.bytes))
    }

    fn auth_insertion_offset(&self) -> Option<usize> {
        self.logical_rules()
            .find(|rule| is_auth_rule(rule.bytes))
            .map(|rule| rule.start)
    }

    fn pam_header_end(&self) -> Option<usize> {
        let header = PhysicalLine::at(self.bytes, 0)?;
        (header.content() == b"#%PAM-1.0").then_some(header.end)
    }

    fn line_ending_from(&self, from: usize) -> Option<&'a [u8]> {
        let newline = self.bytes[from..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|relative| from + relative)?;
        if newline > from && self.bytes[newline - 1] == b'\r' {
            Some(&self.bytes[newline - 1..=newline])
        } else {
            Some(&self.bytes[newline..=newline])
        }
    }

    fn with_facelock_inserted(&self) -> Vec<u8> {
        let auth_offset = self.auth_insertion_offset();
        let header_end = if auth_offset.is_none() {
            self.pam_header_end()
        } else {
            None
        };
        let offset = auth_offset.or(header_end).unwrap_or(0);
        let ending_from = if auth_offset.is_some() { offset } else { 0 };
        let ending = self
            .line_ending_from(ending_from)
            .or_else(|| self.line_ending_from(0))
            .unwrap_or(b"\n");

        let mut output = Vec::with_capacity(self.bytes.len() + PAM_LINE.len() + 2);
        output.extend_from_slice(&self.bytes[..offset]);

        // A header with no newline needs a separator before an appended rule,
        // while the appended rule itself stays unterminated so the document's
        // no-final-newline property survives.
        if header_end == Some(self.bytes.len()) && !self.bytes.ends_with(b"\n") {
            output.extend_from_slice(ending);
            output.extend_from_slice(PAM_LINE.as_bytes());
            return output;
        }

        output.extend_from_slice(PAM_LINE.as_bytes());
        if !self.bytes.is_empty() {
            output.extend_from_slice(ending);
        }
        output.extend_from_slice(&self.bytes[offset..]);
        output
    }

    fn with_facelock_removed(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.bytes.len());
        let mut copied_through = 0;
        for rule in self.logical_rules() {
            if !is_facelock_rule(rule.bytes) {
                continue;
            }

            // Older facelock releases could inject the canonical physical
            // line after a continued administrator line. That structural
            // position, not the administrator rule's type, distinguishes the
            // legacy damage from a genuine Facelock logical rule.
            let mut next = rule.start;
            let mut previous_continued = false;
            let mut repaired_legacy_injection = false;
            while next < rule.end {
                let Some(line) = PhysicalLine::at(self.bytes, next) else {
                    break;
                };
                if previous_continued && line.content() == PAM_LINE.as_bytes() {
                    output.extend_from_slice(&self.bytes[copied_through..line.start]);
                    copied_through = line.end;
                    repaired_legacy_injection = true;
                }
                previous_continued = line.continuation_backslash().is_some();
                next = line.end;
            }
            if repaired_legacy_injection {
                continue;
            }

            output.extend_from_slice(&self.bytes[copied_through..rule.start]);
            copied_through = rule.end;
        }
        output.extend_from_slice(&self.bytes[copied_through..]);
        output
    }
}

#[derive(Clone, Copy)]
struct LogicalRule<'a> {
    start: usize,
    end: usize,
    bytes: &'a [u8],
}

struct LogicalRules<'a> {
    bytes: &'a [u8],
    next: usize,
}

#[derive(Clone, Copy)]
struct PhysicalLine<'a> {
    bytes: &'a [u8],
    start: usize,
    end: usize,
    content_end: usize,
}

impl<'a> PhysicalLine<'a> {
    fn at(bytes: &'a [u8], start: usize) -> Option<Self> {
        if start >= bytes.len() {
            return None;
        }
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |relative| start + relative + 1);
        let mut content_end = end;
        if bytes[content_end - 1] == b'\n' {
            content_end -= 1;
            if content_end > start && bytes[content_end - 1] == b'\r' {
                content_end -= 1;
            }
        }
        Some(Self {
            bytes,
            start,
            end,
            content_end,
        })
    }

    fn content(self) -> &'a [u8] {
        &self.bytes[self.start..self.content_end]
    }

    fn semantic(self) -> &'a [u8] {
        let content = self.content();
        let end = content
            .iter()
            .position(|byte| *byte == b'#')
            .unwrap_or(content.len());
        &content[..end]
    }

    fn has_semantic_content(self) -> bool {
        self.semantic()
            .iter()
            .any(|byte| !matches!(byte, b' ' | b'\t'))
    }

    fn continuation_backslash(self) -> Option<usize> {
        let content = self.content();
        if content.contains(&b'#') {
            return None;
        }
        let mut end = content.len();
        while end > 0 && matches!(content[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        (end > 0 && content[end - 1] == b'\\').then_some(end - 1)
    }
}

impl<'a> Iterator for LogicalRules<'a> {
    type Item = LogicalRule<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let first = loop {
            let line = PhysicalLine::at(self.bytes, self.next)?;
            if line.has_semantic_content() {
                break line;
            }
            self.next = line.end;
        };

        let start = first.start;
        let mut line = first;
        loop {
            if line.continuation_backslash().is_none() || line.end == self.bytes.len() {
                break;
            }
            let Some(next) = PhysicalLine::at(self.bytes, line.end) else {
                break;
            };
            // Linux-PAM consumes a blank/comment physical line to terminate
            // an assembled rule. Leaving it out of the raw rule span keeps
            // that administrator-owned line intact when the rule is removed.
            if !next.has_semantic_content() {
                break;
            }
            line = next;
        }

        let end = line.end;
        self.next = end;
        Some(LogicalRule {
            start,
            end,
            bytes: &self.bytes[start..end],
        })
    }
}

fn semantic_rule(rule: &[u8]) -> Vec<u8> {
    let mut semantic = Vec::with_capacity(rule.len());
    let mut next = 0;
    while let Some(line) = PhysicalLine::at(rule, next) {
        let segment = line.semantic();
        if let Some(backslash) = line.continuation_backslash() {
            semantic.extend_from_slice(&segment[..backslash]);
            semantic.push(b' ');
        } else {
            semantic.extend_from_slice(segment);
        }
        next = line.end;
    }
    semantic
}

fn first_token(semantic: &[u8]) -> Option<&[u8]> {
    let start = semantic
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())?;
    let end = semantic[start..]
        .iter()
        .position(u8::is_ascii_whitespace)
        .map_or(semantic.len(), |relative| start + relative);
    Some(&semantic[start..end])
}

fn is_auth_rule(rule: &[u8]) -> bool {
    let semantic = semantic_rule(rule);
    let Some(token) = first_token(&semantic) else {
        return false;
    };
    token
        .strip_prefix(b"-")
        .unwrap_or(token)
        .eq_ignore_ascii_case(b"auth")
}

fn is_facelock_rule(rule: &[u8]) -> bool {
    semantic_rule(rule)
        .windows(b"pam_facelock.so".len())
        .any(|window| window == b"pam_facelock.so")
}

/// Whether a PAM config line references pam_facelock, regardless of spacing.
///
/// Matches on the module name, not on the canonical line's bytes, so a
/// hand-edited line with different spacing is still recognized — and a
/// commented-out one is not.
pub fn is_facelock_pam_line(line: &str) -> bool {
    is_facelock_rule(line.as_bytes())
}

/// Where the line will land in content, as the message that says so.
fn insertion_hint(content: &[u8]) -> PamMessage {
    let document = PamDocument::new(content);
    if document.auth_insertion_offset().is_some() {
        PamMessage::PamInsertBeforeAuthHint
    } else if document.pam_header_end().is_some() {
        PamMessage::PamInsertAfterHeaderHint
    } else {
        PamMessage::PamInsertAtTopHint
    }
}

fn with_line_inserted(content: &[u8]) -> Vec<u8> {
    PamDocument::new(content).with_facelock_inserted()
}

fn with_line_removed(content: &[u8]) -> Vec<u8> {
    PamDocument::new(content).with_facelock_removed()
}

const VENDOR_OVERRIDE_HEADER_SUFFIX: &[u8] =
    b"# This local override shadows the vendor file and will not track vendor updates.\n";

#[derive(Debug, Clone, PartialEq, Eq)]
enum VendorOverrideDisposition {
    NotFacelock,
    Unchanged,
    Drifted,
    SourceAbsent(PathBuf),
}

#[derive(Debug)]
struct ResolvedVendor {
    path: PathBuf,
    content: Vec<u8>,
    identity: FileIdentity,
}

/// Resolve the package-owned service exactly as Linux-PAM resolves the search
/// path after the writable override root: the first existing entry wins. A
/// malformed first entry is an error, never permission to keep looking for a
/// later file whose bytes happen to match old provenance.
fn resolve_current_vendor(
    dirs: &PamDirs,
    service: &str,
) -> std::io::Result<Option<ResolvedVendor>> {
    use std::io::ErrorKind;

    confined(service)
        .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "invalid PAM service name"))?;
    for root in dirs.iter().skip(1) {
        let directory = match open_directory_nofollow(root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let file = match open_regular_at(&directory, OsStr::new(service)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let content = read_open_bounded(&file, MAX_BACKUP_BYTES)?;
        let identity = identity_for_bytes(&file.metadata()?, &content);
        return Ok(Some(ResolvedVendor {
            path: root.join(service),
            content,
            identity,
        }));
    }
    Ok(None)
}

/// Normalize one configured candidate path without consulting the
/// filesystem. Header text is compared only with paths derived from the
/// configured later roots; it never becomes an input to `open` or traversal.
fn normalized_configured_path(path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn configured_vendor_header_source(
    dirs: &PamDirs,
    service: &str,
    content: &[u8],
) -> Option<PathBuf> {
    let without_line = with_line_removed(content);
    dirs.iter().skip(1).find_map(|root| {
        let candidate = normalized_configured_path(&root.join(service))?;
        vendor_override_payload(&without_line, &candidate).map(|_| candidate)
    })
}

/// Validate the exact byte shapes Facelock can emit for a vendor override:
/// the original copy with exactly one canonical rule, or the restart shape
/// after that one rule has been removed. Hand-written, duplicate, or custom
/// module rules are drift even when removing them yields the vendor payload.
fn exact_vendor_override_shape(
    content: &[u8],
    identity: &FileIdentity,
    vendor: &ResolvedVendor,
) -> bool {
    let without_line = with_line_removed(content);
    let Some(payload) = vendor_override_payload(&without_line, &vendor.path) else {
        return false;
    };
    let header_len = without_line.len() - payload.len();
    let mut emitted = Vec::with_capacity(header_len + vendor.content.len() + PAM_LINE.len() + 1);
    emitted.extend_from_slice(&without_line[..header_len]);
    emitted.extend_from_slice(&with_line_inserted(&vendor.content));
    (content == without_line || content == emitted)
        && payload == vendor.content
        && !PamDocument::new(&vendor.content).has_facelock_rule()
        && (identity.mode, identity.uid, identity.gid)
            == (
                vendor.identity.mode,
                vendor.identity.uid,
                vendor.identity.gid,
            )
}

fn current_vendor_override_matches(
    dirs: &PamDirs,
    service: &str,
    content: &[u8],
    identity: &FileIdentity,
    require_restart_shape: bool,
) -> std::io::Result<bool> {
    let Some(vendor) = resolve_current_vendor(dirs, service)? else {
        return Ok(false);
    };
    Ok(exact_vendor_override_shape(content, identity, &vendor)
        && (!require_restart_shape || !PamDocument::new(content).has_facelock_rule()))
}

fn vendor_override_payload<'a>(content: &'a [u8], vendor: &Path) -> Option<&'a [u8]> {
    let first_end = content.iter().position(|byte| *byte == b'\n')? + 1;
    let second_end = first_end
        + content[first_end..]
            .iter()
            .position(|byte| *byte == b'\n')?
        + 1;
    if &content[first_end..second_end] != VENDOR_OVERRIDE_HEADER_SUFFIX {
        return None;
    }

    let prefix = format!("# Copied from {} by facelock ", vendor.display());
    let first = &content[..first_end];
    let rest = first.strip_prefix(prefix.as_bytes())?;
    let rest = rest.strip_suffix(b".\n")?;
    let separator = rest
        .windows(b" on ".len())
        .rposition(|part| part == b" on ")?;
    let version = &rest[..separator];
    let date = &rest[separator + b" on ".len()..];
    let version_ok = !version.is_empty()
        && version
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    let date_ok = date.len() == 10
        && date.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7) && *byte == b'-'
                || !matches!(index, 4 | 7) && byte.is_ascii_digit()
        });
    (version_ok && date_ok).then_some(&content[second_end..])
}

fn classify_vendor_override(
    dirs: &PamDirs,
    target: &Target,
    content: &[u8],
    identity: &FileIdentity,
) -> VendorOverrideDisposition {
    match resolve_current_vendor(dirs, &target.service) {
        Ok(Some(vendor)) => {
            if exact_vendor_override_shape(content, identity, &vendor) {
                VendorOverrideDisposition::Unchanged
            } else {
                VendorOverrideDisposition::Drifted
            }
        }
        Ok(None) => configured_vendor_header_source(dirs, &target.service, content)
            .map_or(VendorOverrideDisposition::NotFacelock, |vendor| {
                VendorOverrideDisposition::SourceAbsent(vendor)
            }),
        Err(_) if target.shadowed.is_some() => VendorOverrideDisposition::Drifted,
        Err(_) => VendorOverrideDisposition::NotFacelock,
    }
}

fn read_regular_at_bounded(
    directory: &fs::File,
    name: &str,
    limit: usize,
) -> std::io::Result<Option<(Vec<u8>, FileIdentity)>> {
    match open_regular_at(directory, OsStr::new(name)) {
        Ok(file) => {
            let content = read_open_bounded(&file, limit)?;
            let identity = identity_for_bytes(&file.metadata()?, &content);
            Ok(Some((content, identity)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn restore_vendor_quarantine(
    directory: &fs::File,
    service: &str,
    quarantine: &str,
    expected: &FileIdentity,
) -> std::io::Result<()> {
    match open_identity_at(directory, service, MAX_BACKUP_BYTES) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return Err(ambiguous_publication(
                "vendor override could not be restored without overwriting a concurrent file",
            ));
        }
        Err(_) => {
            return Err(ambiguous_publication(
                "vendor override canonical name became ambiguous during restore",
            ));
        }
    }
    rename_noreplace_at(directory, quarantine, service).map_err(|_| {
        ambiguous_publication("vendor override quarantine could not be restored durably")
    })?;
    let restored = open_identity_at(directory, service, MAX_BACKUP_BYTES)
        .map_err(|_| ambiguous_publication("restored vendor override could not be verified"))?
        .ok_or_else(|| ambiguous_publication("restored vendor override disappeared"))?;
    if !identity_matches(expected, &restored) {
        return Err(ambiguous_publication(
            "restored vendor override identity became ambiguous",
        ));
    }
    Ok(())
}

fn validate_vendor_quarantine(
    dirs: &PamDirs,
    directory: &fs::File,
    service: &str,
    quarantine: &str,
    expected: &FileIdentity,
) -> std::io::Result<bool> {
    let Some((content, quarantined)) =
        read_regular_at_bounded(directory, quarantine, MAX_BACKUP_BYTES)?
    else {
        return Ok(false);
    };
    if !identity_matches(expected, &quarantined) {
        return Ok(false);
    }
    if open_identity_at(directory, service, MAX_BACKUP_BYTES)?.is_some() {
        return Ok(false);
    }
    current_vendor_override_matches(dirs, service, &content, &quarantined, true)
}

fn decline_vendor_retirement(
    directory: &fs::File,
    service: &str,
    quarantine: &str,
    expected: &FileIdentity,
    reason: &'static str,
    after_boundary: &mut impl FnMut(VendorRetirePoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let quarantined = open_identity_at(directory, quarantine, MAX_BACKUP_BYTES).map_err(|_| {
        ambiguous_publication("vendor override quarantine could not be authenticated for restore")
    })?;
    let canonical = open_identity_at(directory, service, MAX_BACKUP_BYTES).map_err(|_| {
        ambiguous_publication("vendor override canonical name became ambiguous before restore")
    })?;
    if canonical.is_none()
        && quarantined
            .as_ref()
            .is_some_and(|identity| identity_matches(expected, identity))
    {
        restore_vendor_quarantine(directory, service, quarantine, expected)?;
        after_boundary(VendorRetirePoint::Restored)?;
        return Err(std::io::Error::other(reason));
    }
    Err(ambiguous_publication(
        "vendor override quarantine state became ambiguous",
    ))
}

/// Retire an unchanged local vendor copy without ever unlinking its canonical
/// PAM service name. The exact inode is first moved to a transaction-derived
/// quarantine name with no-clobber semantics and the directory is synced.
/// Only that authenticated quarantine is eligible for checked deletion.
fn retire_vendor_override_with_hook(
    dirs: &PamDirs,
    service: &str,
    operation: &str,
    expected: &FileIdentity,
    mut after_boundary: impl FnMut(VendorRetirePoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    confined(service).map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid service"))?;
    if !valid_backup_name(service, operation) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid vendor-retirement operation",
        ));
    }
    let directory = open_directory_nofollow(dirs.overrides()).map_err(|_| {
        ambiguous_publication("vendor override root could not be reopened for quarantine")
    })?;
    let quarantine = vendor_retire_name(operation);
    let existing_quarantine = read_regular_at_bounded(&directory, &quarantine, MAX_BACKUP_BYTES)
        .map_err(|_| ambiguous_publication("vendor override quarantine could not be inspected"))?;
    if let Some((_, quarantined)) = existing_quarantine.as_ref() {
        if !identity_matches(expected, quarantined) {
            return Err(ambiguous_publication(
                "vendor override quarantine is not the published removal inode",
            ));
        }
    } else {
        let current = open_identity_at(&directory, service, MAX_BACKUP_BYTES).map_err(|_| {
            ambiguous_publication("vendor override canonical name could not be inspected")
        })?;
        match current {
            None => return Ok(()),
            Some(current) if identity_matches(expected, &current) => {}
            Some(_) => {
                return Err(ambiguous_publication(
                    "vendor override changed before quarantine",
                ));
            }
        }
        rename_noreplace_at(&directory, service, &quarantine).map_err(|error| {
            if error.raw_os_error() == Some(libc::EEXIST) {
                ambiguous_publication("vendor override quarantine name already exists")
            } else {
                ambiguous_publication("vendor override could not be quarantined durably")
            }
        })?;
        after_boundary(VendorRetirePoint::Quarantined)?;
    }

    let initially_valid =
        validate_vendor_quarantine(dirs, &directory, service, &quarantine, expected);
    if !matches!(initially_valid, Ok(true)) {
        return decline_vendor_retirement(
            &directory,
            service,
            &quarantine,
            expected,
            "vendor override or current vendor source changed before cleanup",
            &mut after_boundary,
        );
    }

    after_boundary(VendorRetirePoint::BeforeFinalValidation)?;
    if !matches!(
        validate_vendor_quarantine(dirs, &directory, service, &quarantine, expected,),
        Ok(true)
    ) {
        return decline_vendor_retirement(
            &directory,
            service,
            &quarantine,
            expected,
            "vendor override or current vendor source changed before final cleanup",
            &mut after_boundary,
        );
    }
    unlink_at_if_identity_matches(&directory, &quarantine, expected, MAX_BACKUP_BYTES).map_err(
        |_| ambiguous_publication("vendor override quarantine could not be authenticated"),
    )?;
    directory.sync_all().map_err(|_| {
        ambiguous_publication("vendor override quarantine deletion was not durable")
    })?;
    after_boundary(VendorRetirePoint::Unlinked)
}

/// The two comment lines a vendor copy carries, so the next reader knows the
/// file is a local override and what it was forked from.
///
/// Above everything, including the facelock line: it is provenance for the
/// whole file, not for the line. `is_facelock_pam_line` skips comments, so it
/// never affects what `remove` takes out or what `status` sees.
fn provenance_header(vendor: &Path) -> String {
    format!(
        "# Copied from {} by facelock {} on {}.\n\
         # This local override shadows the vendor file and will not track vendor updates.\n",
        vendor.display(),
        env!("CARGO_PKG_VERSION"),
        chrono::Local::now().format("%Y-%m-%d"),
    )
}

// ---------------------------------------------------------------------------
// The atomic replace
// ---------------------------------------------------------------------------

/// Write `content` to `path` as a temp file and a rename, carrying `model`'s
/// mode and owner — and `path`'s own SELinux context — onto the new file.
///
/// This test helper models the original atomic write contract for an in-place
/// edit or vendor copy: a `/etc/pam.d/polkit-1` truncated by a kill between the
/// truncate and the last byte breaks polkit auth machine-wide, and a
/// half-written `system-auth` breaks the machine. A rename is atomic, so a
/// reader sees either the old file or the new one and never a short one.
///
/// `model` is the file whose ownership the result must have: the target itself
/// for an in-place edit — where this preserves what was already there — and
/// the vendor file for a copy, where it is the only provenance available. The
/// temp file is created in the destination's own directory, because a rename
/// across filesystems is not a rename at all, and it is removed on any failure
/// so a refused write leaves no debris in `/etc/pam.d`.
///
/// The SELinux context is taken from the file being **replaced**, not from
/// `model`: an in-place edit must keep the label it had, and a copy into
/// `/etc/pam.d` must *not* inherit the vendor file's `/usr` label. When there
/// is no file to take one from — the copy case — the temp file already has the
/// destination directory's type by SELinux's own type-transition rules, which
/// is the label the new file should have. So the copy is best-effort by
/// construction: failing it degrades to the label the file would have had
/// anyway.
#[cfg(test)]
fn replace_atomically(path: &Path, content: &[u8], model: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let dir = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "no file name"))?;
    let model = fs::metadata(model)?;

    // A dotted name so that even a reader enumerating the directory mid-write
    // sees something obviously not a service file, and unique per process and
    // instant so two concurrent runs cannot collide on it. `create_new` is the
    // check that they did not.
    let temp = dir.join(format!(
        ".{}.facelock-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default(),
    ));

    // Every piece of metadata is applied to the **open descriptor**, before
    // the `fsync` and before the rename. By path it would be a window — a
    // `chmod` on a name, which is a different question from a `chmod` on the
    // file this just wrote — and it would leave the mode, owner and label
    // outside the `fsync` that is supposed to make the new file durable.
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(model.mode())
            .open(&temp)?;
        file.write_all(content)?;

        // `OpenOptions::mode` is masked by the umask, so the mode is set again
        // rather than trusted: a service file that came out 0600 because the
        // caller's umask was 0177 is unreadable to every PAM stack that uses
        // it.
        file.set_permissions(fs::Permissions::from_mode(model.mode()))?;
        let current = file.metadata()?;
        if (model.uid(), model.gid()) != (current.uid(), current.gid()) {
            fchown(&file, model.uid(), model.gid())?;
        }
        copy_selinux_context(path, &file);

        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();

    if written.is_err() {
        // Best effort: the write already failed, and the report names that
        // failure rather than whatever went wrong tidying up after it.
        let _ = fs::remove_file(&temp);
        return written;
    }

    // The rename is what makes the content visible; this is what makes it
    // survive a power cut, and it is best-effort for the same reason `fsync`
    // on a directory is optional on every filesystem that matters.
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    Ok(())
}

/// `fchown(2)`, which `std::fs` does not expose.
///
/// On the descriptor rather than on the path: `chown(2)` follows symlinks and
/// resolves the name again, so it answers a question about whatever the name
/// points at *now* instead of about the file this just wrote.
fn fchown(file: &fs::File, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    // SAFETY: the descriptor is owned by `file` and stays open for the call.
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn apply_owner_then_mode(file: &fs::File, uid: u32, gid: u32, mode: u32) -> std::io::Result<()> {
    apply_owner_then_mode_with_hook(file, uid, gid, mode, fchown)
}

fn apply_owner_then_mode_with_hook(
    file: &fs::File,
    uid: u32,
    gid: u32,
    mode: u32,
    owner_hook: impl FnOnce(&fs::File, u32, u32) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let current = file.metadata()?;
    if (uid, gid) != (current.uid(), current.gid()) {
        owner_hook(file, uid, gid)?;
    }
    // fchown clears setuid/setgid on Linux, so the requested mode must be the
    // final metadata operation before fsync/publication.
    file.set_permissions(fs::Permissions::from_mode(mode))
}

/// Carry the label of the file at `from` onto the open `to`, if there is one
/// to carry.
///
/// The destination is the descriptor, like the mode and the owner: the label
/// belongs to the file this wrote, not to a name that could be something else
/// by the time the call lands. The source is read by path with `lgetxattr`,
/// which is a read of a file that already exists and is not required to be the
/// one being replaced — it simply is, in every case this has.
///
/// Silent when the source has no label (no SELinux, or a filesystem without
/// xattrs), because that is the overwhelmingly common case and it is not a
/// failure. Noisy only when a label existed and could not be set, which is the
/// one combination that leaves the new file labelled differently from the old.
#[cfg(test)]
fn copy_selinux_context(from: &Path, to: &fs::File) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    const SELINUX_XATTR: &std::ffi::CStr = c"security.selinux";
    // Longer than any policy's context; a label that does not fit is left to
    // the type transition rather than truncated, which would be worse.
    let mut context = [0u8; 256];

    let Ok(c_from) = CString::new(from.as_os_str().as_bytes()) else {
        return;
    };

    // SAFETY: the C string outlives the call, and the buffer's length is
    // passed as its capacity, so the call cannot write past it.
    let read = unsafe {
        libc::lgetxattr(
            c_from.as_ptr(),
            SELINUX_XATTR.as_ptr(),
            context.as_mut_ptr().cast(),
            context.len(),
        )
    };
    if read <= 0 {
        return;
    }

    // SAFETY: as above; `read` bytes were just written into `context`, and the
    // descriptor is owned by `to` and stays open for the call.
    let set = unsafe {
        libc::fsetxattr(
            to.as_raw_fd(),
            SELINUX_XATTR.as_ptr(),
            context.as_ptr().cast(),
            read as usize,
            0,
        )
    };
    if set != 0 {
        tracing::warn!(
            source = %from.display(),
            error = %std::io::Error::last_os_error(),
            "could not carry the SELinux context onto the new PAM service file"
        );
    }
}

// ---------------------------------------------------------------------------
// Output routing
// ---------------------------------------------------------------------------

/// Which seam sink one line of this command's human text goes to.
///
/// Named rather than decided inline because two of this module's lines must
/// survive `--quiet`, and "which stream, suppressible or not" is then a rule
/// worth pinning with a test instead of re-reading `apply_add` for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `Terminal::info` — stdout, silenced by `--quiet`. Ordinary progress.
    Info,
    /// `Terminal::notice` — stdout, which `--quiet` does not reach. For the
    /// lines an operator needs *and* that have to stay on stdout to keep the
    /// bytes a normal run prints byte-identical.
    Notice,
    /// `Terminal::error` — stderr, never silenced.
    Error,
    /// Nothing is printed: `--json` replaces the human rendering, and the
    /// document carries the same fact in a field.
    Dropped,
}

/// Where this invocation's human text goes.
///
/// `--json` replaces the human rendering of the payload, so [`Sink::info`] is
/// silenced under it — but [`Sink::error`] is not. stdout is the answer and
/// stderr is everything else (contracts.md, "CLI Output Streams"), so a
/// diagnostic belongs on stderr whether or not a JSON document is being
/// written to stdout. `--quiet` is handled one layer down, by the message
/// seam, which silences `Terminal::info`, and never `Terminal::error` or
/// `Terminal::notice`.
#[derive(Clone, Copy)]
struct Sink {
    json: bool,
    /// Whether a `failed` row gets its human rendering here.
    ///
    /// The verb's rows *are* its output, so each failure is written to stderr
    /// as it happens, interleaved with the services around it. The three
    /// `setup --pam` aliases hand the failure back as an `Err` and their
    /// caller renders it — `setup`'s wizard as `PamConfigureFailed`, the
    /// standalone path through `main` — so rendering it here as well would
    /// print it twice.
    report_failures: bool,
    /// Print nothing at all, on either stream.
    ///
    /// Not the same as `--json`, which silences the human rendering because a
    /// document replaces it and still writes diagnostics to stderr. This is
    /// for a caller that is not producing output at this moment:
    /// [`configured_scan`], whose rows become one line of a report a different
    /// module renders. Without it, `facelock status` would interleave the
    /// writer's per-service lines into its own report.
    silent: bool,
}

impl Sink {
    /// The verb's sink. `--json` decides whether the human half is rendered
    /// at all; the report is this invocation's output either way.
    fn verb(json: bool) -> Self {
        Sink {
            json,
            report_failures: true,
            silent: false,
        }
    }

    /// The sink of a caller that has no `--json` to offer and returns its
    /// failures instead of printing them: the three `setup --pam` aliases.
    fn human() -> Self {
        Sink {
            json: false,
            report_failures: false,
            silent: false,
        }
    }

    /// The sink of a caller that is gathering facts rather than reporting
    /// them: [`configured_scan`]. See [`Sink::silent`].
    fn silent() -> Self {
        Sink {
            json: false,
            report_failures: false,
            silent: true,
        }
    }

    fn info(&self, msg: &dyn Message) {
        self.emit(self.info_route(), msg);
    }

    /// Ordinary progress: stdout, and `--quiet` may have it. `--json` replaces
    /// the human rendering wholesale, so under it there is nothing to print.
    fn info_route(&self) -> Route {
        if self.json {
            Route::Dropped
        } else {
            Route::Info
        }
    }

    fn error(&self, msg: &dyn Message) {
        self.emit(Route::Error, msg);
    }

    /// The context a per-file confirmation needs to be answerable.
    ///
    /// It is `info` when nothing is being asked — nobody is waiting on it, so
    /// `--quiet` may have it. When the question *will* be asked it has to be
    /// seen, so it goes to the sink that `--quiet` cannot reach, on the stream
    /// the prompt is on: see [`preview_route`].
    fn preview(&self, msg: &dyn Message, prompting: bool) {
        self.emit(preview_route(self.json, prompting), msg);
    }

    /// A line that must be seen and must stay on stdout — the rollback
    /// instructions. Dropped under `--json`, where the document's `backup`
    /// field is the same fact in machine form.
    fn notice(&self, msg: &dyn Message) {
        self.emit(self.notice_route(), msg);
    }

    /// Split out from [`Sink::notice`] so the rule is assertable without
    /// capturing stdout, the way [`preview_route`] is.
    fn notice_route(&self) -> Route {
        if self.json {
            Route::Dropped
        } else {
            Route::Notice
        }
    }

    fn emit(&self, route: Route, msg: &dyn Message) {
        if self.silent {
            return;
        }
        match route {
            Route::Info => Terminal.info(msg),
            Route::Notice => Terminal.notice(msg),
            Route::Error => Terminal.error(msg),
            Route::Dropped => {}
        }
    }
}

/// Where the pre-confirmation preview goes, as a pure function of the two
/// facts that decide it.
///
/// The preview says which file is about to change, what line goes in and where
/// the backup lands; a "Proceed?" with that suppressed is a question with no
/// subject. So when the prompt will be asked it is never `Info`:
///
/// - human mode → `Notice`, which prints the same bytes on the same stream a
///   normal run always printed, and keeps printing them under `--quiet`;
/// - `--json` → `Error`, because stdout is holding a document a parser is
///   reading and the prompt itself is on stderr, so that is where its context
///   belongs. Unreachable from the CLI, where `--json` implies `--no-confirm`;
///   the engine takes plain data, so a library caller can still get here.
fn preview_route(json: bool, prompting: bool) -> Route {
    match (json, prompting) {
        (false, false) => Route::Info,
        (false, true) => Route::Notice,
        (true, false) => Route::Dropped,
        (true, true) => Route::Error,
    }
}

/// Whether the per-file confirmation can be asked at all.
///
/// **Both streams, not just stdin.** dialoguer's `Confirm` reads *and* draws
/// through `Term::stderr()`, so `sudo facelock pam add --service sudo
/// 2>install.log` — stdin a TTY, stderr a file — made `interact()` fail with
/// "not a terminal", which the writer reported as `failed` having written
/// nothing. Redirecting a log is not a reason to refuse to install.
///
/// Proceeding is the answer a closed stdin has always got, and it is what
/// makes `setup --pam` work from a provisioning script: the prompt defaults to
/// yes, so skipping it changes whether you are asked, not the outcome. The
/// sensitive-service gate is decided in the validation phase, before any
/// prompt exists, so nothing here can unlock it.
fn should_prompt(no_confirm: bool, stdin_is_tty: bool, stderr_is_tty: bool) -> bool {
    !no_confirm && stdin_is_tty && stderr_is_tty
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

/// Phase two for `add`: preview, confirm, back up, write — one service.
///
/// Message order is the old `pam_install_in`'s, byte for byte: the
/// already-present notice, or the preview, the confirmation, the backup line
/// and the installed line with its rollback instructions.
fn apply_add(target: &Target, no_confirm: bool, sink: &Sink, dirs: &PamDirs) -> Outcome {
    let path = target.reported_path();

    // A vendor copy is `Plan::Override`: a different destination, no backup —
    // there is no `/etc` file to preserve — and a notice of its own, since
    // creating a shadowing file is a durable change with a maintenance
    // consequence the operator has to be told about.
    let (content, from_vendor) = match &target.plan {
        Plan::NoChange => {
            sink.info(&PamMessage::PamLineAlreadyPresent { path });
            return Outcome::Unchanged;
        }
        Plan::Absent => {
            sink.info(&PamMessage::PamServiceAbsentSkipped { path });
            return Outcome::Absent;
        }
        // `plan_writes` builds `VendorOnly` for `remove` alone, so `add`
        // cannot reach it. Answered as the no-op it names rather than with an
        // invented write; the assertion is where a debug build says the
        // invariant broke.
        Plan::VendorOnly => {
            debug_assert!(false, "a vendor-only plan reached `add`");
            sink.info(&PamMessage::PamVendorOnly { path });
            return Outcome::VendorOnly;
        }
        Plan::Rewrite { content } => (content.as_slice(), false),
        Plan::Override { content } => (content.as_slice(), true),
        Plan::DeleteOverride { .. } => {
            debug_assert!(false, "a vendor-delete plan reached `add`");
            sink.info(&PamMessage::PamNoLineFound { path });
            return Outcome::Unchanged;
        }
        Plan::RetainVendorOverride { .. } => {
            debug_assert!(false, "a vendor-retain plan reached `add`");
            sink.info(&PamMessage::PamNoLineFound { path });
            return Outcome::Unchanged;
        }
    };
    // Compute the exact installed bytes before allocating provenance: its
    // hash is part of the prepared record and recovery must be able to decide
    // which side of the rename is present after a crash.
    let with_line = with_line_inserted(content);
    let written = if from_vendor {
        let vendor =
            normalized_configured_path(&target.path).unwrap_or_else(|| target.path.clone());
        let header = provenance_header(&vendor);
        let mut written = Vec::with_capacity(header.len() + with_line.len());
        written.extend_from_slice(header.as_bytes());
        written.extend_from_slice(&with_line);
        written
    } else {
        with_line
    };
    let store = match BackupStore::open(dirs.backup_dir()) {
        Ok(store) => store,
        Err(error) => {
            return Outcome::Failed(format!("failed to open PAM backup state: {error}"));
        }
    };
    let transaction = match store.transaction(dirs) {
        Ok(transaction) => transaction,
        Err(error) => {
            return Outcome::Failed(format!("failed to recover PAM backup state: {error}"));
        }
    };
    let prepared = if from_vendor {
        None
    } else {
        match transaction.plan(&target.service, content, &written) {
            Ok(prepared) => Some(prepared),
            Err(error) => {
                return Outcome::Failed(format!("failed to plan backup for {path}: {error}"));
            }
        }
    };
    let vendor_mutation = if from_vendor {
        match transaction.plan_mutation(&target.service, content, &written) {
            Ok(mutation) => Some(mutation),
            Err(error) => {
                return Outcome::Failed(format!(
                    "failed to plan vendor override for {path}: {error}"
                ));
            }
        }
    } else {
        None
    };
    let backup = prepared
        .as_ref()
        .map(|prepared| prepared.backup_path().display().to_string())
        .unwrap_or_default();

    // Decided before the preview is printed, because it decides where the
    // preview goes: a question that will be asked needs its context seen.
    let prompting = should_prompt(
        no_confirm,
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
    );

    // Two previews, because they promise different things: the in-place edit
    // names the backup it is about to take, and the copy has none to name —
    // its undo is deleting the file it is about to create.
    if from_vendor {
        sink.preview(
            &PamMessage::PamOverridePreview {
                path: path.clone(),
                vendor: target.path_string(),
                line: PAM_LINE.to_string(),
                hint: insertion_hint(content).localized(),
            },
            prompting,
        );
    } else {
        sink.preview(
            &PamMessage::PamModifyPreview {
                path: path.clone(),
                line: PAM_LINE.to_string(),
                hint: insertion_hint(content).localized(),
                backup: backup.clone(),
            },
            prompting,
        );
    }

    let proceed = if !prompting {
        true
    } else {
        match Confirm::new()
            .with_prompt(PamMessage::ConfirmProceed.localized())
            .default(true)
            .interact()
        {
            Ok(answer) => answer,
            Err(error) => return Outcome::Failed(format!("failed to read confirmation: {error}")),
        }
    };

    if !proceed {
        sink.info(&PamMessage::PamSkippedFile { path });
        return Outcome::Declined;
    }

    // No backup for a copy: there is no `/etc` file to preserve, and copying
    // the *vendor* file to `<service>.facelock-backup` would name a backup of
    // a file nothing changed. Deleting the override is the undo, and the
    // notice below says so.
    //
    // The backup and prepared provenance are written atomically inside the
    // root-only state directory. This avoids `fs::copy` following an adjacent
    // symlink or leaving a short rollback file after a crash. `content` is the
    // bytes phase one read, so the backup is the file this is about to replace,
    // not whatever the path holds by the time the state write runs.
    if let Some(prepared) = &prepared {
        if let Err(error) = transaction.persist(prepared, content) {
            return Outcome::Failed(format!("failed to back up {path} to {backup}: {error}"));
        }
        sink.info(&PamMessage::PamBackedUp {
            path: path.clone(),
            backup: backup.clone(),
        });
    }

    let replacement = if let Some(prepared) = &prepared {
        match target.identity.as_ref() {
            Some(expected) => {
                transaction.replace_pam_with_intent(prepared, &target.path, expected, &written)
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "PAM service has no planned file identity",
            )),
        }
    } else if let (Some(mutation), Some(expected)) =
        (vendor_mutation.as_ref(), target.identity.as_ref())
    {
        transaction.create_vendor_with_intent(mutation, target, expected, &written)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "vendor PAM service has no planned file identity",
        ))
    };
    if let Err(error) = replacement {
        return Outcome::Failed(format!("failed to write {path}: {error}"));
    }

    if let Some(prepared) = &prepared
        && let Err(error) = transaction.commit(prepared)
    {
        return Outcome::Failed(format!(
            "installed {path}, but failed to commit backup provenance: {error}"
        ));
    }

    // `notice`, not `info`: these are the messages that tell an operator who
    // has just changed an auth stack how to undo it, so `--quiet` must not
    // take them. Still stdout, so a normal run prints the same bytes it always
    // did. Under `--json` the row's `backup` field carries the same fact.
    if from_vendor {
        sink.notice(&PamMessage::PamVendorOverridden {
            path,
            vendor: target.path_string(),
            service: target.service.clone(),
        });
        return Outcome::Overridden;
    }
    sink.notice(&PamMessage::PamInstalled {
        path,
        backup,
        service: target.service.clone(),
    });
    Outcome::Installed
}

/// Phase two for `remove` — one service.
///
/// No confirmation and no new backup, which is what the old `pam_remove_in`
/// did: removal only takes away a way to authenticate, so there is nothing to
/// be talked out of, and `setup --pam --remove` has never prompted. An
/// existing backup is reported so the operator knows a full restore is
/// available.
fn apply_remove(target: &Target, sink: &Sink, dirs: &PamDirs) -> Outcome {
    apply_remove_with_vendor_hook(target, sink, dirs, |_| Ok(()))
}

fn remove_success_message(
    target: &Target,
    path: String,
    disposition: VendorOverrideDisposition,
) -> PamMessage {
    match disposition {
        VendorOverrideDisposition::Drifted => PamMessage::PamVendorOverrideRetained {
            path,
            vendor: target
                .shadowed
                .as_ref()
                .map(|vendor| vendor.display().to_string())
                .unwrap_or_default(),
        },
        VendorOverrideDisposition::SourceAbsent(vendor) => {
            PamMessage::PamVendorOverrideSourceAbsent {
                path,
                vendor: vendor.display().to_string(),
            }
        }
        VendorOverrideDisposition::NotFacelock | VendorOverrideDisposition::Unchanged => {
            PamMessage::PamRemoved { path }
        }
    }
}

fn apply_remove_with_vendor_hook(
    target: &Target,
    sink: &Sink,
    dirs: &PamDirs,
    mut vendor_hook: impl FnMut(VendorRetirePoint) -> std::io::Result<()>,
) -> Outcome {
    let path = target.path_string();

    match &target.plan {
        Plan::Absent => {
            sink.info(&PamMessage::PamServiceAbsent { path });
            Outcome::Absent
        }
        // The service exists only as a package-owned file. `remove` never
        // writes to a vendor directory, so there is nothing to take out —
        // reported as itself rather than as `unchanged`, which would claim
        // this had looked at a file in `/etc/pam.d`.
        Plan::VendorOnly => {
            sink.info(&PamMessage::PamVendorOnly { path });
            Outcome::VendorOnly
        }
        Plan::NoChange => {
            sink.info(&PamMessage::PamNoLineFound { path: path.clone() });
            Outcome::Unchanged
        }
        Plan::RetainVendorOverride { vendor } => {
            sink.info(&PamMessage::PamVendorOverrideSourceAbsentNoLine {
                path,
                vendor: vendor.display().to_string(),
            });
            Outcome::Unchanged
        }
        // `remove` still takes no backup of its own — it relies on the one
        // `add` wrote, which is the remaining entry under "Limits" in
        // docs/contracts.md. The write itself is atomic, like every other.
        Plan::Rewrite { content } => {
            let vendor_disposition = target
                .identity
                .as_ref()
                .map_or(VendorOverrideDisposition::NotFacelock, |identity| {
                    classify_vendor_override(dirs, target, content, identity)
                });
            let installed = with_line_removed(content);
            let store = match BackupStore::open(dirs.backup_dir()) {
                Ok(store) => store,
                Err(error) => {
                    return Outcome::Failed(format!("failed to open PAM backup state: {error}"));
                }
            };
            let transaction = match store.transaction(dirs) {
                Ok(transaction) => transaction,
                Err(error) => {
                    return Outcome::Failed(format!("failed to recover PAM backup state: {error}"));
                }
            };
            let mutation = match transaction.plan_mutation(&target.service, content, &installed) {
                Ok(mutation) => mutation,
                Err(error) => {
                    return Outcome::Failed(format!("failed to plan removal for {path}: {error}"));
                }
            };
            let replacement = target.identity.as_ref().map_or_else(
                || {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "PAM service has no planned file identity",
                    ))
                },
                |expected| {
                    transaction.remove_pam_with_intent(
                        &mutation,
                        &target.path,
                        expected,
                        &installed,
                    )
                },
            );
            match replacement {
                Ok(()) => {
                    sink.info(&remove_success_message(
                        target,
                        path.clone(),
                        vendor_disposition,
                    ));
                    Outcome::Removed
                }
                Err(error) => Outcome::Failed(format!("failed to write {path}: {error}")),
            }
        }
        Plan::DeleteOverride { content } => {
            let installed = with_line_removed(content);
            let expected = match target.identity.as_ref() {
                Some(expected) => expected,
                None => {
                    return Outcome::Failed(format!(
                        "failed to remove {path}: PAM service has no planned file identity"
                    ));
                }
            };
            let store = match BackupStore::open(dirs.backup_dir()) {
                Ok(store) => store,
                Err(error) => {
                    return Outcome::Failed(format!("failed to open PAM backup state: {error}"));
                }
            };
            let transaction = match store.transaction(dirs) {
                Ok(transaction) => transaction,
                Err(error) => {
                    return Outcome::Failed(format!("failed to recover PAM backup state: {error}"));
                }
            };
            let mutation = match transaction.plan_mutation(&target.service, content, &installed) {
                Ok(mutation) => mutation,
                Err(error) => {
                    return Outcome::Failed(format!("failed to plan removal for {path}: {error}"));
                }
            };
            let operation = mutation.operation.clone();
            if let Err(error) = transaction.remove_pam_with_intent_and_published_hook(
                &mutation,
                &target.path,
                expected,
                &installed,
                |installed_identity| {
                    retire_vendor_override_with_hook(
                        dirs,
                        &target.service,
                        &operation,
                        installed_identity,
                        &mut vendor_hook,
                    )
                },
            ) {
                return Outcome::Failed(format!(
                    "failed to remove unchanged vendor override {path}: {error}"
                ));
            }
            sink.info(&PamMessage::PamVendorOverrideRemoved {
                path: path.clone(),
                vendor: target
                    .shadowed
                    .as_ref()
                    .map(|vendor| vendor.display().to_string())
                    .unwrap_or_default(),
            });
            Outcome::Removed
        }
        // `plan_writes` builds `Override` for `add` alone. Answered like
        // `VendorOnly` rather than with a failure of its own: if that ever
        // stopped being true, the safe answer is still "write nothing to a
        // vendor directory", and the assertion is where a debug build says the
        // invariant broke.
        Plan::Override { .. } => {
            debug_assert!(false, "an override plan reached `remove`");
            sink.info(&PamMessage::PamVendorOnly { path });
            Outcome::VendorOnly
        }
    }
}

/// Render the resolved plan for `--dry-run`, writing nothing.
fn report_plan(target: &Target, action: WriteAction, sink: &Sink) -> Outcome {
    let path = target.reported_path();
    match (&target.plan, action) {
        (Plan::Rewrite { content }, WriteAction::Add) => {
            sink.info(&PamMessage::PamPlanAdd {
                path,
                hint: insertion_hint(content).localized(),
            });
            Outcome::Installed
        }
        (Plan::Rewrite { .. }, WriteAction::Remove) => {
            sink.info(&PamMessage::PamPlanRemove { path });
            Outcome::Removed
        }
        (Plan::DeleteOverride { .. }, WriteAction::Remove) => {
            sink.info(&PamMessage::PamPlanDeleteOverride {
                path,
                vendor: target
                    .shadowed
                    .as_ref()
                    .map(|vendor| vendor.display().to_string())
                    .unwrap_or_default(),
            });
            Outcome::Removed
        }
        (Plan::DeleteOverride { .. }, WriteAction::Add) => {
            debug_assert!(false, "a vendor-delete plan reached an add preview");
            Outcome::Unchanged
        }
        (Plan::RetainVendorOverride { vendor }, WriteAction::Remove) => {
            sink.info(&PamMessage::PamVendorOverrideSourceAbsentNoLine {
                path,
                vendor: vendor.display().to_string(),
            });
            Outcome::Unchanged
        }
        (Plan::RetainVendorOverride { .. }, WriteAction::Add) => {
            debug_assert!(false, "a vendor-retain plan reached an add preview");
            Outcome::Unchanged
        }
        (Plan::Override { content }, _) => {
            sink.info(&PamMessage::PamPlanOverride {
                path,
                vendor: target.path_string(),
                hint: insertion_hint(content).localized(),
            });
            Outcome::Overridden
        }
        (Plan::VendorOnly, _) => {
            sink.info(&PamMessage::PamVendorOnly { path });
            Outcome::VendorOnly
        }
        (Plan::NoChange, _) => {
            sink.info(&PamMessage::PamPlanNoChange { path });
            Outcome::Unchanged
        }
        (Plan::Absent, _) => {
            sink.info(&PamMessage::PamPlanAbsent { path });
            Outcome::Absent
        }
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// `{"command", "dry_run", "services": [{"service", "path", "action",
/// "backup"}]}`, with `"error"` present on a `failed`, `cleanup-failed` or
/// `unknown` service.
///
/// An object rather than a bare array so a later top-level field is an
/// additive change instead of a document-type change. Built through
/// `serde_json::json!` rather than `format!` (E10, the same reason as
/// `commands::list::list_json`): a service name reaches here from argv, and
/// confinement rejects `/` but not `"`, so `--service 'a"b'` would otherwise
/// emit invalid JSON.
fn report_json(
    action: PamAction,
    dry_run: bool,
    reports: &[ServiceReport],
    directories: &[DirectoryScan],
) -> String {
    let services: Vec<serde_json::Value> = reports
        .iter()
        .map(|report| {
            let mut value = serde_json::json!({
                "service": report.service,
                "path": report.path,
                "action": report.outcome.word(),
                "backup": report.backup,
            });
            if let (Some(error), Some(object)) = (report.outcome.error(), value.as_object_mut()) {
                object.insert("error".to_string(), serde_json::Value::String(error.into()));
            }
            // Omitted rather than `null` when there is nothing shadowed: every
            // row on a machine with no vendor directory would otherwise carry
            // a key that is always null, and the absent-key form is the one
            // `error` already established for a field that is sometimes there.
            if let (Some(vendor), Some(object)) = (&report.shadows, value.as_object_mut()) {
                object.insert(
                    "shadows".to_string(),
                    serde_json::Value::String(vendor.clone()),
                );
            }
            value
        })
        .collect();

    let mut document = serde_json::json!({
        "command": action.word(),
        "dry_run": dry_run,
        "services": services,
    });

    // `status` alone carries `module_path`: it is the verb that answers "is
    // this machine wired up", and "the line is present but the module it names
    // is at a path nothing looks at" is the state an integrator could not
    // otherwise see. `add` and `remove` refuse before writing when the module
    // is absent, so the question is already answered for them. A property of
    // the machine, not of a service, so it is top-level rather than repeated
    // in every service object — `null` when no candidate hit.
    if let (PamAction::Status, Some(object)) = (action, document.as_object_mut()) {
        let found = installed_module_path().map(|path| path.display().to_string());
        object.insert("module_path".to_string(), serde_json::json!(found));
    }

    // `--all` alone carries `directories`: it is the only form that claims to
    // have looked everywhere, so it is the only one that owes the reader an
    // account of where it looked and which of those places answered. A named
    // request resolves through the search path without listing it, and an
    // empty array there would read as "nothing searched".
    if let (false, Some(object)) = (directories.is_empty(), document.as_object_mut()) {
        let scanned: Vec<serde_json::Value> = directories
            .iter()
            .map(|dir| {
                let mut value = serde_json::json!({
                    "path": dir.path.display().to_string(),
                    "status": dir.state.word(),
                });
                if let (Some(error), Some(object)) = (dir.state.error(), value.as_object_mut()) {
                    object.insert("error".to_string(), serde_json::Value::String(error.into()));
                }
                value
            })
            .collect();
        object.insert("directories".to_string(), serde_json::Value::Array(scanned));
    }

    document.to_string()
}

/// Emit the machine document on stdout, if this invocation asked for one.
///
/// The `--json` test lives here rather than at each caller, so "was a document
/// requested?" is asked once. Through [`crate::message::payload`], so
/// `--quiet` reaches it without this function knowing the flag exists: under
/// it the document is dropped and the exit code is the whole answer, which is
/// `is-enrolled --quiet --json`'s rule generalized to every payload.
///
/// `dry_run` is a parameter rather than read off the request because `status`
/// has no dry run: it reports `false` whatever a library caller left in the
/// field, since a read that always happens cannot have been a preview.
fn emit_json(
    request: &PamRequest,
    dry_run: bool,
    reports: &[ServiceReport],
    directories: &[DirectoryScan],
) {
    if request.json {
        crate::message::payload(&report_json(request.action, dry_run, reports, directories));
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Run `facelock pam …`, returning the process exit code.
///
/// **C6 ordering.** `add` and `remove` check root as their first statement,
/// before any output, any read of `/etc/pam.d`, and `--dry-run`. A dry run
/// that succeeded unprivileged would be a misleading preview of a command that
/// cannot run, and making the check conditional on a flag is exactly the
/// ordering subtlety C6 exists to prevent. `pam status` needs no root and
/// takes none.
///
/// The refusal is the hard-error kind (`require_root_scripted`), not the
/// interactive sudo re-exec: standalone `setup --pam` has never re-execed —
/// `setup::needs_root_precheck` says so and a test pins it — and silently
/// re-running a `/etc/pam.d` edit under `sudo` from a wrapper script is a
/// surprise, not a convenience.
pub fn run(request: PamRequest) -> anyhow::Result<i32> {
    // `status` needs no root and returns before the check, as it always has.
    if request.action == PamAction::Status {
        return Ok(status_in(&PamDirs::system(), &request));
    }

    // C6: the first statement of the write path, before the config read the
    // search path needs. Resolving the search path first would be harmless
    // today — the read is silent and cannot fail — but "first statement" is
    // the property this rule pins, and a rule that has to be re-argued from
    // the current implementation is not a rule.
    crate::ipc_client::require_root_scripted(&format!(
        "sudo facelock pam {}",
        request.action.word()
    ))?;

    if request.action == PamAction::Add {
        require_module_installed()?;
    }

    if request.action == PamAction::Remove && request.all {
        return remove_all_in(&PamDirs::system_cleanup(), &request);
    }

    write_in(&PamDirs::system(), &request)
}

/// Whether the PAM module is where the line needs it to be.
///
/// A property of the machine, not of a service, so it is asked once per
/// invocation rather than per service — and asked here rather than by each
/// caller spelling [`PAM_MODULE_PATHS`] itself, which is how `setup` came to
/// have its own copy of the test.
pub(crate) fn module_installed() -> bool {
    installed_module_path().is_some()
}

/// The candidate the module was found at, or `None`. The probe is **read
/// only**: it finds the module and never installs, copies or links it —
/// placing it is the packager's job, and writing into the directory `ld.so`
/// loads auth modules from is not something a CLI should do.
pub(crate) fn installed_module_path() -> Option<PathBuf> {
    first_existing(PAM_MODULE_PATHS)
}

/// The first candidate that exists. Split out so the order is testable without
/// a machine that has the module installed in three places.
fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(Path::new)
        .find(|path| path.exists())
        .map(Path::to_path_buf)
}

/// The precondition the line is useless without, as the refusal to write.
///
/// The refusal names **every** candidate: an operator whose distribution puts
/// the module somewhere unlisted can then see what facelock looked for rather
/// than guess which single path it wanted.
fn require_module_installed() -> anyhow::Result<()> {
    if module_installed() {
        return Ok(());
    }
    Err(fail(PamMessage::PamModuleNotInstalled {
        paths: PAM_MODULE_PATHS.join(", "),
        path: PAM_MODULE_PATHS.first().unwrap_or(&"").to_string(),
    }))
}

/// Whether `service` has a file anywhere on the search path.
///
/// The wizard's menu question (`setup.rs`'s `candidates_in`), asked through the
/// resolver rather than with a `base.join(name).exists()` of its own: a
/// candidate that ships only in a vendor directory — `polkit-1` on current
/// Arch — is offerable, and `pam add` will configure it. An entry this refuses
/// to follow is *not* offered: the menu should not propose a service the
/// writer will then decline.
pub(crate) fn service_exists(dirs: &PamDirs, service: &str) -> bool {
    Target::locate(dirs, service).is_ok_and(|target| target.exists())
}

/// Whether `service` under `dirs` carries the facelock line.
///
/// The question `status` answers, for a caller that wants the boolean and has
/// no exit code to produce — `setup`'s hyprlock handoff, which read
/// `/etc/pam.d/hyprlock` itself and so had its own copy of "where does this
/// name resolve to" beside this module's. An unreadable or unresolvable
/// service is `false`: the caller decides whether to *offer* something, and
/// "cannot tell" is not a reason to.
pub(crate) fn is_configured(dirs: &PamDirs, service: &str) -> bool {
    let Ok(target) = Target::locate(dirs, service) else {
        return false;
    };
    read_regular_nofollow(&target.path)
        .is_ok_and(|(content, _)| PamDocument::new(&content).has_facelock_rule())
}

/// Phase two, for every target.
///
/// **Continue and report**, and that is now true of all four entry points
/// rather than of the verb alone. A write-phase failure on service N is one
/// independent file's failure: the rest still have their own backups and their
/// own chance of succeeding, and a half-reported plan is harder to recover
/// from than a fully-reported one. What the entry points differ in is how they
/// *read* the rows — [`write_in`] turns them into an exit code, the three
/// `setup` aliases into a `Result` through [`first_failure`], once every
/// service has been attempted.
fn apply_all(
    dirs: &PamDirs,
    targets: &[Target],
    write: &WriteRequest,
    sink: &Sink,
) -> Vec<ServiceReport> {
    let recovery = if write.request.dry_run {
        Ok(())
    } else {
        BackupStore::open_existing(dirs.backup_dir())
            .and_then(|store| store.map_or(Ok(()), |store| store.recover(dirs)))
    };
    targets
        .iter()
        .map(|target| {
            let mut outcome = if let Err(error) = &recovery {
                Outcome::Failed(format!("failed to recover PAM backup state: {error}"))
            } else if write.request.dry_run {
                report_plan(target, write.action, sink)
            } else {
                match write.action {
                    WriteAction::Add => apply_add(target, write.request.no_confirm, sink, dirs),
                    WriteAction::Remove => apply_remove(target, sink, dirs),
                }
            };

            if write.action == WriteAction::Remove
                && !write.request.dry_run
                && !write.request.keep_backup
                && !matches!(outcome, Outcome::Failed(_))
                && let Err(error) = cleanup_backups(dirs, &target.service)
            {
                outcome = Outcome::CleanupFailed(format!(
                    "failed to clean backups for {}: {error}",
                    target.service
                ));
            }

            if sink.report_failures {
                match &outcome {
                    Outcome::Failed(error) => sink.error(&PamMessage::PamConfigureFailed {
                        service: target.service.clone(),
                        error: error.clone(),
                    }),
                    Outcome::CleanupFailed(error) => {
                        sink.error(&PamMessage::PamBackupCleanupFailed {
                            service: target.service.clone(),
                            error: error.clone(),
                        });
                    }
                    _ => {}
                }
            }

            ServiceReport {
                service: target.service.clone(),
                path: Some(target.reported_path()),
                // An `overridden` row has no backup **by construction**: the
                // copy preserved nothing, so a `.facelock-backup` sitting at
                // the override path is some earlier run's, and offering it as
                // this run's rollback would promise a restore of a file this
                // did not touch. Deleting the override is the undo, and the
                // notice says so.
                backup: match (&outcome, write.request.dry_run) {
                    (_, true) | (Outcome::Overridden, _) => None,
                    _ => reported_backup(dirs, target),
                },
                // The row that *creates* the shadow is the one the resolver
                // could not report it for: at `locate` time there was no
                // `/etc` entry, so `Origin::Vendor` has nothing to hide yet.
                // After the copy, the file it was read from is exactly what
                // it hides — and under `--dry-run` exactly what it would.
                // Leaving this `None` would put the key on every later
                // `status` row and withhold it from the one that made the
                // fact true.
                shadows: match &outcome {
                    Outcome::Overridden => Some(target.path_string()),
                    Outcome::Removed if matches!(target.plan, Plan::DeleteOverride { .. }) => None,
                    _ => target.shadows_string(),
                },
                outcome,
            }
        })
        .collect()
}

/// The closing copy-pasteable hint, once per invocation and after every
/// service — not once per service, as a shell loop over the old
/// one-service-per-process CLI produced. It is human-facing text, so `--json`
/// and `--quiet` both drop it; `--dry-run` keeps it, so a dry run is a
/// faithful preview of what the real run prints.
fn emit_extension_hint(sink: &Sink) {
    sink.info(&PamMessage::PamExtensionHint {
        line: PAM_LINE.to_string(),
    });
}

/// The first failure among the rows, for a caller whose answer is a `Result`
/// rather than an exit code. Every service has already been attempted.
fn first_failure(reports: &[ServiceReport]) -> anyhow::Result<()> {
    for report in reports {
        match &report.outcome {
            Outcome::Failed(error) | Outcome::CleanupFailed(error) => {
                return Err(anyhow::anyhow!(error.clone()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// `add` / `remove` against `base`. The engine tests drive this directly, so
/// it performs no root or module check — [`run`] owns those.
fn write_in(dirs: &PamDirs, request: &PamRequest) -> anyhow::Result<i32> {
    let Some(action) = WriteAction::of(request.action) else {
        // `run` routes `status` to `status_in` and never gets here. Delegating
        // rather than falling through is what stops a future caller from
        // getting a *removal* out of a request that asked to read.
        return Ok(status_in(dirs, request));
    };
    let write = WriteRequest {
        action,
        request,
        remedy: "--allow-sensitive",
    };
    let sink = Sink::verb(request.json);

    // Phase one. An `Err` here has written nothing, by construction.
    let targets = plan_writes(dirs, &write)?;
    let reports = apply_all(dirs, &targets, &write, &sink);

    if action == WriteAction::Add {
        emit_extension_hint(&sink);
    }
    emit_json(request, request.dry_run, &reports, &[]);

    Ok(if first_failure(&reports).is_err() {
        WRITE_FAILED
    } else {
        WRITE_OK
    })
}

/// Whether every live reference in `content` has the exact physical spelling
/// emitted by Facelock before versioned state existed.
///
/// The broad parser remains correct for named `pam remove`: an operator who
/// names a service explicitly asked to remove its module rule. Machine-wide
/// cleanup has no such authorization, so spacing, controls and options are
/// ownership evidence rather than cosmetic differences.
fn has_only_exact_legacy_facelock_rules(content: &[u8]) -> bool {
    let mut found = false;
    for rule in PamDocument::new(content).logical_rules() {
        if !is_facelock_rule(rule.bytes) {
            continue;
        }
        found = true;
        let mut next = rule.start;
        let mut saw_exact = false;
        while next < rule.end {
            let Some(line) = PhysicalLine::at(content, next) else {
                return false;
            };
            if line
                .semantic()
                .windows(b"pam_facelock.so".len())
                .any(|window| window == b"pam_facelock.so")
            {
                if line.content() != PAM_LINE.as_bytes() {
                    return false;
                }
                saw_exact = true;
            }
            next = line.end;
        }
        if !saw_exact {
            return false;
        }
    }
    found
}

fn strict_record_names_for_service(root: &Path, service: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(backup) = name.strip_suffix(".json") else {
            continue;
        };
        if backup_service(backup) == Some(service) {
            names.push(name);
        }
    }
    Ok(names)
}

fn remove_all_reference_is_owned(
    store: Option<&BackupStore>,
    service: &str,
    content: &[u8],
) -> anyhow::Result<bool> {
    let Some(store) = store else {
        return Ok(has_only_exact_legacy_facelock_rules(content));
    };
    let strict_names = strict_record_names_for_service(&store.root, service)?;
    let records = store.validated_records(service)?;
    if strict_names.len() != records.len() {
        anyhow::bail!("corrupt PAM provenance for service {service}");
    }
    if records.is_empty() {
        return Ok(has_only_exact_legacy_facelock_rules(content));
    }
    let current = sha256_hex(content);
    if records.iter().any(|record| {
        record.provenance.state == ProvenanceState::Committed
            && record.provenance.installed_sha256 == current
    }) {
        return Ok(true);
    }
    anyhow::bail!("PAM service {service} no longer matches its Facelock provenance")
}

fn remove_all_name_is_candidate(
    store: Option<&BackupStore>,
    service: &str,
) -> std::io::Result<bool> {
    if is_service_name(service) {
        return Ok(true);
    }
    if confined(service).is_err() {
        return Ok(false);
    }
    let Some(store) = store else {
        return Ok(false);
    };
    Ok(!strict_record_names_for_service(&store.root, service)?.is_empty())
}

fn remove_all_services_with_store(
    dirs: &PamDirs,
    open_store: impl FnOnce(&Path) -> std::io::Result<Option<BackupStore>>,
    include_exact_cleanup_intermediates: bool,
) -> anyhow::Result<Vec<String>> {
    let store = open_store(dirs.backup_dir())?;
    let mut services = Vec::new();
    let mut blockers = Vec::new();

    for (root_index, base) in dirs.iter().enumerate() {
        let directory = match open_directory_nofollow(base) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                blockers.push(format!("{} could not be scanned: {error}", base.display()));
                continue;
            }
        };
        let entries = match directory_entry_names(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                blockers.push(format!("{} could not be scanned: {error}", base.display()));
                continue;
            }
        };
        for entry_name in entries {
            let Some(service) = entry_name.to_str().map(str::to_owned) else {
                blockers.push(format!(
                    "{} contains a PAM entry with a non-UTF-8 name",
                    base.display()
                ));
                continue;
            };
            let name_is_candidate = remove_all_name_is_candidate(store.as_ref(), &service)?;
            let entry_metadata = match metadata_at_nofollow(&directory, OsStr::new(&service)) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) if !name_is_candidate => continue,
                Err(error) => {
                    blockers.push(format!(
                        "{} is unmanaged: {error}",
                        base.join(&service).display()
                    ));
                    continue;
                }
            };
            let file_type = entry_metadata.st_mode & libc::S_IFMT;
            if file_type == libc::S_IFDIR {
                continue;
            }
            if !name_is_candidate && (root_index != 0 || file_type != libc::S_IFREG) {
                continue;
            }
            if file_type == libc::S_IFLNK {
                if symlink_is_covered_by_later_root(
                    &directory,
                    &service,
                    &dirs.dirs[root_index + 1..],
                )
                .unwrap_or(false)
                {
                    continue;
                }
                blockers.push(format!(
                    "{} is unmanaged: PAM service is a symlink",
                    base.join(&service).display()
                ));
                continue;
            }
            if file_type != libc::S_IFREG {
                blockers.push(format!(
                    "{} is unmanaged: PAM service is not a regular file",
                    base.join(&service).display()
                ));
                continue;
            }
            let file = match open_regular_at(&directory, OsStr::new(&service)) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error)
                    if error.raw_os_error() == Some(libc::ELOOP)
                        && symlink_is_covered_by_later_root(
                            &directory,
                            &service,
                            &dirs.dirs[root_index + 1..],
                        )
                        .unwrap_or(false) =>
                {
                    continue;
                }
                Err(error) => {
                    blockers.push(format!(
                        "{} is unmanaged: {error}",
                        base.join(&service).display()
                    ));
                    continue;
                }
            };
            let content = match read_open_bounded(&file, MAX_BACKUP_BYTES) {
                Ok(content) => content,
                Err(error) => {
                    blockers.push(format!(
                        "{} is unmanaged: {error}",
                        base.join(&service).display()
                    ));
                    continue;
                }
            };
            let has_facelock_rule = PamDocument::new(&content).has_facelock_rule();
            if root_index != 0 {
                if has_facelock_rule {
                    blockers.push(format!(
                        "{} is an unmanaged reference outside the writable PAM root",
                        base.join(&service).display()
                    ));
                }
                continue;
            }
            let identity = identity_for_bytes(&file.metadata()?, &content);
            let local_origin = Origin::Local;
            let candidate = Target {
                service: service.clone(),
                path: base.join(&service),
                shadowed: shadowed_vendor(dirs, &service, &local_origin),
                origin: local_origin,
                identity: Some(identity.clone()),
                plan: Plan::NoChange,
            };
            match classify_vendor_override(dirs, &candidate, &content, &identity) {
                VendorOverrideDisposition::Unchanged
                    if has_facelock_rule || include_exact_cleanup_intermediates =>
                {
                    services.push(service);
                    continue;
                }
                VendorOverrideDisposition::Unchanged => continue,
                VendorOverrideDisposition::Drifted if has_facelock_rule => {
                    blockers.push(format!(
                        "{} is an administrator-modified vendor override",
                        base.join(&service).display()
                    ));
                    continue;
                }
                VendorOverrideDisposition::Drifted => continue,
                VendorOverrideDisposition::SourceAbsent(vendor) if has_facelock_rule => {
                    blockers.push(format!(
                        "{} is an administrator-modified vendor override: configured vendor source {} is absent",
                        base.join(&service).display(),
                        vendor.display()
                    ));
                    continue;
                }
                VendorOverrideDisposition::SourceAbsent(_) => continue,
                VendorOverrideDisposition::NotFacelock if !name_is_candidate => continue,
                VendorOverrideDisposition::NotFacelock if !has_facelock_rule => continue,
                VendorOverrideDisposition::NotFacelock => {}
            }
            match remove_all_reference_is_owned(store.as_ref(), &service, &content) {
                Ok(true) => services.push(service),
                Ok(false) => blockers.push(format!(
                    "{} is an administrator-managed PAM reference",
                    base.join(&service).display()
                )),
                Err(error) => blockers.push(error.to_string()),
            }
        }
    }

    if !blockers.is_empty() {
        anyhow::bail!(blockers.join("; "));
    }
    services.sort();
    services.dedup();
    Ok(services)
}

fn remove_all_services(dirs: &PamDirs) -> anyhow::Result<Vec<String>> {
    remove_all_services_with_store(dirs, BackupStore::open_existing, true)
}

fn remove_all_active_references(dirs: &PamDirs) -> anyhow::Result<Vec<String>> {
    remove_all_services_with_store(dirs, BackupStore::open_existing, false)
}

fn remove_all_services_read_only(dirs: &PamDirs) -> anyhow::Result<Vec<String>> {
    remove_all_services_with_store(dirs, BackupStore::open_existing_read_only, true)
}

fn valid_remove_all_operation(operation: &str) -> bool {
    let Some((seconds, nanoseconds)) = operation.split_once('-') else {
        return false;
    };
    !seconds.is_empty()
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && seconds.parse::<u64>().is_ok()
        && nanoseconds.len() == 9
        && nanoseconds.bytes().all(|byte| byte.is_ascii_digit())
        && nanoseconds
            .parse::<u32>()
            .is_ok_and(|value| value < 1_000_000_000)
}

fn remove_all_journal_name(operation: &str) -> String {
    format!(".facelock-remove-all-{operation}.json")
}

fn remove_all_commit_name(operation: &str) -> String {
    format!(".facelock-remove-all-commit-{operation}.json")
}

fn valid_remove_all_target(target: &RemoveAllJournalTarget) -> bool {
    confined(&target.service).is_ok()
        && valid_backup_name(&target.service, &target.backup)
        && target.original.links == 1
        && target.original.mode & libc::S_IFMT == libc::S_IFREG
        && valid_sha256(&target.original.sha256)
        && valid_sha256(&target.installed_sha256)
}

fn valid_remove_all_journal(journal: &RemoveAllJournal) -> bool {
    matches!(
        journal.version,
        REMOVE_ALL_LEGACY_VERSION | REMOVE_ALL_VERSION
    ) && match journal.version {
        REMOVE_ALL_VERSION => journal
            .targets
            .iter()
            .all(|target| target.delete_override.is_some()),
        REMOVE_ALL_LEGACY_VERSION => journal
            .targets
            .iter()
            .all(|target| target.delete_override.is_none()),
        _ => false,
    } && valid_remove_all_operation(&journal.operation)
        && !journal.targets.is_empty()
        && journal.targets.len() <= MAX_REMOVE_ALL_TARGETS
        && journal.targets.iter().all(valid_remove_all_target)
        && {
            let mut services = journal
                .targets
                .iter()
                .map(|target| target.service.as_str())
                .collect::<Vec<_>>();
            let original_len = services.len();
            services.sort_unstable();
            services.dedup();
            services.len() == original_len
        }
}

fn valid_remove_all_commit(commit: &RemoveAllCommit) -> bool {
    matches!(
        commit.version,
        REMOVE_ALL_LEGACY_VERSION | REMOVE_ALL_VERSION
    ) && match commit.version {
        REMOVE_ALL_VERSION => commit
            .targets
            .iter()
            .all(|target| target.delete_override.is_some()),
        REMOVE_ALL_LEGACY_VERSION => commit
            .targets
            .iter()
            .all(|target| target.delete_override.is_none()),
        _ => false,
    } && valid_remove_all_operation(&commit.operation)
        && valid_sha256(&commit.journal_sha256)
        && !commit.targets.is_empty()
        && commit.targets.len() <= MAX_REMOVE_ALL_TARGETS
        && commit.targets.iter().all(|target| {
            confined(&target.service).is_ok()
                && valid_backup_name(&target.service, &target.backup)
                && target.installed.links == 1
                && target.installed.mode & libc::S_IFMT == libc::S_IFREG
                && valid_sha256(&target.installed.sha256)
        })
        && {
            let mut services = commit
                .targets
                .iter()
                .map(|target| target.service.as_str())
                .collect::<Vec<_>>();
            let original_len = services.len();
            services.sort_unstable();
            services.dedup();
            services.len() == original_len
        }
}

fn remove_all_binding_identity(binding: &PublicationBinding) -> FileIdentity {
    FileIdentity {
        device: binding.device,
        inode: binding.inode,
        links: binding.links,
        sha256: binding.sha256.clone(),
        mode: binding.mode,
        uid: binding.uid,
        gid: binding.gid,
    }
}

fn create_remove_all_journal(
    store: &BackupStore,
    keep_backup: bool,
    targets: Vec<RemoveAllJournalTarget>,
) -> std::io::Result<(RemoveAllJournal, String, Vec<u8>, FileIdentity)> {
    use std::io::{Error, ErrorKind};

    let mut since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    for _ in 0..MAX_TIMESTAMP_COLLISION_PROBES {
        let operation = format!(
            "{}-{:09}",
            since_epoch.as_secs(),
            since_epoch.subsec_nanos()
        );
        let name = remove_all_journal_name(&operation);
        let commit_name = remove_all_commit_name(&operation);
        if fs::symlink_metadata(store.root.join(&name)).is_ok()
            || fs::symlink_metadata(store.root.join(&commit_name)).is_ok()
        {
            since_epoch = since_epoch
                .checked_add(std::time::Duration::from_nanos(1))
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "remove-all clock overflow"))?;
            continue;
        }
        let journal = RemoveAllJournal {
            version: REMOVE_ALL_VERSION,
            operation,
            keep_backup,
            targets: targets.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&journal)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        if encoded.len() > MAX_REMOVE_ALL_JOURNAL_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "remove-all journal exceeds its size limit",
            ));
        }
        match atomic_state_create(&store.root, &name, &encoded) {
            Ok(identity) => return Ok((journal, name, encoded, identity)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                since_epoch = since_epoch
                    .checked_add(std::time::Duration::from_nanos(1))
                    .ok_or_else(|| {
                        Error::new(ErrorKind::InvalidData, "remove-all clock overflow")
                    })?;
            }
            Err(error) => return Err(error),
        }
    }
    Err(Error::new(
        ErrorKind::AlreadyExists,
        "remove-all journal collision probe limit exhausted",
    ))
}

fn unlink_remove_all_state(
    store: &BackupStore,
    name: &str,
    identity: &FileIdentity,
) -> std::io::Result<()> {
    let directory = open_directory_nofollow(&store.root)?;
    unlink_at_if_identity_matches(&directory, name, identity, MAX_REMOVE_ALL_JOURNAL_BYTES)?;
    directory.sync_all()
}

fn exact_batch_publication(
    store: &BackupStore,
    target: &RemoveAllJournalTarget,
) -> std::io::Result<Option<(ValidIntent, ValidPublication)>> {
    let intents = store.validated_intents()?;
    let publications = store.validated_publications()?;
    let intent_name = intent_name(IntentRole::PamReplace, &target.backup);
    let publication_name = publication_name(PublicationRole::PamReplace, &target.backup);
    let state_directory = open_directory_nofollow(&store.root)?;
    let intent = intents.into_iter().find(|intent| {
        intent.name == intent_name
            && intent.intent.service == target.service
            && intent.intent.original_sha256 == target.original.sha256
            && intent.intent.installed_sha256 == target.installed_sha256
    });
    let publication = publications.into_iter().find(|publication| {
        publication.name == publication_name
            && publication.binding.service == target.service
            && publication.binding.sha256 == target.installed_sha256
    });
    match (intent, publication) {
        (Some(intent), Some(publication))
            if BackupStore::publication_matches_intent(&publication.binding, &intent) =>
        {
            Ok(Some((intent, publication)))
        }
        (None, None)
            if !entry_exists_at(&state_directory, &intent_name)?
                && !entry_exists_at(&state_directory, &publication_name)? =>
        {
            Ok(None)
        }
        _ => Err(std::io::Error::other(
            "remove-all publication evidence is incomplete or invalid",
        )),
    }
}

fn prepared_for_remove_all_target(
    store: &BackupStore,
    target: &RemoveAllJournalTarget,
) -> std::io::Result<PreparedBackup> {
    store
        .validated_records(&target.service)?
        .into_iter()
        .find(|prepared| {
            prepared.backup == target.backup
                && prepared.provenance.state == ProvenanceState::Prepared
                && prepared.provenance.original_sha256 == target.original.sha256
                && prepared.provenance.installed_sha256 == target.installed_sha256
        })
        .ok_or_else(|| std::io::Error::other("remove-all rollback pair is missing or invalid"))
}

fn recover_unstarted_remove_all_publication(
    store: &BackupStore,
    dirs: &PamDirs,
    target: &RemoveAllJournalTarget,
    current: &FileIdentity,
) -> std::io::Result<bool> {
    let intent_name = intent_name(IntentRole::PamReplace, &target.backup);
    let publication_name = publication_name(PublicationRole::PamReplace, &target.backup);
    let state_directory = open_directory_nofollow(&store.root)?;
    if entry_exists_at(&state_directory, &publication_name)? {
        return Ok(false);
    }
    let Some(intent) = store.validated_intents()?.into_iter().find(|intent| {
        intent.name == intent_name
            && intent.intent.role == IntentRole::PamReplace
            && intent.intent.service == target.service
            && intent.intent.backup == target.backup
            && intent.intent.original_sha256 == target.original.sha256
            && intent.intent.installed_sha256 == target.installed_sha256
            && exact_original_intent_identity(&intent.intent, &target.original)
    }) else {
        return Ok(false);
    };
    let prepared = prepared_for_remove_all_target(store, target)?;
    let record_hash = prepared
        .record_identity
        .as_ref()
        .map(|identity| identity.sha256.as_str())
        .ok_or_else(|| std::io::Error::other("remove-all record identity is missing"))?;
    if intent.intent.sequence != prepared.provenance.sequence
        || intent.intent.record_sha256.as_deref() != Some(record_hash)
    {
        return Ok(false);
    }
    if !identity_matches(&target.original, current) {
        return Ok(false);
    }
    let pam_directory = open_directory_nofollow(dirs.overrides())?;
    if entry_exists_at(&pam_directory, &pam_replace_name(&target.backup))? {
        return Ok(false);
    }
    unlink_state_if_identity_matches(&store.root, &intent.name, &intent.identity)?;
    Ok(true)
}

fn remove_all_pair_state_is_absent(
    directory: &fs::File,
    target: &RemoveAllJournalTarget,
) -> std::io::Result<bool> {
    let names = [
        target.backup.clone(),
        format!("{}.json", target.backup),
        quarantine_name("backup", &target.backup),
        format!("{}.json", quarantine_name("record", &target.backup)),
        intent_name(IntentRole::Cleanup, &target.backup),
    ];
    for name in names {
        if entry_exists_at(directory, &name)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cleanup_remove_all_pair(
    store: &BackupStore,
    directory: &fs::File,
    target: &RemoveAllJournalTarget,
) -> std::io::Result<()> {
    let cleanup_name = intent_name(IntentRole::Cleanup, &target.backup);
    let cleanup_intent = store.validated_intents()?.into_iter().find(|intent| {
        intent.name == cleanup_name
            && intent.intent.role == IntentRole::Cleanup
            && intent.intent.service == target.service
            && intent.intent.backup == target.backup
            && intent.intent.original_sha256 == target.original.sha256
            && intent.intent.installed_sha256 == target.installed_sha256
    });
    if let Some(intent) = cleanup_intent {
        store.recover_cleanup_intent(directory, &intent)?;
        if remove_all_pair_state_is_absent(directory, target)? {
            return Ok(());
        }
        return Err(std::io::Error::other(
            "remove-all rollback pair cleanup remains incomplete or ambiguous",
        ));
    }
    if entry_exists_at(directory, &cleanup_name)? {
        return Err(std::io::Error::other(
            "remove-all rollback pair has invalid cleanup evidence",
        ));
    }
    let backup_quarantine = quarantine_name("backup", &target.backup);
    let record_quarantine = format!("{}.json", quarantine_name("record", &target.backup));
    if entry_exists_at(directory, &backup_quarantine)?
        || entry_exists_at(directory, &record_quarantine)?
    {
        return Err(std::io::Error::other(
            "remove-all rollback pair has conflicting quarantine state",
        ));
    }
    match prepared_for_remove_all_target(store, target) {
        Ok(prepared) => store.cleanup_one_at(directory, &prepared, |_| Ok(())),
        Err(_) if remove_all_pair_state_is_absent(directory, target)? => Ok(()),
        Err(_) => Err(std::io::Error::other(
            "remove-all rollback pair is partial, substituted, or conflicting",
        )),
    }
}

fn cleanup_remove_all_pairs(
    store: &BackupStore,
    journal: &RemoveAllJournal,
) -> std::io::Result<()> {
    let directory = open_directory_nofollow(&store.root)?;
    for target in &journal.targets {
        cleanup_remove_all_pair(store, &directory, target)?;
    }
    directory.sync_all()
}

fn finish_rolled_back_remove_all_publication(
    store: &BackupStore,
    dirs: &PamDirs,
    target: &RemoveAllJournalTarget,
    publication: &ValidPublication,
    temp_name: &str,
    after_boundary: &mut impl FnMut(RemoveAllRollbackPoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = open_directory_nofollow(dirs.overrides())?;
    let current = open_identity_at(&directory, &target.service, MAX_BACKUP_BYTES)?
        .ok_or_else(|| std::io::Error::other("rolled-back PAM service is missing"))?;
    if !identity_matches(&target.original, &current) {
        return Err(std::io::Error::other(
            "rolled-back PAM service no longer matches its original identity",
        ));
    }

    if let Some(temp) = open_identity_at(&directory, temp_name, MAX_BACKUP_BYTES)? {
        if !binding_identity_matches(&publication.binding, &temp) {
            return Err(std::io::Error::other(
                "rolled-back PAM replacement temp is ambiguous",
            ));
        }
        unlink_at_if_identity_matches(&directory, temp_name, &temp, MAX_BACKUP_BYTES)?;
        directory.sync_all()?;
        after_boundary(RemoveAllRollbackPoint::TempUnlink)?;
    }

    unlink_state_if_identity_matches(&store.root, &publication.name, &publication.identity)?;
    after_boundary(RemoveAllRollbackPoint::BindingUnlink)?;

    let current = open_identity_at(&directory, &target.service, MAX_BACKUP_BYTES)?
        .ok_or_else(|| std::io::Error::other("rolled-back PAM service is missing"))?;
    if !recover_unstarted_remove_all_publication(store, dirs, target, &current)? {
        return Err(std::io::Error::other(
            "rolled-back PAM intent is not exact unstarted evidence",
        ));
    }
    after_boundary(RemoveAllRollbackPoint::IntentUnlink)
}

fn rollback_remove_all(
    store: &BackupStore,
    dirs: &PamDirs,
    journal: &RemoveAllJournal,
    journal_name: &str,
    journal_identity: &FileIdentity,
) -> std::io::Result<()> {
    rollback_remove_all_with_hook(store, dirs, journal, journal_name, journal_identity, |_| {
        Ok(())
    })
}

fn rollback_remove_all_with_hook(
    store: &BackupStore,
    dirs: &PamDirs,
    journal: &RemoveAllJournal,
    journal_name: &str,
    journal_identity: &FileIdentity,
    mut after_boundary: impl FnMut(RemoveAllRollbackPoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut ambiguous = Vec::new();
    for target in journal.targets.iter().rev() {
        let canonical = dirs.overrides().join(&target.service);
        let current = match read_regular_nofollow(&canonical) {
            Ok((_, identity)) => identity,
            Err(error) => {
                ambiguous.push(format!("{}: {error}", target.service));
                continue;
            }
        };
        match recover_unstarted_remove_all_publication(store, dirs, target, &current) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                ambiguous.push(format!("{}: {error}", target.service));
                continue;
            }
        }
        match exact_batch_publication(store, target) {
            Ok(Some((_intent, publication))) => {
                let installed = remove_all_binding_identity(&publication.binding);
                let directory = match open_directory_nofollow(dirs.overrides()) {
                    Ok(directory) => directory,
                    Err(error) => {
                        ambiguous.push(format!("{}: {error}", target.service));
                        continue;
                    }
                };
                let temp_name = pam_replace_name(&target.backup);
                let temp = match open_identity_at(&directory, &temp_name, MAX_BACKUP_BYTES) {
                    Ok(identity) => identity,
                    Err(error) => {
                        ambiguous.push(format!("{}: {error}", target.service));
                        continue;
                    }
                };
                if identity_matches(&installed, &current)
                    && temp
                        .as_ref()
                        .is_some_and(|temp| identity_matches(&target.original, temp))
                {
                    if let Err(error) = rename_exchange_at(&directory, &temp_name, &target.service)
                    {
                        ambiguous.push(format!("{} rollback failed: {error}", target.service));
                        continue;
                    }
                    after_boundary(RemoveAllRollbackPoint::ReverseExchange)?;
                } else if !identity_matches(&target.original, &current)
                    || temp
                        .as_ref()
                        .is_some_and(|temp| !identity_matches(&installed, temp))
                {
                    ambiguous.push(format!("{} changed before rollback", target.service));
                    continue;
                }
                finish_rolled_back_remove_all_publication(
                    store,
                    dirs,
                    target,
                    &publication,
                    &temp_name,
                    &mut after_boundary,
                )?;
            }
            Ok(None) if identity_matches(&target.original, &current) => {}
            Ok(None) => ambiguous.push(format!("{} changed after preflight", target.service)),
            Err(error) => ambiguous.push(format!("{}: {error}", target.service)),
        }
    }

    if !ambiguous.is_empty() {
        return Err(std::io::Error::other(format!(
            "remove-all rollback retained evidence: {}",
            ambiguous.join("; ")
        )));
    }
    cleanup_remove_all_pairs(store, journal)?;
    unlink_remove_all_state(store, journal_name, journal_identity)
}

fn committed_remove_all_targets(
    store: &BackupStore,
    journal: &RemoveAllJournal,
) -> std::io::Result<Vec<RemoveAllCommittedTarget>> {
    journal
        .targets
        .iter()
        .map(|target| {
            let (_, publication) = exact_batch_publication(store, target)?.ok_or_else(|| {
                std::io::Error::other("remove-all publication binding disappeared before commit")
            })?;
            let installed = remove_all_binding_identity(&publication.binding);
            Ok(RemoveAllCommittedTarget {
                service: target.service.clone(),
                backup: target.backup.clone(),
                installed,
                delete_override: target.delete_override,
            })
        })
        .collect()
}

fn finish_committed_remove_all(
    store: &BackupStore,
    dirs: &PamDirs,
    journal: Option<(&RemoveAllJournal, &str, &FileIdentity)>,
    commit: &RemoveAllCommit,
    commit_name: &str,
    commit_identity: &FileIdentity,
) -> std::io::Result<()> {
    finish_committed_remove_all_with_hook(
        store,
        dirs,
        journal,
        commit,
        commit_name,
        commit_identity,
        |_| Ok(()),
    )
}

fn journal_vendor_service_matches(
    store: &BackupStore,
    dirs: &PamDirs,
    target: &RemoveAllJournalTarget,
) -> std::io::Result<bool> {
    journal_vendor_service_matches_with_hook(store, dirs, target, || Ok(()))
}

fn journal_vendor_service_matches_with_hook(
    store: &BackupStore,
    dirs: &PamDirs,
    target: &RemoveAllJournalTarget,
    after_prepared: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<bool> {
    let prepared = prepared_for_remove_all_target(store, target)?;
    after_prepared()?;
    let state_directory = open_directory_nofollow(&store.root)?;
    let backup = open_regular_at(&state_directory, OsStr::new(&prepared.backup))?;
    let original = read_open_bounded(&backup, MAX_BACKUP_BYTES)?;
    let reopened_backup = identity_for_bytes(&backup.metadata()?, &original);
    let expected_backup = prepared
        .backup_identity
        .as_ref()
        .ok_or_else(|| std::io::Error::other("journaled backup identity is missing"))?;
    if !identity_matches(expected_backup, &reopened_backup) {
        return Err(std::io::Error::other(
            "journaled backup changed before vendor validation",
        ));
    }
    let without_line = with_line_removed(&original);
    let Some(vendor) = resolve_current_vendor(dirs, &target.service)? else {
        return Ok(false);
    };
    Ok(
        exact_vendor_override_shape(&original, &target.original, &vendor)
            && sha256_hex(&without_line) == target.installed_sha256,
    )
}

fn finish_committed_remove_all_with_hook(
    store: &BackupStore,
    dirs: &PamDirs,
    journal: Option<(&RemoveAllJournal, &str, &FileIdentity)>,
    commit: &RemoveAllCommit,
    commit_name: &str,
    commit_identity: &FileIdentity,
    mut after_boundary: impl FnMut(RemoveAllPoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for target in &commit.targets {
        let path = dirs.overrides().join(&target.service);
        match read_regular_nofollow(&path) {
            Ok((_, current)) if identity_matches(&target.installed, &current) => {}
            Err(error)
                if target.delete_override.unwrap_or(false)
                    && error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(std::io::Error::other(format!(
                    "{} changed before remove-all cleanup",
                    target.service
                )));
            }
        }
    }
    store.recover_publications(dirs)?;
    store.recover_intents(dirs)?;

    let state_directory = open_directory_nofollow(&store.root)?;
    for target in &commit.targets {
        let exact_intent = intent_name(IntentRole::PamReplace, &target.backup);
        let exact_binding = publication_name(PublicationRole::PamReplace, &target.backup);
        if entry_exists_at(&state_directory, &exact_intent)?
            || entry_exists_at(&state_directory, &exact_binding)?
        {
            return Err(std::io::Error::other(
                "remove-all publication evidence could not be finalized",
            ));
        }
    }

    for (index, target) in commit.targets.iter().enumerate() {
        if !target.delete_override.unwrap_or(false) {
            continue;
        }
        after_boundary(RemoveAllPoint::BeforeOverrideDelete(index))?;
        if journal.is_none() {
            let override_directory = open_directory_nofollow(dirs.overrides())?;
            let canonical_exists = entry_exists_at(&override_directory, &target.service)?;
            let quarantine_exists =
                entry_exists_at(&override_directory, &vendor_retire_name(&target.backup))?;
            if !canonical_exists && !quarantine_exists {
                continue;
            }
            return Err(std::io::Error::other(format!(
                "{} retains vendor-retirement state without its journal",
                target.service
            )));
        }
        let planned = journal
            .as_ref()
            .and_then(|(journal, _, _)| {
                journal
                    .targets
                    .iter()
                    .find(|planned| planned.service == target.service)
            })
            .ok_or_else(|| {
                std::io::Error::other(format!("{} has no journaled vendor source", target.service))
            })?;
        if !journal_vendor_service_matches(store, dirs, planned)? {
            return Err(std::io::Error::other(format!(
                "{} no longer matches its journaled vendor service",
                target.service
            )));
        }
        retire_vendor_override_with_hook(
            dirs,
            &target.service,
            &target.backup,
            &target.installed,
            |point| match point {
                VendorRetirePoint::Quarantined => {
                    after_boundary(RemoveAllPoint::OverrideQuarantined(index))
                }
                VendorRetirePoint::BeforeFinalValidation => {
                    after_boundary(RemoveAllPoint::BeforeOverrideFinalValidation(index))
                }
                VendorRetirePoint::Restored => {
                    after_boundary(RemoveAllPoint::OverrideRestored(index))
                }
                VendorRetirePoint::Unlinked => {
                    after_boundary(RemoveAllPoint::AfterOverrideDelete(index))
                }
            },
        )?;
    }

    for target in &commit.targets {
        if commit.keep_backup {
            let prepared = store
                .validated_records(&target.service)?
                .into_iter()
                .find(|record| record.backup == target.backup)
                .ok_or_else(|| {
                    std::io::Error::other("remove-all rollback pair disappeared before commit")
                })?;
            if prepared.provenance.state == ProvenanceState::Prepared {
                store.commit_unlocked(&prepared, |_| Ok(()))?;
            }
        } else {
            for prepared in store.validated_records(&target.service)? {
                store.cleanup_one_at(&state_directory, &prepared, |_| Ok(()))?;
            }
            remove_legacy_backup(dirs, &target.service)?;
        }
    }
    state_directory.sync_all()?;

    if let Some((_, name, identity)) = journal {
        unlink_remove_all_state(store, name, identity)?;
        after_boundary(RemoveAllPoint::JournalUnlinked)?;
    }
    unlink_remove_all_state(store, commit_name, commit_identity)?;
    after_boundary(RemoveAllPoint::CommitUnlinked)
}

#[derive(Debug)]
struct LoadedRemoveAll<T> {
    name: String,
    value: T,
    encoded: Vec<u8>,
    identity: FileIdentity,
}

type LoadedRemoveAllState = (
    Option<LoadedRemoveAll<RemoveAllJournal>>,
    Option<LoadedRemoveAll<RemoveAllCommit>>,
);

fn load_remove_all_state(store: &BackupStore) -> std::io::Result<LoadedRemoveAllState> {
    let directory = open_directory_nofollow(&store.root)?;
    let mut journal = None;
    let mut commit = None;
    for entry in fs::read_dir(&store.root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let commit_operation = name
            .strip_prefix(".facelock-remove-all-commit-")
            .and_then(|rest| rest.strip_suffix(".json"))
            .filter(|operation| valid_remove_all_operation(operation));
        let journal_operation = name
            .strip_prefix(".facelock-remove-all-")
            .and_then(|rest| rest.strip_suffix(".json"))
            .filter(|operation| valid_remove_all_operation(operation));
        let (operation, is_commit) = if let Some(operation) = commit_operation {
            (operation, true)
        } else if let Some(operation) = journal_operation {
            (operation, false)
        } else {
            continue;
        };
        let file = open_regular_at(&directory, OsStr::new(&name)).map_err(|error| {
            std::io::Error::other(format!("reserved remove-all state is invalid: {error}"))
        })?;
        let metadata = file.metadata()?;
        let encoded = read_open_bounded(&file, MAX_REMOVE_ALL_JOURNAL_BYTES)?;
        let identity = identity_for_bytes(&metadata, &encoded);
        if !state_identity_matches(store.expected_owner, &identity, &identity.sha256) {
            return Err(std::io::Error::other(
                "reserved remove-all state owner or mode is invalid",
            ));
        }
        if is_commit {
            let value: RemoveAllCommit = serde_json::from_slice(&encoded).map_err(|error| {
                std::io::Error::other(format!("remove-all commit is corrupt: {error}"))
            })?;
            if operation != value.operation || !valid_remove_all_commit(&value) {
                return Err(std::io::Error::other("remove-all commit is invalid"));
            }
            if commit
                .replace(LoadedRemoveAll {
                    name,
                    value,
                    encoded,
                    identity,
                })
                .is_some()
            {
                return Err(std::io::Error::other(
                    "multiple remove-all commits require manual review",
                ));
            }
            continue;
        }
        let value: RemoveAllJournal = serde_json::from_slice(&encoded).map_err(|error| {
            std::io::Error::other(format!("remove-all journal is corrupt: {error}"))
        })?;
        if operation != value.operation || !valid_remove_all_journal(&value) {
            return Err(std::io::Error::other("remove-all journal is invalid"));
        }
        if journal
            .replace(LoadedRemoveAll {
                name,
                value,
                encoded,
                identity,
            })
            .is_some()
        {
            return Err(std::io::Error::other(
                "multiple remove-all journals require manual review",
            ));
        }
    }
    if let (Some(journal), Some(commit)) = (&journal, &commit) {
        let corresponding = commit.value.operation == journal.value.operation
            && commit.value.keep_backup == journal.value.keep_backup
            && commit.value.journal_sha256 == sha256_hex(&journal.encoded)
            && commit.value.targets.len() == journal.value.targets.len()
            && commit.value.targets.iter().zip(&journal.value.targets).all(
                |(committed, planned)| {
                    committed.service == planned.service
                        && committed.backup == planned.backup
                        && committed.installed.sha256 == planned.installed_sha256
                        && committed.delete_override == planned.delete_override
                },
            );
        if !corresponding {
            return Err(std::io::Error::other(
                "remove-all journal and commit do not describe one transaction",
            ));
        }
    }
    Ok((journal, commit))
}

#[cfg(test)]
fn recover_remove_all_in(dirs: &PamDirs) -> std::io::Result<()> {
    let Some(store) = BackupStore::open_existing(dirs.backup_dir())? else {
        return Ok(());
    };
    let _lock = store.lock_exclusive()?;
    recover_remove_all_locked(&store, dirs)
}

fn recover_remove_all_locked(store: &BackupStore, dirs: &PamDirs) -> std::io::Result<()> {
    let (journal, commit) = load_remove_all_state(store)?;
    if let Some(commit) = commit {
        return finish_committed_remove_all(
            store,
            dirs,
            journal
                .as_ref()
                .map(|journal| (&journal.value, journal.name.as_str(), &journal.identity)),
            &commit.value,
            &commit.name,
            &commit.identity,
        );
    }
    if let Some(journal) = journal {
        rollback_remove_all(
            store,
            dirs,
            &journal.value,
            &journal.name,
            &journal.identity,
        )?;
    }
    Ok(())
}

fn remove_all_in_with_report_hook(
    dirs: &PamDirs,
    request: &PamRequest,
    hook: impl FnMut(RemoveAllPoint) -> std::io::Result<()>,
    mut report_hook: impl FnMut(&[ServiceReport]),
) -> anyhow::Result<i32> {
    use std::cell::RefCell;
    use std::io::{Error, ErrorKind};

    debug_assert_eq!(request.action, PamAction::Remove);
    debug_assert!(request.all);
    if request.dry_run {
        let services = remove_all_services_read_only(dirs)?;
        if services.is_empty() {
            report_hook(&[]);
            return Ok(WRITE_OK);
        }
        if services.len() > MAX_REMOVE_ALL_TARGETS {
            anyhow::bail!("remove-all target limit exceeded");
        }
        let mut named = request.clone();
        named.all = false;
        named.services = services;
        return write_in(dirs, &named);
    }

    // Avoid creating state for a clear no-op or a rejected first preflight.
    // Once state exists, the second scan below is authoritative and happens
    // while the same transaction lock is held through final cleanup.
    let store = match BackupStore::open_existing(dirs.backup_dir())? {
        Some(store) => store,
        None => {
            let services = remove_all_services(dirs)?;
            if services.is_empty() {
                report_hook(&[]);
                return Ok(WRITE_OK);
            }
            BackupStore::open(dirs.backup_dir())?
        }
    };
    let transaction = store.transaction(dirs)?;
    let hook = RefCell::new(hook);
    hook.borrow_mut()(RemoveAllPoint::Locked)?;
    let services = remove_all_services(dirs)?;
    if services.is_empty() {
        report_hook(&[]);
        return Ok(WRITE_OK);
    }
    if services.len() > MAX_REMOVE_ALL_TARGETS {
        anyhow::bail!("remove-all target limit exceeded");
    }
    let mut named = request.clone();
    named.all = false;
    named.services = services;
    let write = WriteRequest {
        action: WriteAction::Remove,
        request: &named,
        remedy: "--allow-sensitive",
    };
    let targets = plan_writes(dirs, &write)?;
    let mut planned = Vec::with_capacity(targets.len());
    for target in targets {
        let (content, delete_override) = match &target.plan {
            Plan::Rewrite { content } => (content, false),
            Plan::DeleteOverride { content } => (content, true),
            _ => {
                anyhow::bail!(
                    "remove-all target {} no longer needs a rewrite",
                    target.service
                );
            }
        };
        let expected = target.identity.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "remove-all target {} has no captured identity",
                target.service
            )
        })?;
        let installed = with_line_removed(content);
        let prepared = transaction.plan(&target.service, content, &installed)?;
        transaction.persist(&prepared, content)?;
        planned.push((target, installed, prepared, expected, delete_override));
    }
    let journal_targets = planned
        .iter()
        .map(
            |(target, installed, prepared, expected, delete_override)| RemoveAllJournalTarget {
                service: target.service.clone(),
                backup: prepared.backup.clone(),
                original: expected.clone(),
                installed_sha256: sha256_hex(installed),
                delete_override: Some(*delete_override),
            },
        )
        .collect();
    let (journal, journal_name, journal_encoded, journal_identity) =
        create_remove_all_journal(&store, request.keep_backup, journal_targets)?;
    if let Err(error) = hook.borrow_mut()(RemoveAllPoint::Journaled) {
        return Err(error.into());
    }

    const HELD: &str = "remove-all publication retained for batch commit";
    for (index, (target, installed, prepared, expected, _)) in planned.iter().enumerate() {
        if let Err(error) = hook.borrow_mut()(RemoveAllPoint::BeforeMutation(index)) {
            if error.kind() == ErrorKind::Interrupted {
                return Err(error.into());
            }
            let rollback =
                rollback_remove_all(&store, dirs, &journal, &journal_name, &journal_identity);
            return Err(anyhow::anyhow!(match rollback {
                Ok(()) => error.to_string(),
                Err(rollback) => format!("{error}; {rollback}"),
            }));
        }
        let user_error = RefCell::new(None);
        let result = transaction.replace_pam_with_intent_hook(
            prepared,
            &target.path,
            expected,
            installed,
            |point| {
                if point != PamReplaceCrashPoint::Exchange {
                    return Ok(());
                }
                match hook.borrow_mut()(RemoveAllPoint::AfterMutation(index)) {
                    Ok(()) => Err(Error::new(ErrorKind::Interrupted, HELD)),
                    Err(error) => {
                        *user_error.borrow_mut() = Some(error);
                        // Publication has already exchanged the two complete
                        // inodes. Keep the per-file intent and binding until
                        // the batch rollback consumes them, regardless of the
                        // caller error's original kind.
                        Err(Error::new(
                            ErrorKind::Interrupted,
                            "remove-all hook stopped publication",
                        ))
                    }
                }
            },
        );
        if let Some(error) = user_error.into_inner() {
            if error.kind() == ErrorKind::Interrupted {
                return Err(error.into());
            }
            let rollback =
                rollback_remove_all(&store, dirs, &journal, &journal_name, &journal_identity);
            return Err(anyhow::anyhow!(match rollback {
                Ok(()) => error.to_string(),
                Err(rollback) => format!("{error}; {rollback}"),
            }));
        }
        match result {
            Err(error) if error.kind() == ErrorKind::Interrupted && error.to_string() == HELD => {}
            Err(error) if is_ambiguous_publication(&error) => return Err(error.into()),
            Err(error) => {
                let rollback =
                    rollback_remove_all(&store, dirs, &journal, &journal_name, &journal_identity);
                return Err(anyhow::anyhow!(match rollback {
                    Ok(()) => error.to_string(),
                    Err(rollback) => format!("{error}; {rollback}"),
                }));
            }
            Ok(()) => {
                return Err(anyhow::anyhow!(
                    "remove-all publication finalized without retained recovery evidence"
                ));
            }
        }
    }

    if let Err(error) = remove_all_active_references(dirs).and_then(|remaining| {
        if remaining.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("remove-all final scan still found active references")
        }
    }) {
        let rollback =
            rollback_remove_all(&store, dirs, &journal, &journal_name, &journal_identity);
        return Err(anyhow::anyhow!(match rollback {
            Ok(()) => error.to_string(),
            Err(rollback) => format!("{error}; {rollback}"),
        }));
    }

    let committed_targets = committed_remove_all_targets(&store, &journal)?;
    let commit = RemoveAllCommit {
        version: REMOVE_ALL_VERSION,
        operation: journal.operation.clone(),
        journal_sha256: sha256_hex(&journal_encoded),
        keep_backup: journal.keep_backup,
        targets: committed_targets,
    };
    let commit_encoded = serde_json::to_vec_pretty(&commit)
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    if commit_encoded.len() > MAX_REMOVE_ALL_JOURNAL_BYTES {
        anyhow::bail!("remove-all commit exceeds its size limit");
    }
    let commit_name = remove_all_commit_name(&journal.operation);
    let commit_identity = match atomic_state_create(&store.root, &commit_name, &commit_encoded) {
        Ok(identity) => identity,
        Err(error) if is_ambiguous_publication(&error) => return Err(error.into()),
        Err(error) => {
            let rollback =
                rollback_remove_all(&store, dirs, &journal, &journal_name, &journal_identity);
            return Err(anyhow::anyhow!(match rollback {
                Ok(()) => error.to_string(),
                Err(rollback) => format!("{error}; {rollback}"),
            }));
        }
    };
    hook.borrow_mut()(RemoveAllPoint::CommitMarked)?;
    finish_committed_remove_all_with_hook(
        &store,
        dirs,
        Some((&journal, &journal_name, &journal_identity)),
        &commit,
        &commit_name,
        &commit_identity,
        |point| hook.borrow_mut()(point),
    )?;
    let reports = planned
        .iter()
        .map(|(target, _, _, _, _)| ServiceReport {
            service: target.service.clone(),
            path: Some(target.reported_path()),
            backup: request
                .keep_backup
                .then(|| reported_backup(dirs, target))
                .flatten(),
            shadows: if matches!(target.plan, Plan::DeleteOverride { .. }) {
                None
            } else {
                target.shadows_string()
            },
            outcome: Outcome::Removed,
        })
        .collect::<Vec<_>>();
    report_hook(&reports);
    Ok(WRITE_OK)
}

fn remove_all_in_with_hook(
    dirs: &PamDirs,
    request: &PamRequest,
    hook: impl FnMut(RemoveAllPoint) -> std::io::Result<()>,
) -> anyhow::Result<i32> {
    remove_all_in_with_report_hook(dirs, request, hook, |reports| {
        emit_json(request, request.dry_run, reports, &[]);
    })
}

fn remove_all_in(dirs: &PamDirs, request: &PamRequest) -> anyhow::Result<i32> {
    remove_all_in_with_hook(dirs, request, |_| Ok(()))
}

/// `pam status` against `base`.
///
/// Unprivileged by design (DEC-6): `/etc/pam.d/*` is `0644`, this never
/// writes, and it is the probe an integration wants without `sudo` — the one
/// that replaces `grep -q pam_facelock.so /etc/pam.d/<service>`. An unreadable
/// file reports `unknown` and exits 2 rather than pretending it is missing.
///
/// The exit code is the answer, on `grep`'s scale and `is-enrolled`'s: 0 every
/// service has the line, 1 at least one does not, 2 at least one could not be
/// answered. The worst outcome wins.
fn status_in(dirs: &PamDirs, request: &PamRequest) -> i32 {
    let sink = Sink::verb(request.json);
    if request.all {
        return status_all_in(dirs, request, &sink);
    }
    let reports = status_reports(dirs, &requested_services(&request.services), &sink);

    emit_json(request, false, &reports, &[]);

    reports
        .iter()
        .map(|report| status_code(&report.outcome, request.if_present))
        .max()
        .unwrap_or(STATUS_PRESENT)
}

/// `pam status --all`: every service on the search path that names the module.
///
/// The rows come from the same [`status_reports`] the named form uses, so a
/// service reported here is reported identically when it is asked for by name.
/// Three things decide the exit code, and each is a different question:
///
/// - the worst row, exactly as in the named form;
/// - **nothing configured is [`STATUS_MISSING`]**, not 0. A machine with no
///   facelock line anywhere is not "fine", it is not set up, and it is the
///   same answer `pam status` gives for a service file with no line in it.
///   `--if-present` does not change it either: there are no `absent` rows to
///   forgive here — a name only reaches the report by having been found — so
///   the flag has nothing to convert;
/// - **a directory that would not list is [`STATUS_ERROR`]**, because the
///   enumeration is then incomplete and a 0 or a 1 would be a claim about
///   services this never saw.
fn status_all_in(dirs: &PamDirs, request: &PamRequest, sink: &Sink) -> i32 {
    let scan = scan_directories(dirs);
    let reports = status_reports(dirs, &scan.names, sink);

    // After the rows: the services are the answer, and this is why the answer
    // may be short. On stderr, like every other "could not tell" line here.
    let unchecked: Vec<String> = scan
        .unreadable()
        .map(|(path, error)| {
            sink.error(&PamMessage::PamStatusDirUnreadable {
                dir: path.display().to_string(),
                error: error.to_string(),
            });
            path.display().to_string()
        })
        .collect();

    // **The empty answer has to be qualified by what could not be looked at.**
    // An unqualified "no service file under <every directory> carries the
    // line" is a claim about a directory this failed to open, and read
    // `2>/dev/null` — the ordinary way to take the human answer — it asserts
    // exactly what enumeration exists to stop it asserting. Three cases, and
    // the third is why this is not a one-line guard: when *nothing* could be
    // read there is no set of directories to say "none here" about, and the
    // per-directory lines above are already the whole answer.
    if reports.is_empty() {
        let answered = scan.answered();
        if unchecked.is_empty() {
            sink.info(&PamMessage::PamStatusNoServices {
                dirs: dirs.display(),
            });
        } else if !answered.is_empty() {
            sink.info(&PamMessage::PamStatusNoServicesIncomplete {
                dirs: answered.join(", "),
                unchecked: unchecked.join(", "),
            });
        } else {
            // Nothing was read, so there is no set of directories to say
            // "none here" about. stdout still gets a line: a human reading it
            // alone would otherwise get a sentence in the other two branches
            // and silence in the one where the machine is worst off. It
            // asserts only what is true — that nothing could be read.
            sink.info(&PamMessage::PamStatusNothingReadable {
                dirs: unchecked.join(", "),
            });
        }
    }

    emit_json(request, false, &reports, &scan.directories);

    let worst = reports
        .iter()
        .map(|report| status_code(&report.outcome, request.if_present))
        .max()
        .unwrap_or(STATUS_PRESENT);
    let nothing_configured = if reports.is_empty() {
        STATUS_MISSING
    } else {
        STATUS_PRESENT
    };
    let unreadable = if unchecked.is_empty() {
        STATUS_PRESENT
    } else {
        STATUS_ERROR
    };
    worst.max(nothing_configured).max(unreadable)
}

/// One report row per service. Split out from [`status_in`] so the rows —
/// which are the `--json` payload — are assertable without capturing stdout.
fn status_reports(dirs: &PamDirs, services: &[String], sink: &Sink) -> Vec<ServiceReport> {
    services
        .iter()
        .map(|service| {
            // Both rejections read as `unknown`, the same word an unreadable
            // file gets: the question "does this service carry the line?" has
            // no answer this may go and look for. Not an `Err` return either
            // — `status` owns its exit codes, and a usage error is exit 2
            // rather than the generic exit 1 `main` gives an `anyhow` failure.
            let target = match Target::locate(dirs, service) {
                Ok(target) => target,
                Err(why) => {
                    sink.error(&why.message(service));
                    return ServiceReport {
                        service: service.clone(),
                        path: why.path(),
                        outcome: Outcome::Unknown(why.reason().to_string()),
                        backup: why.backup(),
                        // Nothing was resolved, so there is nothing this could
                        // be shadowing.
                        shadows: None,
                    };
                }
            };
            let display = target.path_string();

            let vendor_only = matches!(target.origin, Origin::Vendor { .. });

            let outcome = match read_regular_nofollow(&target.path).map(|(content, _)| content) {
                // Read before the vendor test, not after: a service whose
                // vendor file already carries the line — a distribution that
                // ships face auth in its own PAM stack — *is* configured, and
                // reporting it `vendor-only` would send an integrator off to
                // create an override that adds nothing.
                Ok(content) if PamDocument::new(&content).has_facelock_rule() => {
                    // Configured either way; the second line says the copy is
                    // a local override, which is what tells an operator it
                    // will not follow the package's updates.
                    match &target.shadowed {
                        Some(vendor) => sink.info(&PamMessage::PamStatusOverride {
                            path: display.clone(),
                            vendor: vendor.display().to_string(),
                        }),
                        None => sink.info(&PamMessage::PamStatusPresent {
                            path: display.clone(),
                        }),
                    }
                    Outcome::Present
                }
                // Exists, carries no line, and has no `/etc/pam.d` copy for
                // one to be added to. `missing` would be true of the file and
                // misleading about the machine: `add` will create an override
                // here rather than edit what `status` just named.
                Ok(_) if vendor_only => {
                    sink.info(&PamMessage::PamStatusVendorOnly {
                        path: display.clone(),
                    });
                    Outcome::VendorOnly
                }
                Ok(_) => {
                    sink.info(&PamMessage::PamStatusMissing {
                        path: display.clone(),
                    });
                    Outcome::Missing
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Every path tried, like `add`'s refusal: the same
                    // question must not be answered two ways by two verbs.
                    // The row's `path` field stays the first directory's —
                    // where an override would go — because it is one string,
                    // and `contracts.md` says which of the two it is.
                    sink.info(&PamMessage::PamStatusAbsent {
                        paths: target.tried_paths(),
                    });
                    Outcome::Absent
                }
                Err(error) => {
                    sink.error(&PamMessage::PamStatusUnknown {
                        path: display.clone(),
                        error: error.to_string(),
                    });
                    Outcome::Unknown(error.to_string())
                }
            };

            ServiceReport {
                service: target.service.clone(),
                path: Some(display),
                outcome,
                backup: reported_backup(dirs, &target),
                shadows: target.shadows_string(),
            }
        })
        .collect()
}

/// `grep`'s scale, and `is-enrolled`'s. The worst outcome across the requested
/// services wins, which is why this is a function of one outcome and the
/// caller takes the max.
fn status_code(outcome: &Outcome, if_present: bool) -> i32 {
    match outcome {
        Outcome::Present => STATUS_PRESENT,
        Outcome::Missing => STATUS_MISSING,
        // `--if-present` means the same thing here as on `add` and `remove`:
        // a service file that is not installed is not an error, so the row is
        // still reported and the exit code is decided by the services that do
        // exist. It converts *absence* and nothing else.
        Outcome::Absent if if_present => STATUS_PRESENT,
        Outcome::Absent | Outcome::Unknown(_) => STATUS_ERROR,
        // The service exists and does not carry the line, which is exactly
        // what `missing` means to a caller branching on the exit code. The
        // word is what says *why* `add` will behave differently here, and
        // `--if-present` does not convert it: this is not an absence.
        Outcome::VendorOnly => STATUS_MISSING,
        // Not reachable: `status_reports` produces the five words above and no
        // other. Spelled out rather than left to a `_` arm so that a word
        // added to the vocabulary has to be given a code here on purpose —
        // under a wildcard, a new `status` outcome would silently be exit 2.
        Outcome::Installed
        | Outcome::Overridden
        | Outcome::Removed
        | Outcome::Unchanged
        | Outcome::Declined
        | Outcome::Failed(_)
        | Outcome::CleanupFailed(_) => STATUS_ERROR,
    }
}

// ---------------------------------------------------------------------------
// The `setup --pam` alias
// ---------------------------------------------------------------------------

/// The three aliases take the same [`PamRequest`] the verb does.
///
/// They used to take loose parameters, in two different orders —
/// `install_for_setup(services, no_confirm, allow_sensitive)` beside
/// `install_one_in(base, service, allow_sensitive, no_confirm)` — so a swapped
/// pair type-checked and quietly unlocked the sensitive-service gate. `setup`
/// builds a request, names every field, and the writer reads the same value in
/// both phases.
///
/// The `remedy` is `--allow-sensitive` on every setup alias. `setup --yes`
/// suppresses the ordinary confirmation only, matching `pam add --yes`.
///
/// Root is re-checked in the two that `run_with_plan` reaches directly for a
/// standalone `--pam`, which does not take the base setup's root pre-check.
/// `install_one_in` is the wizard's, and the wizard has already checked.
///
/// `setup --pam [--service X]`.
pub(crate) fn install_for_setup(request: &PamRequest) -> anyhow::Result<()> {
    debug_assert_eq!(request.action, PamAction::Add);
    crate::ipc_client::require_root_scripted("sudo facelock setup --pam")?;
    require_module_installed()?;

    let write = WriteRequest {
        action: WriteAction::Add,
        request,
        remedy: "--allow-sensitive",
    };
    let sink = Sink::human();
    let dirs = PamDirs::system();
    let reports = apply_all(&dirs, &plan_writes(&dirs, &write)?, &write, &sink);
    // Before the hint, which the alias has never printed after a failure —
    // unlike the verb, whose closing hint fires whatever the rows say.
    first_failure(&reports)?;
    emit_extension_hint(&sink);
    Ok(())
}

/// `setup --pam --remove [--if-present]`.
pub(crate) fn remove_for_setup(request: &PamRequest) -> anyhow::Result<()> {
    debug_assert_eq!(request.action, PamAction::Remove);
    crate::ipc_client::require_root_scripted("sudo facelock setup --pam --remove")?;

    let write = WriteRequest {
        action: WriteAction::Remove,
        request,
        remedy: "--allow-sensitive",
    };
    let sink = Sink::human();
    let dirs = PamDirs::system();
    let reports = apply_all(&dirs, &plan_writes(&dirs, &write)?, &write, &sink);
    first_failure(&reports)
}

/// One service, against `base`, with the wizard's semantics: the multi-select
/// already *is* the per-service consent, and the module check already ran, so
/// neither is repeated here — nor is the closing hint, which the wizard emits
/// itself once, after step 9, whatever that step decided.
///
/// `Ok(true)` means the service carries a facelock line now. The wizard needs
/// that answer and cannot get it from `Ok(())`: it names the configured
/// services in its closing summary and offers the hyprlock handoff off the
/// same list, and both are claims about the file rather than about the call
/// having not failed. `absent` and `declined` are successes that configured
/// nothing.
///
/// The wizard's request names exactly one service, so the fold below reads it
/// off a single row; `any` rather than `all` so a future empty request answers
/// "nothing was configured" instead of "everything was".
pub(crate) fn install_one_in(dirs: &PamDirs, request: &PamRequest) -> anyhow::Result<bool> {
    debug_assert_eq!(request.action, PamAction::Add);
    let write = WriteRequest {
        action: WriteAction::Add,
        request,
        remedy: "--allow-sensitive",
    };
    let sink = Sink::human();
    let reports = apply_all(dirs, &plan_writes(dirs, &write)?, &write, &sink);
    first_failure(&reports)?;
    Ok(reports
        .iter()
        .any(|report| add_left_the_line(&report.outcome)))
}

/// Whether `add` left the service carrying a facelock line.
///
/// `overridden` is `true`: the `/etc/pam.d` copy that now exists carries it.
/// `absent` and `declined` are `false` — neither is a failure, and neither
/// configured anything. Every other word belongs to `remove` or `status`, or
/// is `failed`, which [`first_failure`] has already turned into an `Err`
/// before this is consulted.
fn add_left_the_line(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Installed | Outcome::Overridden | Outcome::Unchanged => true,
        Outcome::Absent
        | Outcome::Declined
        | Outcome::Removed
        | Outcome::VendorOnly
        | Outcome::Failed(_)
        | Outcome::CleanupFailed(_)
        | Outcome::Present
        | Outcome::Missing
        | Outcome::Unknown(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // -----------------------------------------------------------------------
    // Goldens captured from `main` (4c8cf28) before the extraction.
    //
    // Produced by running that commit's `pam_install_in` / `pam_remove_in`
    // against a tempdir and dumping the resulting bytes — not written by hand.
    // They are the regression guard the whole refactor turns on. Issue #192
    // intentionally changes the no-auth placement to follow the magic header;
    // every other byte remains pinned to the pre-refactor output.
    // -----------------------------------------------------------------------

    /// A service whose first `auth` line is not its first line.
    const SUDO_BEFORE: &str = "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\nsession\t\tinclude\t\tsystem-auth\n";
    const SUDO_AFTER: &str = "#%PAM-1.0\nauth      sufficient pam_facelock.so\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\nsession\t\tinclude\t\tsystem-auth\n";

    /// A service with no `auth` line at all: the magic header stays first.
    const POLKIT_BEFORE: &str =
        "#%PAM-1.0\naccount\t\tinclude\t\tsystem-auth\npassword\tinclude\t\tsystem-auth\n";
    const POLKIT_AFTER: &str = "#%PAM-1.0\nauth      sufficient pam_facelock.so\naccount\t\tinclude\t\tsystem-auth\npassword\tinclude\t\tsystem-auth\n";

    /// A service that already carries the line: untouched, and no backup.
    const OMARCHY_PRESENT: &str = "#%PAM-1.0\nauth      sufficient pam_facelock.so\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\n";
    const OMARCHY_REMOVED: &str =
        "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\n";

    /// A file with no trailing newline keeps not having one.
    const NO_NEWLINE_BEFORE: &str = "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth";
    const NO_NEWLINE_AFTER: &str =
        "#%PAM-1.0\nauth      sufficient pam_facelock.so\nauth\t\tinclude\t\tsystem-auth";

    #[test]
    fn pam_line_placement_contract_is_frozen() {
        const CANONICAL_LINE: &[u8; 36] = b"auth      sufficient pam_facelock.so";
        const OMARCHY_SKELETON: &[u8] = b"#%PAM-1.0\n\
auth       required                    pam_deny.so\n\
account    include                     system-local-login\n";
        const OMARCHY_CONFIGURED: &[u8] = b"#%PAM-1.0\n\
auth      sufficient pam_facelock.so\n\
auth       required                    pam_deny.so\n\
account    include                     system-local-login\n";
        const HEADERLESS_NO_AUTH: &[u8] = b"account required pam_unix.so\n";
        const HEADERLESS_CONFIGURED: &[u8] = b"auth      sufficient pam_facelock.so\n\
account required pam_unix.so\n";

        assert_eq!(PAM_LINE.as_bytes(), CANONICAL_LINE);
        assert_eq!(PAM_LINE.len(), 36);
        assert!(!PAM_LINE.contains('\n'));
        assert_eq!(with_line_inserted(OMARCHY_SKELETON), OMARCHY_CONFIGURED);
        assert_eq!(
            with_line_inserted(HEADERLESS_NO_AUTH),
            HEADERLESS_CONFIGURED
        );
    }

    #[test]
    fn backup_prepare_and_commit_are_versioned_root_only_provenance() {
        let root = tempfile::tempdir().unwrap();
        let backup_dir = root.path().join("pam-backups");
        let store = BackupStore::open(&backup_dir).unwrap();

        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();

        assert_eq!(
            fs::read(prepared.backup_path()).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        let backup_meta = fs::symlink_metadata(prepared.backup_path()).unwrap();
        assert!(backup_meta.file_type().is_file());
        assert_eq!(backup_meta.permissions().mode() & 0o7777, 0o600);
        assert_eq!(backup_meta.nlink(), 1);

        let record_bytes = fs::read(prepared.record_path()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&record_bytes).unwrap();
        assert_eq!(value["version"], PROVENANCE_VERSION);
        assert_eq!(value["sequence"], 1);
        assert_eq!(value["state"], "prepared");
        assert_eq!(value["service"], "sudo");
        assert_eq!(value["backup"], prepared.backup_name());
        assert_eq!(value["original_sha256"], sha256_hex(SUDO_BEFORE.as_bytes()));
        assert_eq!(value["installed_sha256"], sha256_hex(SUDO_AFTER.as_bytes()));
        assert!(value.get("path").is_none());
        assert!(value.get("target").is_none());
        assert_eq!(
            Path::new(value["backup"].as_str().unwrap()).file_name(),
            Some(OsStr::new(value["backup"].as_str().unwrap()))
        );

        store.commit(&prepared).unwrap();
        let committed: serde_json::Value =
            serde_json::from_slice(&fs::read(prepared.record_path()).unwrap()).unwrap();
        assert_eq!(committed["state"], "committed");
        assert_eq!(committed["backup"], prepared.backup_name());

        if unsafe { libc::geteuid() } == 0 {
            assert_eq!((backup_meta.uid(), backup_meta.gid()), (0, 0));
        }
    }

    #[test]
    fn opening_an_existing_store_retightens_its_directory_before_trust() {
        let root = tempfile::tempdir().unwrap();
        let backup_dir = root.path().join("pam-backups");
        let _store = BackupStore::open(&backup_dir).unwrap();
        fs::set_permissions(&backup_dir, fs::Permissions::from_mode(0o777)).unwrap();

        let reopened = BackupStore::open_existing(&backup_dir).unwrap();

        assert!(reopened.is_some());
        assert_eq!(
            fs::metadata(&backup_dir).unwrap().permissions().mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn new_state_directory_is_never_group_or_world_accessible() {
        let root = tempfile::tempdir().unwrap();
        let backup_dir = root.path().join("pam-backups");

        create_state_directory(&backup_dir).unwrap();

        let mode = fs::symlink_metadata(&backup_dir)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn state_directory_trust_does_not_follow_a_chowned_directory() {
        assert!(state_directory_attributes_match(
            libc::S_IFDIR | 0o700,
            0,
            0,
            (0, 0)
        ));
        assert!(!state_directory_attributes_match(
            libc::S_IFDIR | 0o700,
            1000,
            1000,
            (0, 0)
        ));
        assert!(!state_directory_attributes_match(
            libc::S_IFDIR | 0o770,
            0,
            0,
            (0, 0)
        ));
    }

    #[test]
    fn state_entry_trust_uses_the_expected_owner_not_the_directory_as_authority() {
        let identity = FileIdentity {
            device: 1,
            inode: 2,
            links: 1,
            sha256: sha256_hex(b"state bytes"),
            mode: libc::S_IFREG | 0o600,
            uid: 1000,
            gid: 1000,
        };

        assert!(state_identity_matches(
            (1000, 1000),
            &identity,
            &identity.sha256
        ));
        assert!(!state_identity_matches((0, 0), &identity, &identity.sha256));
    }

    #[test]
    fn a_timestamp_collision_allocates_a_new_exact_name() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let timestamp = std::time::Duration::new(1_700_000_000, 42);
        let collided = "sudo.1700000000-000000042";
        let collided_record = format!("{collided}.json");
        fs::write(root.path().join(collided), b"administrator backup\n").unwrap();
        fs::write(
            root.path().join(&collided_record),
            b"administrator record\n",
        )
        .unwrap();

        let prepared = store
            .plan_at(
                "sudo",
                SUDO_BEFORE.as_bytes(),
                SUDO_AFTER.as_bytes(),
                timestamp,
            )
            .unwrap();
        store.persist(&prepared, SUDO_BEFORE.as_bytes()).unwrap();

        assert_eq!(
            fs::read(root.path().join(collided)).unwrap(),
            b"administrator backup\n"
        );
        assert_eq!(
            fs::read(root.path().join(collided_record)).unwrap(),
            b"administrator record\n"
        );
        assert_ne!(prepared.backup_name(), collided);
        assert!(valid_backup_name("sudo", prepared.backup_name()));
    }

    #[test]
    fn latest_committed_uses_sequence_across_clock_rollback() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let first = store
            .plan_at(
                "sudo",
                b"first original\n",
                b"first installed\n",
                std::time::Duration::new(1_900_000_000, 1),
            )
            .unwrap();
        store.persist(&first, b"first original\n").unwrap();
        store.commit(&first).unwrap();
        let second = store
            .plan_at(
                "sudo",
                b"second original\n",
                b"second installed\n",
                std::time::Duration::new(1_700_000_000, 1),
            )
            .unwrap();
        store.persist(&second, b"second original\n").unwrap();
        store.commit(&second).unwrap();

        assert_eq!(first.provenance.sequence, 1);
        assert_eq!(second.provenance.sequence, 2);
        assert_eq!(
            store.latest_committed("sudo").unwrap(),
            Some(second.backup_path())
        );
    }

    #[test]
    fn duplicate_sequences_are_ambiguous_and_sequence_overflow_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let first = store
            .prepare("sudo", b"first\n", b"first installed\n")
            .unwrap();
        store.commit(&first).unwrap();
        let second = store
            .prepare("sudo", b"second\n", b"second installed\n")
            .unwrap();
        store.commit(&second).unwrap();

        let mut first_record: ProvenanceRecord =
            serde_json::from_slice(&fs::read(first.record_path()).unwrap()).unwrap();
        first_record.sequence = second.provenance.sequence;
        fs::write(
            first.record_path(),
            serde_json::to_vec_pretty(&first_record).unwrap(),
        )
        .unwrap();
        assert_eq!(store.latest_committed("sudo").unwrap(), None);

        first_record.sequence = u64::MAX;
        fs::write(
            first.record_path(),
            serde_json::to_vec_pretty(&first_record).unwrap(),
        )
        .unwrap();
        let error = store
            .plan_at(
                "sudo",
                b"third\n",
                b"third installed\n",
                std::time::Duration::new(1_600_000_000, 0),
            )
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn malformed_hashes_never_participate_in_sequence_order_or_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let valid = store
            .prepare("sudo", b"valid original\n", b"valid installed\n")
            .unwrap();
        store.commit(&valid).unwrap();

        let poisoned_backup = "login.1700000000-000000001";
        let poisoned_bytes = b"administrator backup\n";
        fs::write(root.path().join(poisoned_backup), poisoned_bytes).unwrap();
        fs::write(
            root.path().join(format!("{poisoned_backup}.json")),
            serde_json::to_vec_pretty(&ProvenanceRecord {
                version: PROVENANCE_VERSION,
                sequence: u64::MAX,
                state: ProvenanceState::Committed,
                service: "login".into(),
                backup: poisoned_backup.into(),
                original_sha256: sha256_hex(poisoned_bytes),
                installed_sha256: "not-a-sha256".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let duplicate_backup = "login.1700000000-000000002";
        fs::write(root.path().join(duplicate_backup), poisoned_bytes).unwrap();
        fs::write(
            root.path().join(format!("{duplicate_backup}.json")),
            serde_json::to_vec_pretty(&ProvenanceRecord {
                version: PROVENANCE_VERSION,
                sequence: valid.provenance.sequence,
                state: ProvenanceState::Committed,
                service: "login".into(),
                backup: duplicate_backup.into(),
                original_sha256: "not-a-sha256".into(),
                installed_sha256: sha256_hex(b"installed\n"),
            })
            .unwrap(),
        )
        .unwrap();
        let ownerless_record = "login.1700000000-000000003";
        fs::write(
            root.path().join(format!("{ownerless_record}.json")),
            serde_json::to_vec_pretty(&ProvenanceRecord {
                version: PROVENANCE_VERSION,
                sequence: u64::MAX,
                state: ProvenanceState::Committed,
                service: "login".into(),
                backup: ownerless_record.into(),
                original_sha256: sha256_hex(b"missing backup\n"),
                installed_sha256: sha256_hex(b"installed\n"),
            })
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            root.path().join(format!("{ownerless_record}.json")),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        assert_eq!(
            store.latest_committed("sudo").unwrap(),
            Some(valid.backup_path()),
            "a malformed record cannot make a valid sequence ambiguous"
        );
        let next = store
            .plan_at(
                "sudo",
                b"next original\n",
                b"next installed\n",
                std::time::Duration::new(1_600_000_000, 0),
            )
            .unwrap();
        assert_eq!(next.provenance.sequence, 2);

        store.cleanup("login").unwrap();
        assert_eq!(
            fs::read(root.path().join(poisoned_backup)).unwrap(),
            poisoned_bytes,
            "malformed provenance never owns a backup"
        );
        assert!(root.path().join(format!("{poisoned_backup}.json")).exists());
        assert_eq!(
            fs::read(root.path().join(duplicate_backup)).unwrap(),
            poisoned_bytes
        );
        assert!(
            root.path()
                .join(format!("{duplicate_backup}.json"))
                .exists()
        );
        assert!(
            root.path()
                .join(format!("{ownerless_record}.json"))
                .exists()
        );
    }

    #[test]
    fn add_transaction_blocks_recovery_until_the_backup_is_committed() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let path = dir.path().join("sudo");
        let (original, expected) = read_regular_nofollow(&path).unwrap();
        let installed = with_line_inserted(&original);
        let prepared = transaction
            .plan_at(
                "sudo",
                &original,
                &installed,
                std::time::Duration::new(1_700_000_000, 1),
            )
            .unwrap();
        transaction.persist(&prepared, &original).unwrap();

        let competing_store = store.clone();
        let competing_dirs = dirs.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let recoverer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = competing_store.recover(&competing_dirs);
            done_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "recovery must not enter between backup persistence and PAM publication"
        );

        transaction
            .replace_pam_with_intent(&prepared, &path, &expected, &installed)
            .unwrap();
        transaction.commit(&prepared).unwrap();
        drop(transaction);

        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        recoverer.join().unwrap();
        assert_eq!(fs::read(&path).unwrap(), installed);
        assert!(prepared.backup_path().exists());
        assert_eq!(
            store.latest_committed("sudo").unwrap(),
            Some(prepared.backup_path())
        );
    }

    #[test]
    fn recovery_resolves_every_local_remove_exchange_boundary() {
        for crash_at in [
            PamRemoveCrashPoint::Intent,
            PamRemoveCrashPoint::ReplacementTemp,
            PamRemoveCrashPoint::Exchange,
            PamRemoveCrashPoint::Finalize,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let path = dir.path().join("sudo");
            let (original, expected) = read_regular_nofollow(&path).unwrap();
            let installed = with_line_removed(&original);
            let mutation = transaction
                .plan_mutation("sudo", &original, &installed)
                .unwrap();

            let error = transaction
                .remove_pam_with_intent_hook(&mutation, &path, &expected, &installed, |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            drop(transaction);

            store.recover(&dirs).unwrap();

            assert_eq!(
                fs::read(&path).unwrap(),
                if matches!(
                    crash_at,
                    PamRemoveCrashPoint::Exchange | PamRemoveCrashPoint::Finalize
                ) {
                    installed.as_slice()
                } else {
                    original.as_slice()
                }
            );
            assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
            assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".facelock-pam-remove-")
            }));
        }
    }

    #[test]
    fn local_remove_recovery_preserves_chmod_mutated_original_or_replacement() {
        for mutate_replacement in [false, true] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let path = dir.path().join("sudo");
            let (original, expected) = read_regular_nofollow(&path).unwrap();
            let installed = with_line_removed(&original);
            let mutation = transaction
                .plan_mutation("sudo", &original, &installed)
                .unwrap();
            let error = transaction
                .remove_pam_with_intent_hook(&mutation, &path, &expected, &installed, |point| {
                    if point == PamRemoveCrashPoint::ReplacementTemp {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            drop(transaction);

            let temp = dir.path().join(pam_remove_name(&mutation.operation));
            let changed = if mutate_replacement { &temp } else { &path };
            let mode = fs::metadata(changed).unwrap().permissions().mode() & 0o7777;
            fs::set_permissions(changed, fs::Permissions::from_mode(mode ^ 0o040)).unwrap();
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::PamRemove, &mutation.operation));

            store.recover(&dirs).unwrap();

            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(fs::read(&temp).unwrap(), installed);
            assert!(intent.exists());
        }
    }

    #[test]
    fn recovery_resolves_every_vendor_create_boundary() {
        for crash_at in [
            VendorCreateCrashPoint::Intent,
            VendorCreateCrashPoint::ReplacementTemp,
            VendorCreateCrashPoint::Publish,
            VendorCreateCrashPoint::Finalize,
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            let request = add(&["polkit-1"]);
            let write = WriteRequest {
                action: WriteAction::Add,
                request: &request,
                remedy: "--allow-sensitive",
            };
            let target = plan_writes(&dirs, &write).unwrap().remove(0);
            let expected = target.identity.as_ref().unwrap();
            let installed = POLKIT_AFTER.as_bytes();
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let mutation = transaction
                .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
                .unwrap();

            let error = transaction
                .create_vendor_with_intent_hook(&mutation, &target, expected, installed, |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            drop(transaction);

            store.recover(&dirs).unwrap();

            if matches!(
                crash_at,
                VendorCreateCrashPoint::Publish | VendorCreateCrashPoint::Finalize
            ) {
                assert_eq!(fs::read(etc.join("polkit-1")).unwrap(), installed);
            } else {
                assert!(!etc.join("polkit-1").exists());
            }
            assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
            assert!(fs::read_dir(&etc).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".facelock-vendor-create-")
            }));
        }
    }

    #[test]
    fn vendor_create_recovery_preserves_a_chmod_mutated_created_entry() {
        for crash_at in [
            VendorCreateCrashPoint::ReplacementTemp,
            VendorCreateCrashPoint::Publish,
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            let request = add(&["polkit-1"]);
            let write = WriteRequest {
                action: WriteAction::Add,
                request: &request,
                remedy: "--allow-sensitive",
            };
            let target = plan_writes(&dirs, &write).unwrap().remove(0);
            let expected = target.identity.as_ref().unwrap();
            let installed = POLKIT_AFTER.as_bytes();
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let mutation = transaction
                .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
                .unwrap();
            let error = transaction
                .create_vendor_with_intent_hook(&mutation, &target, expected, installed, |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            drop(transaction);

            let created = if crash_at == VendorCreateCrashPoint::Publish {
                etc.join("polkit-1")
            } else {
                etc.join(vendor_create_name(&mutation.operation))
            };
            let mode = fs::metadata(&created).unwrap().permissions().mode() & 0o7777;
            fs::set_permissions(&created, fs::Permissions::from_mode(mode ^ 0o040)).unwrap();
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::VendorCreate, &mutation.operation));

            store.recover(&dirs).unwrap();

            assert_eq!(fs::read(&created).unwrap(), installed);
            assert!(intent.exists());
        }
    }

    #[test]
    fn vendor_create_final_check_preserves_post_publication_substitutions() {
        for mutation in [
            PostPublicationMutation::ExactBytesNewInode,
            PostPublicationMutation::Chmod,
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            let request = add(&["polkit-1"]);
            let write = WriteRequest {
                action: WriteAction::Add,
                request: &request,
                remedy: "--allow-sensitive",
            };
            let target = plan_writes(&dirs, &write).unwrap().remove(0);
            let expected = target.identity.as_ref().unwrap();
            let installed = POLKIT_AFTER.as_bytes();
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let mutation_plan = transaction
                .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
                .unwrap();
            let canonical = etc.join("polkit-1");
            let holding = etc.join("administrator-published-override");

            let result = transaction.create_vendor_with_intent_hook(
                &mutation_plan,
                &target,
                expected,
                installed,
                |point| {
                    if point == VendorCreateCrashPoint::Publish {
                        mutate_published_file(&canonical, &holding, mutation);
                    }
                    Ok(())
                },
            );

            assert!(result.is_err());
            drop(transaction);
            let intent = dirs.backup_dir().join(intent_name(
                IntentRole::VendorCreate,
                &mutation_plan.operation,
            ));
            store.recover(&dirs).unwrap();
            assert_eq!(fs::read(&canonical).unwrap(), installed);
            assert!(intent.exists(), "ambiguous vendor intent must remain");
        }
    }

    #[test]
    fn vendor_create_rechecks_publication_before_intent_cleanup() {
        for mutation in [
            PostPublicationMutation::ExactBytesNewInode,
            PostPublicationMutation::Chmod,
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            let request = add(&["polkit-1"]);
            let write = WriteRequest {
                action: WriteAction::Add,
                request: &request,
                remedy: "--allow-sensitive",
            };
            let target = plan_writes(&dirs, &write).unwrap().remove(0);
            let expected = target.identity.as_ref().unwrap();
            let installed = POLKIT_AFTER.as_bytes();
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let mutation_plan = transaction
                .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
                .unwrap();
            let canonical = etc.join("polkit-1");
            let holding = etc.join("administrator-late-override");

            let result = transaction.create_vendor_with_intent_hook(
                &mutation_plan,
                &target,
                expected,
                installed,
                |point| {
                    if point == VendorCreateCrashPoint::Finalize {
                        mutate_published_file(&canonical, &holding, mutation);
                    }
                    Ok(())
                },
            );

            assert!(result.is_err());
            drop(transaction);
            let intent = dirs.backup_dir().join(intent_name(
                IntentRole::VendorCreate,
                &mutation_plan.operation,
            ));
            store.recover(&dirs).unwrap();
            assert!(intent.exists());
        }
    }

    #[test]
    fn an_ambiguous_intent_mode_never_owns_a_matching_pam_temp() {
        let dir = seeded(&[]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let operation = "sudo.1700000000-000000001";
        let installed = b"administrator bytes in a reserved-looking name\n";
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::VendorCreate,
            sequence: 1,
            service: "sudo".into(),
            backup: operation.into(),
            original_sha256: sha256_hex(b"vendor bytes\n"),
            installed_sha256: sha256_hex(installed),
            record_sha256: None,
            replacement_record_sha256: None,
            original_device: None,
            original_inode: None,
            original_links: None,
            original_mode: Some(libc::S_IFREG | 0o644),
            original_uid: Some(unsafe { libc::geteuid() }),
            original_gid: Some(unsafe { libc::getegid() }),
        };
        let intent_path = dirs
            .backup_dir()
            .join(intent_name(IntentRole::VendorCreate, operation));
        fs::write(&intent_path, serde_json::to_vec_pretty(&intent).unwrap()).unwrap();
        fs::set_permissions(&intent_path, fs::Permissions::from_mode(0o640)).unwrap();
        let temp_path = dir.path().join(vendor_create_name(operation));
        fs::write(&temp_path, installed).unwrap();

        store.recover(&dirs).unwrap();

        assert!(intent_path.exists());
        assert_eq!(fs::read(temp_path).unwrap(), installed);
    }

    #[test]
    fn oversized_backup_is_rejected_before_state_is_published() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let original = vec![b'x'; MAX_BACKUP_BYTES + 1];
        let prepared = store.plan("sudo", &original, b"installed\n").unwrap();

        let error = store.persist(&prepared, &original).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn bounded_record_read_refuses_oversized_untrusted_json() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        fs::write(&path, vec![b' '; MAX_RECORD_BYTES + 1]).unwrap();
        let file = fs::File::open(path).unwrap();

        let error = read_open_bounded(&file, MAX_RECORD_BYTES).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_publication_collision_never_overwrites_or_unlinks_existing_state() {
        for collide_record in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = BackupStore::open(root.path()).unwrap();
            let prepared = store
                .plan_at(
                    "sudo",
                    SUDO_BEFORE.as_bytes(),
                    SUDO_AFTER.as_bytes(),
                    std::time::Duration::new(1_700_000_000, 73),
                )
                .unwrap();
            let existing = if collide_record {
                prepared.record_path()
            } else {
                prepared.backup_path()
            };
            fs::write(&existing, b"administrator state\n").unwrap();

            assert!(store.persist(&prepared, SUDO_BEFORE.as_bytes()).is_err());

            assert_eq!(fs::read(&existing).unwrap(), b"administrator state\n");
            if collide_record {
                assert!(
                    !prepared.backup_path().exists(),
                    "only the backup created by this failed prepare is cleaned"
                );
            }
        }
    }

    #[test]
    fn a_service_changed_after_planning_is_not_replaced() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let request = add(&["sudo"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();

        let intervening = b"# administrator changed this after planning\n";
        fs::write(dir.path().join("sudo"), intervening).unwrap();
        let reports = apply_all(&dirs, &targets, &write, &Sink::verb(true));

        assert!(matches!(reports[0].outcome, Outcome::Failed(_)));
        assert_eq!(fs::read(dir.path().join("sudo")).unwrap(), intervening);
    }

    #[test]
    fn a_service_changed_while_the_temp_is_written_is_not_replaced() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let request = add(&["sudo"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();
        let target = &targets[0];
        let expected = target.identity.as_ref().unwrap();
        let installed = with_line_inserted(SUDO_BEFORE.as_bytes());
        let intervening = b"# administrator changed this at publication\n";

        let error = replace_existing_verified_with_hook(&target.path, expected, &installed, || {
            fs::write(&target.path, intervening).unwrap()
        })
        .unwrap_err();

        assert!(error.to_string().contains("changed after it was planned"));
        assert_eq!(fs::read(&target.path).unwrap(), intervening);
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".sudo.facelock-")
        }));
    }

    #[test]
    fn a_service_replaced_after_the_final_check_is_exchanged_back() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let request = add(&["sudo"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();
        let target = &targets[0];
        let expected = target.identity.as_ref().unwrap();
        let installed = with_line_inserted(SUDO_BEFORE.as_bytes());
        let displaced = dir.path().join("planned-original");
        let administrator = b"# administrator published at the rename boundary\n";

        let error =
            replace_existing_verified_with_publish_hook(&target.path, expected, &installed, || {
                fs::rename(&target.path, &displaced).unwrap();
                fs::write(&target.path, administrator).unwrap();
            })
            .unwrap_err();

        assert!(error.to_string().contains("changed after it was planned"));
        assert_eq!(fs::read(&target.path).unwrap(), administrator);
        assert_eq!(fs::read(displaced).unwrap(), SUDO_BEFORE.as_bytes());
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".sudo.facelock-")
        }));
    }

    #[test]
    fn add_writes_a_committed_backup_only_in_dedicated_state() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());

        assert_eq!(write_in(&dirs, &add(&["sudo"])).unwrap(), WRITE_OK);

        assert!(!dir.path().join("sudo.facelock-backup").exists());
        let entries = fs::read_dir(dirs.backup_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let backup = entries
            .iter()
            .find(|path| path.extension().and_then(OsStr::to_str) != Some("json"))
            .unwrap();
        let record = entries
            .iter()
            .find(|path| path.extension().and_then(OsStr::to_str) == Some("json"))
            .unwrap();
        assert_eq!(fs::read(backup).unwrap(), SUDO_BEFORE.as_bytes());
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
        assert_eq!(provenance["state"], "committed");
        assert_eq!(
            provenance["backup"],
            backup.file_name().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn remove_cleans_owned_state_and_legacy_backup_unless_kept() {
        for keep_backup in [false, true] {
            let dir = seeded(&[("sudo", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            write_in(&dirs, &add(&["sudo"])).unwrap();
            fs::write(dir.path().join("sudo.facelock-backup"), SUDO_BEFORE).unwrap();
            let before = fs::read_dir(dirs.backup_dir()).unwrap().count();
            assert_eq!(before, 2, "one backup and one provenance record");

            let request = PamRequest {
                keep_backup,
                ..remove(&["sudo"])
            };
            assert_eq!(write_in(&dirs, &request).unwrap(), WRITE_OK);

            assert_eq!(
                fs::read_dir(dirs.backup_dir()).unwrap().count(),
                if keep_backup { before } else { 0 }
            );
            assert_eq!(
                dir.path().join("sudo.facelock-backup").exists(),
                keep_backup
            );
        }
    }

    #[test]
    fn cleanup_failure_reports_the_partial_result_and_remains_fatal() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let sentinel = dir.path().join("administrator-sentinel");
        fs::write(&sentinel, b"administrator state\n").unwrap();
        let legacy = backup_path(&dir.path().join("sudo"));
        std::os::unix::fs::symlink(&sentinel, &legacy).unwrap();
        let request = remove(&["sudo"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();

        let reports = apply_all(&dirs, &targets, &write, &Sink::verb(true));

        assert_eq!(
            fs::read_to_string(dir.path().join("sudo")).unwrap(),
            SUDO_BEFORE
        );
        assert!(legacy.is_symlink());
        assert_eq!(fs::read(&sentinel).unwrap(), b"administrator state\n");
        assert!(matches!(reports[0].outcome, Outcome::CleanupFailed(_)));
        assert!(first_failure(&reports).is_err());
        let json: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Remove, false, &reports, &[])).unwrap();
        assert_eq!(json["services"][0]["action"], "cleanup-failed");
        assert!(
            json["services"][0]["error"]
                .as_str()
                .unwrap()
                .contains("failed to clean backups")
        );
    }

    #[test]
    fn cleanup_preserves_an_unresolved_prepared_pair() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let backup_before = fs::read(prepared.backup_path()).unwrap();
        let record_before = fs::read(prepared.record_path()).unwrap();

        store.cleanup("sudo").unwrap();

        assert_eq!(fs::read(prepared.backup_path()).unwrap(), backup_before);
        assert_eq!(fs::read(prepared.record_path()).unwrap(), record_before);
    }

    #[test]
    fn cleanup_preserves_a_pair_substituted_after_validation() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        store.commit(&prepared).unwrap();
        let displaced = root.path().join("displaced-facelock-backup");
        let administrator = b"administrator replacement\n";

        let result = store.cleanup_with_hook("sudo", |pair| {
            fs::rename(pair.backup_path(), &displaced).unwrap();
            fs::write(pair.backup_path(), administrator).unwrap();
        });

        assert!(result.is_err());
        assert_eq!(fs::read(prepared.backup_path()).unwrap(), administrator);
        assert!(prepared.record_path().exists());
        assert_eq!(fs::read(displaced).unwrap(), SUDO_BEFORE.as_bytes());
    }

    #[test]
    fn cleanup_preserves_a_same_inode_pair_with_mutated_mode() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        store.commit(&prepared).unwrap();

        let result = store.cleanup_with_hook("sudo", |pair| {
            fs::set_permissions(pair.backup_path(), fs::Permissions::from_mode(0o640)).unwrap();
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read(prepared.backup_path()).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        assert!(prepared.record_path().exists());
    }

    #[test]
    fn cleanup_preserves_symlink_and_hardlink_substitutions() {
        for hard_link in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = BackupStore::open(root.path()).unwrap();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
                .unwrap();
            store.commit(&prepared).unwrap();
            let displaced = root.path().join("displaced-owned-backup");

            let result = store.cleanup_with_hook("sudo", |pair| {
                fs::rename(pair.backup_path(), &displaced).unwrap();
                if hard_link {
                    fs::hard_link(&displaced, pair.backup_path()).unwrap();
                } else {
                    std::os::unix::fs::symlink(&displaced, pair.backup_path()).unwrap();
                }
            });

            assert!(result.is_err());
            assert!(fs::symlink_metadata(prepared.backup_path()).is_ok());
            assert!(prepared.record_path().exists());
            assert_eq!(fs::read(displaced).unwrap(), SUDO_BEFORE.as_bytes());
        }
    }

    #[test]
    fn legacy_cleanup_preserves_a_substitution_after_validation() {
        let dir = seeded(&[("sudo", SUDO_AFTER), ("sudo.facelock-backup", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let legacy = dir.path().join("sudo.facelock-backup");
        let displaced = dir.path().join("displaced-legacy-backup");
        let administrator = b"administrator legacy backup\n";

        let result = remove_legacy_backup_with_hook(&dirs, "sudo", || {
            fs::rename(&legacy, &displaced).unwrap();
            fs::write(&legacy, administrator).unwrap();
        });

        assert!(result.is_err());
        assert_eq!(fs::read(&legacy).unwrap(), administrator);
        assert_eq!(fs::read(displaced).unwrap(), SUDO_BEFORE.as_bytes());
    }

    #[test]
    fn legacy_cleanup_preserves_symlink_and_hardlink_substitutions() {
        for hard_link in [false, true] {
            let dir = seeded(&[("sudo", SUDO_AFTER), ("sudo.facelock-backup", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            let legacy = dir.path().join("sudo.facelock-backup");
            let displaced = dir.path().join("displaced-legacy-backup");

            let result = remove_legacy_backup_with_hook(&dirs, "sudo", || {
                fs::rename(&legacy, &displaced).unwrap();
                if hard_link {
                    fs::hard_link(&displaced, &legacy).unwrap();
                } else {
                    std::os::unix::fs::symlink(&displaced, &legacy).unwrap();
                }
            });

            assert!(result.is_err());
            assert!(fs::symlink_metadata(&legacy).is_ok());
            assert_eq!(fs::read(displaced).unwrap(), SUDO_BEFORE.as_bytes());
        }
    }

    #[test]
    fn recovery_removes_only_exact_hash_bound_owned_state_temps() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let owned = b"facelock temporary bytes\n";
        let destination = "sudo.1700000000-000000001";
        let owned_name = format!(".facelock-tmp-{destination}-{}-12-34", sha256_hex(owned));
        let ambiguous_name = format!(
            ".facelock-tmp-{destination}-{}-56-78",
            sha256_hex(b"other\n")
        );
        fs::write(dirs.backup_dir().join(&owned_name), owned).unwrap();
        fs::write(
            dirs.backup_dir().join(&ambiguous_name),
            b"administrator bytes\n",
        )
        .unwrap();
        fs::set_permissions(
            dirs.backup_dir().join(&owned_name),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::set_permissions(
            dirs.backup_dir().join(&ambiguous_name),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        store.recover(&dirs).unwrap();

        assert!(!dirs.backup_dir().join(owned_name).exists());
        assert_eq!(
            fs::read(dirs.backup_dir().join(ambiguous_name)).unwrap(),
            b"administrator bytes\n"
        );
    }

    #[test]
    fn backup_service_rejects_unconfined_service_components() {
        for service in ["", ".", ".."] {
            let name = format!("{service}.1700000000-000000001");
            assert_eq!(
                backup_service(&name),
                None,
                "{service:?} is not a confined PAM service"
            );
        }
    }

    #[test]
    fn recovery_preserves_owned_shape_temps_with_unconfined_services() {
        for service in ["", ".", ".."] {
            let dir = seeded(&[("sudo", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let content = b"administrator state-shaped bytes\n";
            let destination = format!("{service}.1700000000-000000001");
            let name = format!(".facelock-tmp-{destination}-{}-12-34", sha256_hex(content));
            let path = dirs.backup_dir().join(&name);
            fs::write(&path, content).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

            store.recover(&dirs).unwrap();

            assert_eq!(
                fs::read(&path).unwrap(),
                content,
                "recovery must not adopt {service:?} as a PAM service"
            );
        }
    }

    #[test]
    fn untrusted_provenance_cannot_name_a_cleanup_target() {
        let root = tempfile::tempdir().unwrap();
        let pam = root.path().join("pam");
        fs::create_dir(&pam).unwrap();
        fs::write(pam.join("sudo"), SUDO_AFTER).unwrap();
        let dirs = only(&pam);
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let victim = root.path().join("administrator-backup");
        fs::write(&victim, b"do not delete\n").unwrap();
        let record = dirs.backup_dir().join("sudo.1700000000-000000001.json");
        fs::write(
            &record,
            br#"{"version":1,"sequence":1,"state":"committed","service":"sudo","backup":"../../administrator-backup","original_sha256":"00","installed_sha256":"00"}"#,
        )
        .unwrap();
        drop(store);

        assert_eq!(write_in(&dirs, &remove(&["sudo"])).unwrap(), WRITE_OK);
        assert_eq!(fs::read(&victim).unwrap(), b"do not delete\n");
        assert!(
            record.exists(),
            "invalid provenance is preserved for inspection"
        );
    }

    #[test]
    fn a_valid_hash_cannot_make_an_admin_name_facelock_owned() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let backup_name = "sudo.admin-note";
        let record_name = "sudo.admin-note.json";
        let content = b"administrator rollback note\n";
        fs::write(root.path().join(backup_name), content).unwrap();
        fs::write(
            root.path().join(record_name),
            serde_json::to_vec(&ProvenanceRecord {
                version: PROVENANCE_VERSION,
                sequence: 1,
                state: ProvenanceState::Committed,
                service: "sudo".into(),
                backup: backup_name.into(),
                original_sha256: sha256_hex(content),
                installed_sha256: sha256_hex(b"installed\n"),
            })
            .unwrap(),
        )
        .unwrap();

        store.cleanup("sudo").unwrap();

        assert!(root.path().join(backup_name).exists());
        assert!(root.path().join(record_name).exists());
    }

    #[test]
    fn recovery_commits_installed_and_discards_unapplied_prepares() {
        for (current, expected_state_files, expected_state) in [
            (SUDO_AFTER, 2, Some("committed")),
            (SUDO_BEFORE, 0, None),
            ("# unrelated administrator bytes\n", 2, Some("prepared")),
        ] {
            let dir = seeded(&[("sudo", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
                .unwrap();
            fs::write(dir.path().join("sudo"), current).unwrap();

            store.recover(&dirs).unwrap();

            assert_eq!(
                fs::read_dir(dirs.backup_dir()).unwrap().count(),
                expected_state_files
            );
            if let Some(expected_state) = expected_state {
                let record: serde_json::Value =
                    serde_json::from_slice(&fs::read(prepared.record_path()).unwrap()).unwrap();
                assert_eq!(record["state"], expected_state);
            }
            assert_eq!(
                fs::read(dir.path().join("sudo")).unwrap(),
                current.as_bytes()
            );
        }
    }

    #[test]
    fn backup_publication_parent_sync_failure_retains_prepare_evidence() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .plan("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let failed_name = prepared.backup.clone();
        install_state_publication_sync_test_hook(move |name| {
            if name == failed_name {
                return Err(std::io::Error::other(
                    "injected backup publication parent sync failure",
                ));
            }
            Ok(())
        });

        let error = store
            .persist(&prepared, SUDO_BEFORE.as_bytes())
            .unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(prepared.backup_path().exists());
        assert!(!prepared.record_path().exists());
        assert!(
            dirs.backup_dir()
                .join(intent_name(IntentRole::Prepare, prepared.backup_name()))
                .exists()
        );

        store.recover(&dirs).unwrap();
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn record_publication_parent_sync_failure_retains_prepare_evidence() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .plan("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let failed_name = prepared.record.clone();
        install_state_publication_sync_test_hook(move |name| {
            if name == failed_name {
                return Err(std::io::Error::other(
                    "injected record publication parent sync failure",
                ));
            }
            Ok(())
        });

        let error = store
            .persist(&prepared, SUDO_BEFORE.as_bytes())
            .unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(prepared.backup_path().exists());
        assert!(prepared.record_path().exists());
        assert!(
            dirs.backup_dir()
                .join(intent_name(IntentRole::Prepare, prepared.backup_name()))
                .exists()
        );

        store.recover(&dirs).unwrap();
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn commit_replacement_parent_sync_failure_retains_intent() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let exchange = format!("{}.json", quarantine_name("commit", prepared.backup_name()));
        let failed_name = exchange.clone();
        install_state_publication_sync_test_hook(move |name| {
            if name == failed_name {
                return Err(std::io::Error::other(
                    "injected commit replacement parent sync failure",
                ));
            }
            Ok(())
        });

        let error = store.commit(&prepared).unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(prepared.backup_path().exists());
        assert!(prepared.record_path().exists());
        assert!(dirs.backup_dir().join(&exchange).exists());
        assert!(
            dirs.backup_dir()
                .join(intent_name(IntentRole::Commit, prepared.backup_name()))
                .exists()
        );
        assert!(
            !dirs
                .backup_dir()
                .join(publication_name(
                    PublicationRole::Commit,
                    prepared.backup_name()
                ))
                .exists()
        );
    }

    #[test]
    fn commit_binding_parent_sync_failure_retains_complete_evidence() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::Commit, prepared.backup_name()));
        let exchange = dirs.backup_dir().join(format!(
            "{}.json",
            quarantine_name("commit", prepared.backup_name())
        ));
        let binding_name = publication_name(PublicationRole::Commit, prepared.backup_name());
        let binding = dirs.backup_dir().join(&binding_name);
        install_state_publication_sync_test_hook(move |name| {
            if name == binding_name {
                return Err(std::io::Error::other(
                    "injected commit binding parent sync failure",
                ));
            }
            Ok(())
        });

        let error = store.commit(&prepared).unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(intent.exists());
        assert!(exchange.exists());
        assert!(binding.exists());

        store.recover(&dirs).unwrap();
        assert!(!intent.exists());
        assert!(!exchange.exists());
        assert!(!binding.exists());
        let record: ProvenanceRecord =
            serde_json::from_slice(&fs::read(prepared.record_path()).unwrap()).unwrap();
        assert_eq!(record.state, ProvenanceState::Committed);
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 2);
    }

    #[test]
    fn pam_replace_binding_parent_sync_failure_retains_complete_evidence() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let path = dir.path().join("sudo");
        let (_, expected) = read_regular_nofollow(&path).unwrap();
        let temp = dir.path().join(pam_replace_name(prepared.backup_name()));
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::PamReplace, prepared.backup_name()));
        let binding_name = publication_name(PublicationRole::PamReplace, prepared.backup_name());
        let binding = dirs.backup_dir().join(&binding_name);
        install_state_publication_sync_test_hook(move |name| {
            if name == binding_name {
                return Err(std::io::Error::other(
                    "injected PAM replacement binding parent sync failure",
                ));
            }
            Ok(())
        });

        let error = store
            .replace_pam_with_intent_hook(
                &prepared,
                &path,
                &expected,
                SUDO_AFTER.as_bytes(),
                |_| Ok(()),
            )
            .unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(intent.exists());
        assert!(binding.exists());
        assert!(temp.exists());
        assert_eq!(fs::read(&path).unwrap(), SUDO_BEFORE.as_bytes());

        store.recover(&dirs).unwrap();
        assert!(!intent.exists());
        assert!(!binding.exists());
        assert!(!temp.exists());
        assert_eq!(fs::read(&path).unwrap(), SUDO_BEFORE.as_bytes());
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn pam_remove_binding_parent_sync_failure_retains_complete_evidence() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let path = dir.path().join("sudo");
        let (original, expected) = read_regular_nofollow(&path).unwrap();
        let installed = with_line_removed(&original);
        let mutation = transaction
            .plan_mutation("sudo", &original, &installed)
            .unwrap();
        let temp = dir.path().join(pam_remove_name(&mutation.operation));
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::PamRemove, &mutation.operation));
        let binding_name = publication_name(PublicationRole::PamRemove, &mutation.operation);
        let binding = dirs.backup_dir().join(&binding_name);
        install_state_publication_sync_test_hook(move |name| {
            if name == binding_name {
                return Err(std::io::Error::other(
                    "injected PAM removal binding parent sync failure",
                ));
            }
            Ok(())
        });

        let error = transaction
            .remove_pam_with_intent_hook(&mutation, &path, &expected, &installed, |_| Ok(()))
            .unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(intent.exists());
        assert!(binding.exists());
        assert!(temp.exists());
        assert_eq!(fs::read(&path).unwrap(), SUDO_AFTER.as_bytes());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert!(!intent.exists());
        assert!(!binding.exists());
        assert!(!temp.exists());
        assert_eq!(fs::read(&path).unwrap(), SUDO_AFTER.as_bytes());
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn vendor_binding_parent_sync_failure_retains_complete_evidence() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let expected = target.identity.as_ref().unwrap();
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let mutation = transaction
            .plan_mutation(
                "polkit-1",
                POLKIT_BEFORE.as_bytes(),
                POLKIT_AFTER.as_bytes(),
            )
            .unwrap();
        let temp = etc.join(vendor_create_name(&mutation.operation));
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::VendorCreate, &mutation.operation));
        let binding_name = publication_name(PublicationRole::VendorCreate, &mutation.operation);
        let binding = dirs.backup_dir().join(&binding_name);
        install_state_publication_sync_test_hook(move |name| {
            if name == binding_name {
                return Err(std::io::Error::other(
                    "injected vendor binding parent sync failure",
                ));
            }
            Ok(())
        });

        let error = transaction
            .create_vendor_with_intent_hook(
                &mutation,
                &target,
                expected,
                POLKIT_AFTER.as_bytes(),
                |_| Ok(()),
            )
            .unwrap_err();
        clear_state_publication_sync_test_hook();

        assert!(is_ambiguous_publication(&error));
        assert!(intent.exists());
        assert!(binding.exists());
        assert!(temp.exists());
        assert!(!etc.join("polkit-1").exists());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert!(!intent.exists());
        assert!(!binding.exists());
        assert!(!temp.exists());
        assert!(!etc.join("polkit-1").exists());
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn recovery_removes_intent_owned_backup_left_before_record_publication() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .plan("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let error = store
            .persist_with_hook(&prepared, SUDO_BEFORE.as_bytes(), |point| {
                if point == PrepareCrashPoint::Backup {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "simulated crash",
                    ));
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(prepared.backup_path().exists());

        store.recover(&dirs).unwrap();

        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
        assert_eq!(
            fs::read(dir.path().join("sudo")).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
    }

    #[test]
    fn recovery_preserves_a_chmod_mutated_state_entry_and_its_intent() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .plan("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let error = store
            .persist_with_hook(&prepared, SUDO_BEFORE.as_bytes(), |point| {
                if point == PrepareCrashPoint::Backup {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "simulated crash",
                    ));
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        fs::set_permissions(prepared.backup_path(), fs::Permissions::from_mode(0o640)).unwrap();
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::Prepare, prepared.backup_name()));

        store.recover(&dirs).unwrap();

        assert_eq!(
            fs::read(prepared.backup_path()).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        assert!(intent.exists());
    }

    #[test]
    fn identity_comparison_rejects_owner_and_mode_drift() {
        let expected = FileIdentity {
            device: 1,
            inode: 2,
            links: 1,
            sha256: sha256_hex(b"same bytes"),
            mode: libc::S_IFREG | 0o600,
            uid: 0,
            gid: 0,
        };
        let mut changed = expected.clone();
        changed.mode = libc::S_IFREG | 0o640;
        assert!(!identity_matches(&expected, &changed));
        changed = expected.clone();
        changed.uid = 1000;
        assert!(!identity_matches(&expected, &changed));
        changed = expected.clone();
        changed.gid = 1000;
        assert!(!identity_matches(&expected, &changed));
    }

    #[test]
    fn publication_binding_rejects_modeled_owner_and_mode_drift() {
        let identity = FileIdentity {
            device: 1,
            inode: 2,
            links: 1,
            sha256: sha256_hex(b"published bytes"),
            mode: libc::S_IFREG | 0o600,
            uid: 0,
            gid: 0,
        };
        let intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: IntentRole::PamRemove,
            sequence: 1,
            service: "sudo".into(),
            backup: "sudo.1700000000-000000001".into(),
            original_sha256: identity.sha256.clone(),
            installed_sha256: identity.sha256.clone(),
            record_sha256: None,
            replacement_record_sha256: None,
            original_device: Some(identity.device),
            original_inode: Some(identity.inode),
            original_links: Some(identity.links),
            original_mode: Some(identity.mode),
            original_uid: Some(identity.uid),
            original_gid: Some(identity.gid),
        };
        let encoded = serde_json::to_vec_pretty(&intent).unwrap();
        let binding =
            publication_binding_for(PublicationRole::PamRemove, &intent, &encoded, &identity);

        let mut changed = identity.clone();
        changed.uid = 1000;
        assert!(!binding_identity_matches(&binding, &changed));
        changed = identity.clone();
        changed.gid = 1000;
        assert!(!binding_identity_matches(&binding, &changed));
        changed = identity;
        changed.mode ^= 0o040;
        assert!(!binding_identity_matches(&binding, &changed));

        assert!(valid_publication_binding(&binding));
        let mut malformed = binding.clone();
        malformed.sequence = 0;
        assert!(!valid_publication_binding(&malformed));
        malformed = binding.clone();
        malformed.intent_sha256 = "not-a-hash".into();
        assert!(!valid_publication_binding(&malformed));
        malformed = binding.clone();
        malformed.links = 2;
        assert!(!valid_publication_binding(&malformed));
        malformed = binding;
        malformed.mode = libc::S_IFDIR | 0o700;
        assert!(!valid_publication_binding(&malformed));
    }

    #[derive(Debug, Clone, Copy)]
    enum InvalidExactIntent {
        WrongMode,
        UnknownSchemaField,
        MalformedJson,
        MismatchingIntent,
        Symlink,
        HardLink,
    }

    fn invalid_exact_intent_variants() -> Vec<InvalidExactIntent> {
        vec![
            InvalidExactIntent::WrongMode,
            InvalidExactIntent::UnknownSchemaField,
            InvalidExactIntent::MalformedJson,
            InvalidExactIntent::MismatchingIntent,
            InvalidExactIntent::Symlink,
            InvalidExactIntent::HardLink,
        ]
    }

    fn publication_recovery_test_intent(
        role: PublicationRole,
        backup: &str,
        replacement: &FileIdentity,
    ) -> StateIntent {
        let mut intent = StateIntent {
            version: PROVENANCE_VERSION,
            role: role.intent_role(),
            sequence: 1,
            service: "sudo".into(),
            backup: backup.into(),
            original_sha256: replacement.sha256.clone(),
            installed_sha256: replacement.sha256.clone(),
            record_sha256: None,
            replacement_record_sha256: None,
            original_device: None,
            original_inode: None,
            original_links: None,
            original_mode: None,
            original_uid: None,
            original_gid: None,
        };
        match role {
            PublicationRole::Commit => {
                intent.record_sha256 = Some(sha256_hex(b"prepared record"));
                intent.replacement_record_sha256 = Some(replacement.sha256.clone());
            }
            PublicationRole::PamReplace => {
                intent.record_sha256 = Some(sha256_hex(b"prepared record"));
                intent.original_device = Some(replacement.device);
                intent.original_inode = Some(replacement.inode);
                intent.original_links = Some(replacement.links);
                intent.original_mode = Some(replacement.mode);
                intent.original_uid = Some(replacement.uid);
                intent.original_gid = Some(replacement.gid);
            }
            PublicationRole::PamRemove => {
                intent.original_device = Some(replacement.device);
                intent.original_inode = Some(replacement.inode);
                intent.original_links = Some(replacement.links);
                intent.original_mode = Some(replacement.mode);
                intent.original_uid = Some(replacement.uid);
                intent.original_gid = Some(replacement.gid);
            }
            PublicationRole::VendorCreate => {
                intent.original_mode = Some(replacement.mode);
                intent.original_uid = Some(replacement.uid);
                intent.original_gid = Some(replacement.gid);
            }
        }
        assert!(valid_state_intent(&intent));
        intent
    }

    fn write_test_state_entry(path: &Path, content: &[u8]) {
        fs::write(path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn install_invalid_exact_intent(
        root: &Path,
        path: &Path,
        intent: &StateIntent,
        encoded: &[u8],
        variant: InvalidExactIntent,
    ) {
        match variant {
            InvalidExactIntent::WrongMode => {
                write_test_state_entry(path, encoded);
                fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
            }
            InvalidExactIntent::UnknownSchemaField => {
                let mut value = serde_json::to_value(intent).unwrap();
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("administrator".into(), serde_json::Value::Bool(true));
                write_test_state_entry(path, &serde_json::to_vec_pretty(&value).unwrap());
            }
            InvalidExactIntent::MalformedJson => write_test_state_entry(path, b"{"),
            InvalidExactIntent::MismatchingIntent => {
                let mut mismatching = intent.clone();
                mismatching.sequence += 1;
                assert!(valid_state_intent(&mismatching));
                write_test_state_entry(path, &serde_json::to_vec_pretty(&mismatching).unwrap());
            }
            InvalidExactIntent::Symlink => {
                let source = root.join("administrator-symlink-intent");
                write_test_state_entry(&source, encoded);
                std::os::unix::fs::symlink(source, path).unwrap();
            }
            InvalidExactIntent::HardLink => {
                let source = root.join("administrator-hardlink-intent");
                write_test_state_entry(&source, encoded);
                fs::hard_link(source, path).unwrap();
            }
        }
    }

    fn assert_invalid_exact_intent_preserves_binding(role: PublicationRole) {
        for variant in invalid_exact_intent_variants() {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let backup = "sudo.1700000000-000000001";
            let replacement = if role == PublicationRole::Commit {
                atomic_state_create(
                    dirs.backup_dir(),
                    &format!("{backup}.json"),
                    SUDO_AFTER.as_bytes(),
                )
                .unwrap()
            } else {
                read_regular_nofollow(&dir.path().join("sudo")).unwrap().1
            };
            let intent = publication_recovery_test_intent(role, backup, &replacement);
            let encoded = serde_json::to_vec_pretty(&intent).unwrap();
            let publication = store
                .create_publication_binding(role, &intent, &encoded, &replacement)
                .unwrap();
            let intent_path = dirs
                .backup_dir()
                .join(intent_name(role.intent_role(), backup));
            install_invalid_exact_intent(
                dirs.backup_dir(),
                &intent_path,
                &intent,
                &encoded,
                variant,
            );

            store.recover(&dirs).unwrap();

            assert!(
                dirs.backup_dir().join(&publication.name).exists(),
                "{role:?} must preserve its binding for {variant:?}"
            );
        }
    }

    #[test]
    fn exact_intent_presence_is_independent_of_modeled_owner_validation() {
        let root = tempfile::tempdir().unwrap();
        let name = ".facelock-intent-pam-remove-sudo.1700000000-000000001.json";
        let path = root.path().join(name);
        write_test_state_entry(&path, b"administrator intent bytes");
        let (_, identity) = read_regular_nofollow(&path).unwrap();
        let expected_owner = (identity.uid.wrapping_add(1), identity.gid);
        assert!(!state_identity_matches(
            expected_owner,
            &identity,
            &identity.sha256
        ));

        let directory = open_directory_nofollow(root.path()).unwrap();
        assert!(entry_exists_at(&directory, name).unwrap());
    }

    #[test]
    fn commit_binding_is_not_orphaned_by_an_invalid_exact_intent() {
        assert_invalid_exact_intent_preserves_binding(PublicationRole::Commit);
    }

    #[test]
    fn pam_replace_binding_is_not_orphaned_by_an_invalid_exact_intent() {
        assert_invalid_exact_intent_preserves_binding(PublicationRole::PamReplace);
    }

    #[test]
    fn pam_remove_binding_is_not_orphaned_by_an_invalid_exact_intent() {
        assert_invalid_exact_intent_preserves_binding(PublicationRole::PamRemove);
    }

    #[test]
    fn vendor_binding_is_not_orphaned_by_an_invalid_exact_intent() {
        assert_invalid_exact_intent_preserves_binding(PublicationRole::VendorCreate);
    }

    #[test]
    fn pam_replace_binding_collision_cleans_only_its_created_temp() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let path = dir.path().join("sudo");
        let (_, expected) = read_regular_nofollow(&path).unwrap();
        let collision = dirs.backup_dir().join(publication_name(
            PublicationRole::PamReplace,
            prepared.backup_name(),
        ));
        let administrator = b"administrator publication collision\n";
        fs::write(&collision, administrator).unwrap();

        let error = store
            .replace_pam_with_intent_hook(
                &prepared,
                &path,
                &expected,
                SUDO_AFTER.as_bytes(),
                |_| Ok(()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert!(
            !dir.path()
                .join(pam_replace_name(prepared.backup_name()))
                .exists(),
            "a failed binding must not leave its replacement temp unauthenticated"
        );
        assert!(
            !dirs
                .backup_dir()
                .join(intent_name(IntentRole::PamReplace, prepared.backup_name()))
                .exists(),
            "the intent may be removed only after exact temp cleanup succeeds"
        );
        assert_eq!(fs::read(&path).unwrap(), SUDO_BEFORE.as_bytes());

        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert_eq!(fs::read(&path).unwrap(), SUDO_BEFORE.as_bytes());
    }

    #[test]
    fn pam_remove_binding_collision_cleans_only_its_created_temp() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let path = dir.path().join("sudo");
        let (original, expected) = read_regular_nofollow(&path).unwrap();
        let installed = with_line_removed(&original);
        let mutation = transaction
            .plan_mutation("sudo", &original, &installed)
            .unwrap();
        let collision = dirs.backup_dir().join(publication_name(
            PublicationRole::PamRemove,
            &mutation.operation,
        ));
        let administrator = b"administrator publication collision\n";
        fs::write(&collision, administrator).unwrap();

        let error = transaction
            .remove_pam_with_intent(&mutation, &path, &expected, &installed)
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert!(
            !dir.path()
                .join(pam_remove_name(&mutation.operation))
                .exists(),
            "a failed binding must not leave its removal temp unauthenticated"
        );
        assert!(
            !dirs
                .backup_dir()
                .join(intent_name(IntentRole::PamRemove, &mutation.operation))
                .exists(),
            "the intent may be removed only after exact temp cleanup succeeds"
        );
        assert_eq!(fs::read(&path).unwrap(), original);

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn vendor_create_binding_collision_cleans_only_its_created_temp() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let expected = target.identity.as_ref().unwrap();
        let installed = POLKIT_AFTER.as_bytes();
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let mutation = transaction
            .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
            .unwrap();
        let collision = dirs.backup_dir().join(publication_name(
            PublicationRole::VendorCreate,
            &mutation.operation,
        ));
        let administrator = b"administrator publication collision\n";
        fs::write(&collision, administrator).unwrap();

        let error = transaction
            .create_vendor_with_intent(&mutation, &target, expected, installed)
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert!(
            !etc.join(vendor_create_name(&mutation.operation)).exists(),
            "a failed binding must not leave its vendor temp unauthenticated"
        );
        assert!(
            !dirs
                .backup_dir()
                .join(intent_name(IntentRole::VendorCreate, &mutation.operation))
                .exists(),
            "the intent may be removed only after exact temp cleanup succeeds"
        );
        assert!(!etc.join("polkit-1").exists());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert!(!etc.join("polkit-1").exists());
    }

    #[test]
    fn pam_replace_preserves_a_substituted_bound_temp_when_source_drifts() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let path = dir.path().join("sudo");
        let (_, expected) = read_regular_nofollow(&path).unwrap();
        let temp = dir.path().join(pam_replace_name(prepared.backup_name()));
        let holding = dir.path().join("facelock-created-replacement");
        let administrator_temp = b"administrator reserved-name replacement\n";
        let administrator_source = b"administrator changed the PAM service\n";

        let error = store
            .replace_pam_with_intent_hook(
                &prepared,
                &path,
                &expected,
                SUDO_AFTER.as_bytes(),
                |point| {
                    if point == PamReplaceCrashPoint::ReplacementTemp {
                        fs::rename(&temp, &holding).unwrap();
                        fs::write(&temp, administrator_temp).unwrap();
                        fs::write(&path, administrator_source).unwrap();
                    }
                    Ok(())
                },
            )
            .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert_eq!(fs::read(&holding).unwrap(), SUDO_AFTER.as_bytes());
        assert_eq!(fs::read(&path).unwrap(), administrator_source);
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::PamReplace, prepared.backup_name()));
        let binding = dirs.backup_dir().join(publication_name(
            PublicationRole::PamReplace,
            prepared.backup_name(),
        ));
        assert!(intent.exists());
        assert!(binding.exists());

        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert!(intent.exists());
        assert!(binding.exists());
    }

    #[test]
    fn pam_remove_preserves_a_substituted_bound_temp_when_source_drifts() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let path = dir.path().join("sudo");
        let (original, expected) = read_regular_nofollow(&path).unwrap();
        let installed = with_line_removed(&original);
        let mutation = transaction
            .plan_mutation("sudo", &original, &installed)
            .unwrap();
        let temp = dir.path().join(pam_remove_name(&mutation.operation));
        let holding = dir.path().join("facelock-created-removal");
        let administrator_temp = b"administrator reserved-name removal\n";
        let administrator_source = b"administrator changed the PAM service\n";

        let error = transaction
            .remove_pam_with_intent_hook(&mutation, &path, &expected, &installed, |point| {
                if point == PamRemoveCrashPoint::ReplacementTemp {
                    fs::rename(&temp, &holding).unwrap();
                    fs::write(&temp, administrator_temp).unwrap();
                    fs::write(&path, administrator_source).unwrap();
                }
                Ok(())
            })
            .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert_eq!(fs::read(&holding).unwrap(), installed);
        assert_eq!(fs::read(&path).unwrap(), administrator_source);
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::PamRemove, &mutation.operation));
        let binding = dirs.backup_dir().join(publication_name(
            PublicationRole::PamRemove,
            &mutation.operation,
        ));
        assert!(intent.exists());
        assert!(binding.exists());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert!(intent.exists());
        assert!(binding.exists());
    }

    #[test]
    fn vendor_create_preserves_a_substituted_bound_temp_when_canonical_collides() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let expected = target.identity.as_ref().unwrap();
        let installed = POLKIT_AFTER.as_bytes();
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let mutation = transaction
            .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
            .unwrap();
        let temp = etc.join(vendor_create_name(&mutation.operation));
        let holding = etc.join("facelock-created-vendor-override");
        let canonical = etc.join("polkit-1");
        let administrator_temp = b"administrator reserved-name override\n";
        let administrator_canonical = b"administrator canonical override\n";

        let error = transaction
            .create_vendor_with_intent_hook(&mutation, &target, expected, installed, |point| {
                if point == VendorCreateCrashPoint::ReplacementTemp {
                    fs::rename(&temp, &holding).unwrap();
                    fs::write(&temp, administrator_temp).unwrap();
                    fs::write(&canonical, administrator_canonical).unwrap();
                }
                Ok(())
            })
            .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert_eq!(fs::read(&holding).unwrap(), installed);
        assert_eq!(fs::read(&canonical).unwrap(), administrator_canonical);
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::VendorCreate, &mutation.operation));
        let binding = dirs.backup_dir().join(publication_name(
            PublicationRole::VendorCreate,
            &mutation.operation,
        ));
        assert!(intent.exists());
        assert!(binding.exists());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert!(intent.exists());
        assert!(binding.exists());
    }

    #[test]
    fn vendor_create_preserves_a_substituted_bound_temp_when_source_drifts() {
        let (_root, etc, vendor) = pair();
        let vendor_path = vendor.join("polkit-1");
        fs::write(&vendor_path, POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let expected = target.identity.as_ref().unwrap();
        let installed = POLKIT_AFTER.as_bytes();
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let mutation = transaction
            .plan_mutation("polkit-1", POLKIT_BEFORE.as_bytes(), installed)
            .unwrap();
        let temp = etc.join(vendor_create_name(&mutation.operation));
        let holding = etc.join("facelock-created-vendor-override");
        let administrator_temp = b"administrator reserved-name override\n";
        let administrator_source = b"administrator changed vendor source\n";

        let error = transaction
            .create_vendor_with_intent_hook(&mutation, &target, expected, installed, |point| {
                if point == VendorCreateCrashPoint::ReplacementTemp {
                    fs::rename(&temp, &holding).unwrap();
                    fs::write(&temp, administrator_temp).unwrap();
                    fs::write(&vendor_path, administrator_source).unwrap();
                }
                Ok(())
            })
            .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert_eq!(fs::read(&holding).unwrap(), installed);
        assert_eq!(fs::read(&vendor_path).unwrap(), administrator_source);
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::VendorCreate, &mutation.operation));
        let binding = dirs.backup_dir().join(publication_name(
            PublicationRole::VendorCreate,
            &mutation.operation,
        ));
        assert!(intent.exists());
        assert!(binding.exists());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), administrator_temp);
        assert!(intent.exists());
        assert!(binding.exists());
    }

    #[test]
    fn pam_replace_rejects_a_prebinding_temp_substitution() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let path = dir.path().join("sudo");
        let (_, expected) = read_regular_nofollow(&path).unwrap();
        let temp = dir.path().join(pam_replace_name(prepared.backup_name()));
        let holding = dir.path().join("facelock-created-prebinding-replacement");
        let administrator = b"different reserved-name PAM bytes\n";
        let hook_temp = temp.clone();
        let hook_holding = holding.clone();
        install_temp_creation_test_hook(move |_| {
            fs::rename(&hook_temp, &hook_holding)?;
            fs::write(&hook_temp, administrator)
        });

        let error = store
            .replace_pam_with_intent_hook(
                &prepared,
                &path,
                &expected,
                SUDO_AFTER.as_bytes(),
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&path).unwrap(), SUDO_BEFORE.as_bytes());
        assert_eq!(fs::read(&temp).unwrap(), administrator);
        assert_eq!(fs::read(&holding).unwrap(), SUDO_AFTER.as_bytes());
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::PamReplace, prepared.backup_name()));
        let binding = dirs.backup_dir().join(publication_name(
            PublicationRole::PamReplace,
            prepared.backup_name(),
        ));
        assert!(intent.exists());
        assert!(!binding.exists());

        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), administrator);
        assert!(intent.exists());
    }

    #[test]
    fn vendor_create_rejects_a_prebinding_temp_substitution() {
        let (_root, etc, vendor) = pair();
        let vendor_path = vendor.join("polkit-1");
        fs::write(&vendor_path, POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let expected = target.identity.as_ref().unwrap();
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let mutation = transaction
            .plan_mutation(
                "polkit-1",
                POLKIT_BEFORE.as_bytes(),
                POLKIT_AFTER.as_bytes(),
            )
            .unwrap();
        let temp = etc.join(vendor_create_name(&mutation.operation));
        let holding = etc.join("facelock-created-prebinding-vendor-override");
        let canonical = etc.join("polkit-1");
        let administrator = b"different reserved-name vendor bytes\n";
        let hook_temp = temp.clone();
        let hook_holding = holding.clone();
        install_temp_creation_test_hook(move |_| {
            fs::rename(&hook_temp, &hook_holding)?;
            fs::write(&hook_temp, administrator)
        });

        let error = transaction
            .create_vendor_with_intent_hook(
                &mutation,
                &target,
                expected,
                POLKIT_AFTER.as_bytes(),
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert!(!canonical.exists());
        assert_eq!(fs::read(&temp).unwrap(), administrator);
        assert_eq!(fs::read(&holding).unwrap(), POLKIT_AFTER.as_bytes());
        let intent = dirs
            .backup_dir()
            .join(intent_name(IntentRole::VendorCreate, &mutation.operation));
        let binding = dirs.backup_dir().join(publication_name(
            PublicationRole::VendorCreate,
            &mutation.operation,
        ));
        assert!(intent.exists());
        assert!(!binding.exists());

        drop(transaction);
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), administrator);
        assert!(intent.exists());
    }

    #[test]
    fn creator_error_cleanup_preserves_a_substituted_temp() {
        let root = tempfile::tempdir().unwrap();
        let model_path = root.path().join("model");
        fs::write(&model_path, SUDO_AFTER).unwrap();
        let (_, model) = read_regular_nofollow(&model_path).unwrap();
        let directory = open_directory_nofollow(root.path()).unwrap();
        let temp_name = ".facelock-pam-replace-creator-error";
        let temp = root.path().join(temp_name);
        let holding = root.path().join("facelock-created-before-error");
        let administrator = b"different reserved-name error bytes\n";
        let hook_temp = temp.clone();
        let hook_holding = holding.clone();
        install_temp_creation_test_hook(move |_| {
            fs::rename(&hook_temp, &hook_holding)?;
            fs::write(&hook_temp, administrator)?;
            Err(std::io::Error::other("injected post-create failure"))
        });

        let error = create_temp_at_named_with_context_hook(
            &directory,
            OsStr::new("sudo"),
            SUDO_AFTER.as_bytes(),
            &model,
            None,
            Some(temp_name),
            |_, _| {},
        )
        .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), administrator);
        assert_eq!(fs::read(&holding).unwrap(), SUDO_AFTER.as_bytes());
    }

    #[test]
    fn pam_exchange_failure_preserves_a_substituted_created_temp() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let path = dir.path().join("sudo");
        let (_, expected) = read_regular_nofollow(&path).unwrap();
        let temp_name = ".facelock-pam-replace-test-transaction";
        let temp = dir.path().join(temp_name);
        let holding = dir.path().join("facelock-created-pam-replacement");
        let administrator = b"administrator reserved-name replacement\n";

        let error = replace_existing_verified_with_hooks(
            &path,
            &expected,
            SUDO_AFTER.as_bytes(),
            Some(temp_name),
            |_| Ok(()),
            || {
                fs::rename(&temp, &holding).unwrap();
                fs::write(&temp, administrator).unwrap();
                fs::remove_file(&path).unwrap();
            },
            || Ok(()),
        )
        .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), administrator);
        assert_eq!(fs::read(&holding).unwrap(), SUDO_AFTER.as_bytes());
    }

    #[test]
    fn commit_binding_collision_cleans_only_its_created_state_temp() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let collision = dirs.backup_dir().join(publication_name(
            PublicationRole::Commit,
            prepared.backup_name(),
        ));
        let administrator = b"administrator publication collision\n";
        fs::write(&collision, administrator).unwrap();

        let error = store.commit(&prepared).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert!(
            !dirs
                .backup_dir()
                .join(format!(
                    "{}.json",
                    quarantine_name("commit", prepared.backup_name())
                ))
                .exists(),
            "a failed binding must not leave its state replacement unauthenticated"
        );
        assert!(
            !dirs
                .backup_dir()
                .join(intent_name(IntentRole::Commit, prepared.backup_name()))
                .exists()
        );

        assert!(store.recover(&dirs).is_err());
        assert_eq!(fs::read(&collision).unwrap(), administrator);
        assert!(prepared.backup_path().exists());
        assert!(prepared.record_path().exists());
    }

    #[test]
    fn unpublished_temp_cleanup_preserves_an_identity_substitution() {
        let root = tempfile::tempdir().unwrap();
        let temp_name = ".facelock-pam-replace-sudo.1700000000-000000001";
        let temp = root.path().join(temp_name);
        let holding = root.path().join("administrator-held-created-temp");
        fs::write(&temp, SUDO_AFTER.as_bytes()).unwrap();
        let (_, created) = read_regular_nofollow(&temp).unwrap();
        fs::rename(&temp, &holding).unwrap();
        fs::write(&temp, SUDO_AFTER.as_bytes()).unwrap();

        let error = cleanup_unpublished_temp(
            root.path(),
            temp_name,
            &created,
            MAX_BACKUP_BYTES,
            "unpublished replacement temp became ambiguous",
        )
        .unwrap_err();

        assert!(is_ambiguous_publication(&error));
        assert_eq!(fs::read(&temp).unwrap(), SUDO_AFTER.as_bytes());
        assert_eq!(fs::read(&holding).unwrap(), SUDO_AFTER.as_bytes());
    }

    #[test]
    fn publication_cleanup_recovers_both_state_unlink_boundaries() {
        for crash_at in [
            PublicationCleanupPoint::IntentUnlink,
            PublicationCleanupPoint::BindingUnlink,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let (_, published) = read_regular_nofollow(&dir.path().join("sudo")).unwrap();
            let intent = StateIntent {
                version: PROVENANCE_VERSION,
                role: IntentRole::PamRemove,
                sequence: 1,
                service: "sudo".into(),
                backup: "sudo.1700000000-000000001".into(),
                original_sha256: published.sha256.clone(),
                installed_sha256: published.sha256.clone(),
                record_sha256: None,
                replacement_record_sha256: None,
                original_device: Some(published.device),
                original_inode: Some(published.inode),
                original_links: Some(published.links),
                original_mode: Some(published.mode),
                original_uid: Some(published.uid),
                original_gid: Some(published.gid),
            };
            let intent_name = intent_name(IntentRole::PamRemove, &intent.backup);
            let intent_encoded = serde_json::to_vec_pretty(&intent).unwrap();
            let intent_identity =
                atomic_state_create(dirs.backup_dir(), &intent_name, &intent_encoded).unwrap();
            let publication = store
                .create_publication_binding(
                    PublicationRole::PamRemove,
                    &intent,
                    &intent_encoded,
                    &published,
                )
                .unwrap();

            let error = store
                .finish_publication_state_with_hook(
                    &intent_name,
                    &intent_identity,
                    Some(&publication),
                    |point| {
                        if point == crash_at {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "simulated crash",
                            ));
                        }
                        Ok(())
                    },
                )
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);

            store.recover(&dirs).unwrap();

            assert!(!dirs.backup_dir().join(&intent_name).exists());
            assert!(!dirs.backup_dir().join(&publication.name).exists());
            assert_eq!(
                fs::read(dir.path().join("sudo")).unwrap(),
                SUDO_AFTER.as_bytes()
            );
        }
    }

    #[test]
    fn recovery_completes_every_commit_exchange_boundary() {
        for crash_at in [
            CommitCrashPoint::Intent,
            CommitCrashPoint::ReplacementTemp,
            CommitCrashPoint::Exchange,
            CommitCrashPoint::DisplacedUnlink,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
                .unwrap();

            let error = store
                .commit_with_hook(&prepared, |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);

            store.recover(&dirs).unwrap();

            let record: ProvenanceRecord =
                serde_json::from_slice(&fs::read(prepared.record_path()).unwrap()).unwrap();
            assert_eq!(record.state, ProvenanceState::Committed);
            assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 2);
        }
    }

    #[test]
    fn commit_preserves_a_chmod_mutated_prepared_record() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        fs::set_permissions(prepared.record_path(), fs::Permissions::from_mode(0o640)).unwrap();

        let result = store.commit(&prepared);

        assert!(result.is_err());
        let record: ProvenanceRecord =
            serde_json::from_slice(&fs::read(prepared.record_path()).unwrap()).unwrap();
        assert_eq!(record.state, ProvenanceState::Prepared);
        assert_eq!(
            fs::metadata(prepared.record_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o640
        );
    }

    #[test]
    fn commit_preserves_an_exact_byte_metadata_inode_substitution() {
        let root = tempfile::tempdir().unwrap();
        let store = BackupStore::open(root.path()).unwrap();
        let prepared = store
            .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
            .unwrap();
        let encoded = fs::read(prepared.record_path()).unwrap();
        let displaced = root.path().join("administrator-prepared-record");

        let result = store.commit_with_hook(&prepared, |point| {
            if point == CommitCrashPoint::ReplacementTemp {
                fs::rename(prepared.record_path(), &displaced).unwrap();
                fs::write(prepared.record_path(), &encoded).unwrap();
                fs::set_permissions(prepared.record_path(), fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            Ok(())
        });

        assert!(result.is_err());
        assert_eq!(fs::read(prepared.record_path()).unwrap(), encoded);
        assert_eq!(fs::read(displaced).unwrap(), encoded);
    }

    #[test]
    fn commit_final_check_preserves_post_publication_substitutions() {
        for mutation in [
            PostPublicationMutation::ExactBytesNewInode,
            PostPublicationMutation::Chmod,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
                .unwrap();
            let canonical = prepared.record_path();
            let holding = dirs.backup_dir().join("administrator-committed-record");

            let result = store.commit_with_hook(&prepared, |point| {
                if point == CommitCrashPoint::Exchange {
                    mutate_published_file(&canonical, &holding, mutation);
                }
                Ok(())
            });

            assert!(result.is_err());
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::Commit, prepared.backup_name()));
            let displaced = dirs.backup_dir().join(format!(
                "{}.json",
                quarantine_name("commit", prepared.backup_name())
            ));
            store.recover(&dirs).unwrap();
            assert!(intent.exists(), "ambiguous commit intent must remain");
            assert!(displaced.exists(), "displaced prepared record must remain");
            assert_eq!(
                serde_json::from_slice::<ProvenanceRecord>(&fs::read(&canonical).unwrap())
                    .unwrap()
                    .state,
                ProvenanceState::Committed
            );
        }
    }

    #[test]
    fn commit_rechecks_publication_after_displaced_cleanup() {
        for mutation in [
            PostPublicationMutation::ExactBytesNewInode,
            PostPublicationMutation::Chmod,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
                .unwrap();
            let canonical = prepared.record_path();
            let holding = dirs
                .backup_dir()
                .join("administrator-late-committed-record");

            let result = store.commit_with_hook(&prepared, |point| {
                if point == CommitCrashPoint::DisplacedUnlink {
                    mutate_published_file(&canonical, &holding, mutation);
                }
                Ok(())
            });

            assert!(result.is_err());
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::Commit, prepared.backup_name()));
            let publication = dirs.backup_dir().join(publication_name(
                PublicationRole::Commit,
                prepared.backup_name(),
            ));
            store.recover(&dirs).unwrap();
            assert!(intent.exists());
            assert!(publication.exists());
        }
    }

    #[test]
    fn recovery_completes_every_pair_cleanup_boundary() {
        for crash_at in [
            CleanupCrashPoint::Intent,
            CleanupCrashPoint::BackupQuarantine,
            CleanupCrashPoint::RecordQuarantine,
            CleanupCrashPoint::BackupUnlink,
            CleanupCrashPoint::RecordUnlink,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), SUDO_AFTER.as_bytes())
                .unwrap();
            store.commit(&prepared).unwrap();

            let error = store
                .cleanup_with_crash_hook("sudo", |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);

            store.recover(&dirs).unwrap();

            assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
            assert_eq!(
                fs::read(dir.path().join("sudo")).unwrap(),
                SUDO_AFTER.as_bytes()
            );
        }
    }

    #[test]
    fn recovery_resolves_every_pam_exchange_boundary() {
        for crash_at in [
            PamReplaceCrashPoint::Intent,
            PamReplaceCrashPoint::ReplacementTemp,
            PamReplaceCrashPoint::Exchange,
            PamReplaceCrashPoint::Finalize,
        ] {
            let dir = seeded(&[("sudo", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let installed = SUDO_AFTER.as_bytes();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), installed)
                .unwrap();
            let (_, expected) = read_regular_nofollow(&dir.path().join("sudo")).unwrap();

            let error = store
                .replace_pam_with_intent_hook(
                    &prepared,
                    &dir.path().join("sudo"),
                    &expected,
                    installed,
                    |point| {
                        if point == crash_at {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "simulated crash",
                            ));
                        }
                        Ok(())
                    },
                )
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);

            store.recover(&dirs).unwrap();

            if matches!(
                crash_at,
                PamReplaceCrashPoint::Exchange | PamReplaceCrashPoint::Finalize
            ) {
                assert_eq!(fs::read(dir.path().join("sudo")).unwrap(), installed);
                let record: ProvenanceRecord =
                    serde_json::from_slice(&fs::read(prepared.record_path()).unwrap()).unwrap();
                assert_eq!(record.state, ProvenanceState::Committed);
                assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 2);
            } else {
                assert_eq!(
                    fs::read(dir.path().join("sudo")).unwrap(),
                    SUDO_BEFORE.as_bytes()
                );
                assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
            }
            assert!(
                !dir.path()
                    .join(pam_replace_name(prepared.backup_name()))
                    .exists()
            );
        }
    }

    #[test]
    fn pam_replace_recovery_preserves_chmod_mutated_original_or_replacement() {
        for mutate_replacement in [false, true] {
            let dir = seeded(&[("sudo", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let installed = SUDO_AFTER.as_bytes();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), installed)
                .unwrap();
            let path = dir.path().join("sudo");
            let (_, expected) = read_regular_nofollow(&path).unwrap();
            let error = store
                .replace_pam_with_intent_hook(&prepared, &path, &expected, installed, |point| {
                    if point == PamReplaceCrashPoint::ReplacementTemp {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "simulated crash",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);

            let temp = dir.path().join(pam_replace_name(prepared.backup_name()));
            let changed = if mutate_replacement { &temp } else { &path };
            let mode = fs::metadata(changed).unwrap().permissions().mode() & 0o7777;
            fs::set_permissions(changed, fs::Permissions::from_mode(mode ^ 0o040)).unwrap();
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::PamReplace, prepared.backup_name()));

            store.recover(&dirs).unwrap();

            assert_eq!(fs::read(&path).unwrap(), SUDO_BEFORE.as_bytes());
            assert_eq!(fs::read(&temp).unwrap(), installed);
            assert!(intent.exists());
            assert!(prepared.backup_path().exists());
            assert!(prepared.record_path().exists());
        }
    }

    #[test]
    fn pam_replace_final_check_preserves_post_publication_substitutions() {
        for mutation in [
            PostPublicationMutation::ExactBytesNewInode,
            PostPublicationMutation::Chmod,
        ] {
            let dir = seeded(&[("sudo", SUDO_BEFORE)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let installed = SUDO_AFTER.as_bytes();
            let prepared = store
                .prepare("sudo", SUDO_BEFORE.as_bytes(), installed)
                .unwrap();
            let path = dir.path().join("sudo");
            let (_, expected) = read_regular_nofollow(&path).unwrap();
            let holding = dir.path().join("administrator-published-replacement");

            let result = store.replace_pam_with_intent_hook(
                &prepared,
                &path,
                &expected,
                installed,
                |point| {
                    if point == PamReplaceCrashPoint::Exchange {
                        mutate_published_file(&path, &holding, mutation);
                    }
                    Ok(())
                },
            );

            assert!(result.is_err());
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::PamReplace, prepared.backup_name()));
            let displaced = dir.path().join(pam_replace_name(prepared.backup_name()));
            store.recover(&dirs).unwrap();
            assert_eq!(fs::read(&path).unwrap(), installed);
            assert_eq!(fs::read(&displaced).unwrap(), SUDO_BEFORE.as_bytes());
            assert!(intent.exists(), "ambiguous replacement intent must remain");
        }
    }

    #[test]
    fn pam_remove_final_check_preserves_post_publication_substitutions() {
        for mutation in [
            PostPublicationMutation::ExactBytesNewInode,
            PostPublicationMutation::Chmod,
        ] {
            let dir = seeded(&[("sudo", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let path = dir.path().join("sudo");
            let (original, expected) = read_regular_nofollow(&path).unwrap();
            let installed = with_line_removed(&original);
            let mutation_plan = transaction
                .plan_mutation("sudo", &original, &installed)
                .unwrap();
            let holding = dir.path().join("administrator-published-removal");

            let result = transaction.remove_pam_with_intent_hook(
                &mutation_plan,
                &path,
                &expected,
                &installed,
                |point| {
                    if point == PamRemoveCrashPoint::Exchange {
                        mutate_published_file(&path, &holding, mutation);
                    }
                    Ok(())
                },
            );

            assert!(result.is_err());
            drop(transaction);
            let intent = dirs
                .backup_dir()
                .join(intent_name(IntentRole::PamRemove, &mutation_plan.operation));
            let displaced = dir.path().join(pam_remove_name(&mutation_plan.operation));
            store.recover(&dirs).unwrap();
            assert_eq!(fs::read(&path).unwrap(), installed);
            assert_eq!(fs::read(&displaced).unwrap(), original);
            assert!(intent.exists(), "ambiguous removal intent must remain");
        }
    }

    #[test]
    fn pam_mutations_recheck_publication_before_intent_cleanup() {
        for (remove, mutation) in [
            (false, PostPublicationMutation::ExactBytesNewInode),
            (false, PostPublicationMutation::Chmod),
            (true, PostPublicationMutation::ExactBytesNewInode),
            (true, PostPublicationMutation::Chmod),
        ] {
            let initial = if remove { SUDO_AFTER } else { SUDO_BEFORE };
            let dir = seeded(&[("sudo", initial)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let path = dir.path().join("sudo");
            let (original, expected) = read_regular_nofollow(&path).unwrap();
            let installed = if remove {
                with_line_removed(&original)
            } else {
                SUDO_AFTER.as_bytes().to_vec()
            };
            let holding = dir.path().join("administrator-late-pam-entry");

            let (result, role, operation) = if remove {
                let plan = transaction
                    .plan_mutation("sudo", &original, &installed)
                    .unwrap();
                let result = transaction.remove_pam_with_intent_hook(
                    &plan,
                    &path,
                    &expected,
                    &installed,
                    |point| {
                        if point == PamRemoveCrashPoint::Finalize {
                            mutate_published_file(&path, &holding, mutation);
                        }
                        Ok(())
                    },
                );
                (result, IntentRole::PamRemove, plan.operation)
            } else {
                let prepared = transaction.plan("sudo", &original, &installed).unwrap();
                transaction.persist(&prepared, &original).unwrap();
                let result = transaction.replace_pam_with_intent_hook(
                    &prepared,
                    &path,
                    &expected,
                    &installed,
                    |point| {
                        if point == PamReplaceCrashPoint::Finalize {
                            mutate_published_file(&path, &holding, mutation);
                        }
                        Ok(())
                    },
                );
                (result, IntentRole::PamReplace, prepared.backup)
            };

            assert!(result.is_err());
            drop(transaction);
            let intent = dirs.backup_dir().join(intent_name(role, &operation));
            store.recover(&dirs).unwrap();
            assert!(intent.exists());
        }
    }

    #[test]
    fn a_vendor_override_that_appears_after_planning_is_not_overwritten() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();
        let administrator = b"# administrator override\n";
        fs::write(etc.join("polkit-1"), administrator).unwrap();

        let reports = apply_all(&dirs, &targets, &write, &Sink::verb(true));

        assert!(matches!(reports[0].outcome, Outcome::Failed(_)));
        assert_eq!(fs::read(etc.join("polkit-1")).unwrap(), administrator);
    }

    #[test]
    fn a_vendor_override_uses_the_destination_selinux_label() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();
        let target = &targets[0];
        let expected = target.identity.as_ref().unwrap();
        let mut copied_from_vendor = true;

        create_override_verified_with_context_hook(
            target,
            expected,
            b"# replacement\n",
            |source, _destination| copied_from_vendor = source.is_some(),
        )
        .unwrap();

        assert!(etc.join("polkit-1").exists());
        assert!(
            !copied_from_vendor,
            "a new /etc override must keep the label assigned by its destination directory"
        );
    }

    #[test]
    fn ownership_is_applied_before_the_final_setid_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replacement");
        fs::write(&path, b"replacement\n").unwrap();
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let metadata = file.metadata().unwrap();

        apply_owner_then_mode_with_hook(
            &file,
            metadata.uid().saturating_add(1),
            metadata.gid(),
            0o6755,
            |file, _, _| {
                // Linux clears setuid/setgid during fchown. Simulate that
                // side effect without requiring this test process to be root.
                file.set_permissions(fs::Permissions::from_mode(0o0755))
            },
        )
        .unwrap();

        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o7777,
            0o6755
        );
    }

    fn seeded(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    #[derive(Clone, Copy)]
    enum PostPublicationMutation {
        ExactBytesNewInode,
        Chmod,
    }

    fn mutate_published_file(path: &Path, holding: &Path, mutation: PostPublicationMutation) {
        let content = fs::read(path).unwrap();
        let metadata = fs::metadata(path).unwrap();
        match mutation {
            PostPublicationMutation::ExactBytesNewInode => {
                fs::rename(path, holding).unwrap();
                fs::write(path, content).unwrap();
                fs::set_permissions(
                    path,
                    fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
                )
                .unwrap();
                assert_ne!(fs::metadata(path).unwrap().ino(), metadata.ino());
            }
            PostPublicationMutation::Chmod => {
                fs::set_permissions(
                    path,
                    fs::Permissions::from_mode((metadata.permissions().mode() & 0o7777) ^ 0o040),
                )
                .unwrap();
            }
        }
    }

    /// Every entry under `dir` and its exact bytes. Enumerating the directory
    /// rather than the files we wrote is what catches a stray
    /// `.facelock-backup` appearing where none should.
    fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                (!entry.file_type().unwrap().is_dir()).then_some(entry)
            })
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap_or_default(),
                )
            })
            .collect()
    }

    #[derive(Debug, PartialEq, Eq)]
    struct DirectorySnapshot {
        device: u64,
        inode: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
        entries: BTreeMap<String, Vec<u8>>,
    }

    fn directory_snapshot(dir: &Path) -> DirectorySnapshot {
        let metadata = fs::symlink_metadata(dir).unwrap();
        DirectorySnapshot {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            entries: snapshot(dir),
        }
    }

    fn add(services: &[&str]) -> PamRequest {
        PamRequest {
            action: PamAction::Add,
            services: services.iter().map(|s| s.to_string()).collect(),
            no_confirm: true,
            ..PamRequest::default()
        }
    }

    fn remove(services: &[&str]) -> PamRequest {
        PamRequest {
            action: PamAction::Remove,
            services: services.iter().map(|s| s.to_string()).collect(),
            no_confirm: true,
            ..PamRequest::default()
        }
    }

    fn read(dir: &tempfile::TempDir, name: &str) -> String {
        fs::read_to_string(dir.path().join(name)).unwrap()
    }

    fn latest_backup_bytes(dirs: &PamDirs, service: &str) -> Vec<u8> {
        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        let path = store.latest_committed(service).unwrap().unwrap();
        fs::read(path).unwrap()
    }

    // -- byte identity with `main` ------------------------------------------

    #[test]
    fn add_reproduces_main_byte_for_byte() {
        for (service, before, after) in [
            ("sudo", SUDO_BEFORE, SUDO_AFTER),
            ("polkit-1", POLKIT_BEFORE, POLKIT_AFTER),
            ("omarchy-lock-face", OMARCHY_PRESENT, OMARCHY_PRESENT),
            ("sudo", NO_NEWLINE_BEFORE, NO_NEWLINE_AFTER),
        ] {
            let dir = seeded(&[(service, before)]);
            let code = write_in(&only(dir.path()), &add(&[service])).unwrap();

            assert_eq!(code, WRITE_OK, "{service}");
            assert_eq!(read(&dir, service), after, "{service} content");
        }
    }

    #[test]
    fn insertion_treats_a_backslash_continuation_as_one_logical_rule() {
        let before = concat!(
            "password required pam_pwquality.so \\ \t\n",
            "    auth\n",
            "auth include system-auth\n",
        );
        let after = concat!(
            "password required pam_pwquality.so \\ \t\n",
            "    auth\n",
            "auth      sufficient pam_facelock.so\n",
            "auth include system-auth\n",
        );

        assert_eq!(with_line_inserted(before.as_bytes()), after.as_bytes());
    }

    #[test]
    fn insertion_does_not_split_the_issue_192_authtok_continuation() {
        let before = concat!(
            "password required pam_pwquality.so \\\n",
            "    authtok_type=\n",
            "auth include system-auth\n",
        );
        let after = concat!(
            "password required pam_pwquality.so \\\n",
            "    authtok_type=\n",
            "auth      sufficient pam_facelock.so\n",
            "auth include system-auth\n",
        );

        assert_eq!(with_line_inserted(before.as_bytes()), after.as_bytes());
    }

    #[test]
    fn insertion_and_removal_preserve_crlf_bytes() {
        let before = b"#%PAM-1.0\r\nauth include system-auth\r\n";
        let installed =
            b"#%PAM-1.0\r\nauth      sufficient pam_facelock.so\r\nauth include system-auth\r\n";
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("sudo"), before).unwrap();

        assert_eq!(
            write_in(&only(dir.path()), &add(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(fs::read(dir.path().join("sudo")).unwrap(), installed);
        let dirs = only(dir.path());
        assert_eq!(latest_backup_bytes(&dirs, "sudo"), before);
        assert_eq!(
            write_in(&only(dir.path()), &remove(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(fs::read(dir.path().join("sudo")).unwrap(), before);
    }

    #[test]
    fn unterminated_auth_rule_uses_the_documents_crlf_ending() {
        let before = b"#%PAM-1.0\r\nauth include system-auth";
        let installed =
            b"#%PAM-1.0\r\nauth      sufficient pam_facelock.so\r\nauth include system-auth";

        assert_eq!(with_line_inserted(before), installed);
    }

    #[test]
    fn insertion_accepts_linux_pam_auth_types_but_not_authtok_type() {
        let before = "#%PAM-1.0\nauthtok_type=\n-AUTH include system-auth\n";
        let after = concat!(
            "#%PAM-1.0\n",
            "authtok_type=\n",
            "auth      sufficient pam_facelock.so\n",
            "-AUTH include system-auth\n",
        );

        assert_eq!(with_line_inserted(before.as_bytes()), after.as_bytes());
    }

    #[test]
    fn no_auth_insertion_follows_the_pam_header_and_preserves_line_endings() {
        for (before, after) in [
            (
                b"#%PAM-1.0\naccount include system-auth\n".as_slice(),
                b"#%PAM-1.0\nauth      sufficient pam_facelock.so\naccount include system-auth\n"
                    .as_slice(),
            ),
            (
                b"#%PAM-1.0\r\naccount include system-auth\r\n".as_slice(),
                b"#%PAM-1.0\r\nauth      sufficient pam_facelock.so\r\naccount include system-auth\r\n"
                    .as_slice(),
            ),
            (
                b"#%PAM-1.0".as_slice(),
                b"#%PAM-1.0\nauth      sufficient pam_facelock.so".as_slice(),
            ),
        ] {
            assert_eq!(with_line_inserted(before), after);
        }
    }

    #[test]
    fn no_auth_preview_describes_header_aware_insertion() {
        assert_eq!(
            insertion_hint(b"#%PAM-1.0\naccount required pam_unix.so\n").localized(),
            "no 'auth' line found — inserted after the PAM header"
        );
        assert_eq!(
            insertion_hint(b"account required pam_unix.so\n").localized(),
            "no 'auth' line found — inserted at the top of the file"
        );
    }

    #[test]
    fn add_preserves_invalid_bytes_and_no_op_is_byte_identical() {
        let before = b"#%PAM-1.0\n# vendor byte: \xff\nauth include system-auth\n";
        let installed = b"#%PAM-1.0\n# vendor byte: \xff\nauth      sufficient pam_facelock.so\nauth include system-auth\n";
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("sudo"), before).unwrap();

        assert_eq!(
            write_in(&only(dir.path()), &add(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(fs::read(dir.path().join("sudo")).unwrap(), installed);
        let dirs = only(dir.path());
        assert_eq!(latest_backup_bytes(&dirs, "sudo"), before);

        let unchanged = snapshot(dir.path());
        assert_eq!(
            write_in(&only(dir.path()), &add(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(
            snapshot(dir.path()),
            unchanged,
            "a configured non-UTF-8 document is a byte-identical no-op"
        );
    }

    #[test]
    fn removal_drops_the_whole_continued_rule_and_preserves_other_bytes() {
        let installed = b"#%PAM-1.0\r\nauth sufficient pam_facelock.so \\\r\n    debug=\xff\r\nauth include system-auth";
        let expected = b"#%PAM-1.0\r\nauth include system-auth";
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("sudo"), installed).unwrap();

        assert_eq!(
            write_in(&only(dir.path()), &remove(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(fs::read(dir.path().join("sudo")).unwrap(), expected);
    }

    #[test]
    fn removal_repairs_a_legacy_line_inserted_inside_an_admin_rule() {
        let installed = concat!(
            "#%PAM-1.0\n",
            "password required pam_pwquality.so \\\n",
            "auth      sufficient pam_facelock.so\n",
            "    authtok_type=\n",
            "auth include system-auth\n",
        );
        let expected = concat!(
            "#%PAM-1.0\n",
            "password required pam_pwquality.so \\\n",
            "    authtok_type=\n",
            "auth include system-auth\n",
        );
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("sudo"), installed).unwrap();

        assert!(is_configured(&only(dir.path()), "sudo"));
        assert_eq!(
            write_in(&only(dir.path()), &remove(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(
            fs::read(dir.path().join("sudo")).unwrap(),
            expected.as_bytes()
        );
    }

    #[test]
    fn legacy_repair_is_structural_for_auth_typed_admin_rules() {
        for (installed, expected) in [
            (
                concat!(
                    "AUTH required pam_unix.so \\\n",
                    "auth      sufficient pam_facelock.so\n",
                    "    debug\n",
                    "auth requisite pam_deny.so\n",
                ),
                concat!(
                    "AUTH required pam_unix.so \\\n",
                    "    debug\n",
                    "auth requisite pam_deny.so\n",
                ),
            ),
            (
                concat!(
                    "-auth required pam_unix.so \\\n",
                    "auth      sufficient pam_facelock.so\n",
                    "    debug\n",
                    "auth requisite pam_deny.so\n",
                ),
                concat!(
                    "-auth required pam_unix.so \\\n",
                    "    debug\n",
                    "auth requisite pam_deny.so\n",
                ),
            ),
        ] {
            assert!(PamDocument::new(installed.as_bytes()).has_facelock_rule());
            assert_eq!(with_line_removed(installed.as_bytes()), expected.as_bytes());
        }
    }

    #[test]
    fn comments_terminate_continuations_without_hiding_or_removing_rules() {
        let commented_module = concat!(
            "auth required pam_unix.so \\\n",
            "# pam_facelock.so\n",
            "auth requisite pam_deny.so\n",
        );
        assert!(!PamDocument::new(commented_module.as_bytes()).has_facelock_rule());
        assert_eq!(
            with_line_removed(commented_module.as_bytes()),
            commented_module.as_bytes()
        );

        let active_module = concat!(
            "# note \\\n",
            "auth sufficient pam_facelock.so\n",
            "auth requisite pam_deny.so\n",
        );
        assert!(PamDocument::new(active_module.as_bytes()).has_facelock_rule());
        assert_eq!(
            with_line_removed(active_module.as_bytes()),
            b"# note \\\nauth requisite pam_deny.so\n"
        );
    }

    /// The backup is a byte copy of the original, and it only appears when
    /// something was actually written.
    #[test]
    fn add_backup_is_the_original_and_only_exists_on_a_real_write() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        write_in(&dirs, &add(&["sudo"])).unwrap();
        assert_eq!(latest_backup_bytes(&dirs, "sudo"), SUDO_BEFORE.as_bytes());

        let untouched = seeded(&[("omarchy-lock-face", OMARCHY_PRESENT)]);
        let before = snapshot(untouched.path());
        write_in(&only(untouched.path()), &add(&["omarchy-lock-face"])).unwrap();
        assert_eq!(
            before,
            snapshot(untouched.path()),
            "an already-configured service must not gain a backup"
        );
    }

    #[test]
    fn remove_reproduces_main_byte_for_byte() {
        let dir = seeded(&[
            ("omarchy-lock-face", OMARCHY_PRESENT),
            ("sudo", SUDO_BEFORE),
        ]);
        let code = write_in(&only(dir.path()), &remove(&["omarchy-lock-face", "sudo"])).unwrap();

        assert_eq!(code, WRITE_OK);
        assert_eq!(read(&dir, "omarchy-lock-face"), OMARCHY_REMOVED);
        assert_eq!(read(&dir, "sudo"), SUDO_BEFORE, "no line, no rewrite");
    }

    /// The `setup --pam` alias reaches the writer through `install_one_in`,
    /// not through `write_in`, so the goldens have to cover it too — the two
    /// entry points sharing an engine is the claim, and this is what makes it
    /// a checked one rather than a comment.
    #[test]
    fn the_setup_alias_writes_the_same_bytes_as_the_verb() {
        for (service, before, after) in [
            ("sudo", SUDO_BEFORE, SUDO_AFTER),
            ("polkit-1", POLKIT_BEFORE, POLKIT_AFTER),
            ("omarchy-lock-face", OMARCHY_PRESENT, OMARCHY_PRESENT),
            ("sudo", NO_NEWLINE_BEFORE, NO_NEWLINE_AFTER),
        ] {
            let via_alias = seeded(&[(service, before)]);
            install_one_in(
                &only(via_alias.path()),
                &PamRequest {
                    allow_sensitive: true,
                    ..add(&[service])
                },
            )
            .unwrap();

            let via_verb = seeded(&[(service, before)]);
            write_in(&only(via_verb.path()), &add(&[service])).unwrap();

            assert_eq!(read(&via_alias, service), after, "{service} via the alias");
            assert_eq!(
                snapshot(via_alias.path()),
                snapshot(via_verb.path()),
                "{service}: the alias and the verb must leave the same directory"
            );
        }
    }

    /// `add` then `remove` is a round trip, which is the property that makes
    /// the rollback advice in `PamInstalled` true.
    #[test]
    fn add_then_remove_restores_the_original_bytes() {
        for before in [SUDO_BEFORE, POLKIT_BEFORE, NO_NEWLINE_BEFORE] {
            let dir = seeded(&[("sudo", before)]);
            write_in(&only(dir.path()), &add(&["sudo"])).unwrap();
            write_in(&only(dir.path()), &remove(&["sudo"])).unwrap();
            assert_eq!(read(&dir, "sudo"), before);
        }
    }

    // -- confinement --------------------------------------------------------

    #[test]
    fn one_path_component_is_the_whole_rule() {
        for good in ["sudo", "polkit-1", "omarchy-lock-face", "..x", "a\"b"] {
            assert!(confined(good).is_ok(), "{good} must be accepted");
        }
        for bad in [
            "",
            ".",
            "..",
            "/",
            "/etc/shadow",
            "../shadow",
            "a/b",
            "sudo/",
            "./sudo",
            "sudo\0",
        ] {
            assert!(confined(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    /// The property `pam_remove_absolute_service_stays_anchored_under_base`
    /// protected — never touch a file outside `base` — now holds by rejecting
    /// the name before any I/O rather than by re-anchoring it. The old writer
    /// stripped a leading `/` on removal and stripped nothing on install, so
    /// `--service /etc/shadow` resolved to `/etc/shadow` on the install side.
    #[test]
    fn an_absolute_service_is_rejected_before_any_io() {
        for action in [PamAction::Add, PamAction::Remove] {
            let dir = tempfile::TempDir::new().unwrap();
            let base = dir.path().join("pam.d");
            fs::create_dir(&base).unwrap();
            let outside = dir.path().join("outside-service");
            fs::write(&outside, OMARCHY_PRESENT).unwrap();

            let request = PamRequest {
                action,
                services: vec![outside.to_str().unwrap().to_string()],
                no_confirm: true,
                if_present: true,
                ..PamRequest::default()
            };
            let error = write_in(&only(&base), &request).unwrap_err().to_string();

            assert!(error.contains("Invalid PAM service name"), "got: {error}");
            assert_eq!(fs::read_to_string(&outside).unwrap(), OMARCHY_PRESENT);
            assert!(fs::read_dir(&base).unwrap().next().is_none());
        }
    }

    #[test]
    fn a_parent_traversal_is_rejected_before_any_io() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        fs::write(dir.path().join("shadow"), "root:!:1::::::\n").unwrap();

        for action in [PamAction::Add, PamAction::Remove] {
            let request = PamRequest {
                action,
                services: vec!["../shadow".to_string()],
                no_confirm: true,
                if_present: true,
                ..PamRequest::default()
            };
            assert!(write_in(&only(&base), &request).is_err());
        }
        assert_eq!(
            fs::read_to_string(dir.path().join("shadow")).unwrap(),
            "root:!:1::::::\n"
        );
    }

    // -- two-phase ----------------------------------------------------------

    /// The plan's acceptance criterion: a validation failure on service two
    /// leaves service one byte-identical and makes no backup. Without the
    /// phase split, `sudo` would already be written by the time `sshd` was
    /// rejected.
    #[test]
    fn a_validation_failure_writes_nothing_at_all() {
        for second in ["sshd", "does-not-exist", "../escape"] {
            let dir = seeded(&[("sudo", SUDO_BEFORE), ("sshd", SUDO_BEFORE)]);
            let before = snapshot(dir.path());

            let error = write_in(&only(dir.path()), &add(&["sudo", second])).unwrap_err();

            assert_eq!(
                before,
                snapshot(dir.path()),
                "`{second}` was rejected, so `sudo` must be untouched: {error}"
            );
        }
    }

    #[test]
    fn a_valid_multi_service_add_writes_every_service() {
        let dir = seeded(&[("sudo", SUDO_BEFORE), ("polkit-1", POLKIT_BEFORE)]);

        let code = write_in(&only(dir.path()), &add(&["sudo", "polkit-1"])).unwrap();

        assert_eq!(code, WRITE_OK);
        assert_eq!(read(&dir, "sudo"), SUDO_AFTER);
        assert_eq!(read(&dir, "polkit-1"), POLKIT_AFTER);
    }

    // -- the flag split -----------------------------------------------------

    /// The defect the split exists to fix: unattended and "allowed to edit
    /// system-auth" are different authorizations, and the first must never
    /// imply the second.
    #[test]
    fn no_confirm_does_not_unlock_a_sensitive_service() {
        for service in SENSITIVE_SERVICES {
            let dir = seeded(&[(service, SUDO_BEFORE)]);
            let before = snapshot(dir.path());

            let error = write_in(&only(dir.path()), &add(&[service]))
                .unwrap_err()
                .to_string();

            assert!(
                error.contains(&format!("Refusing to modify '{service}'")),
                "got: {error}"
            );
            assert!(
                error.contains("--allow-sensitive"),
                "the refusal must name the flag that unlocks it: {error}"
            );
            assert_eq!(before, snapshot(dir.path()));
        }
    }

    #[test]
    fn allow_sensitive_unlocks_it() {
        let dir = seeded(&[("sshd", SUDO_BEFORE)]);
        let request = PamRequest {
            allow_sensitive: true,
            ..add(&["sshd"])
        };

        assert_eq!(write_in(&only(dir.path()), &request).unwrap(), WRITE_OK);
        assert_eq!(read(&dir, "sshd"), SUDO_AFTER);
    }

    /// The setup alias uses `--yes` only for prompt suppression. A sensitive
    /// write stays refused until the alias's explicit authorization is set,
    /// and the refusal names that authorization rather than the prompt flag.
    #[test]
    fn setup_alias_requires_explicit_sensitive_authorization() {
        let dir = seeded(&[("system-auth", SUDO_BEFORE)]);
        let before = snapshot(dir.path());
        let prompt_only = PamRequest {
            no_confirm: true,
            ..add(&["system-auth"])
        };

        let error = install_one_in(&only(dir.path()), &prompt_only)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--allow-sensitive"), "got: {error}");
        assert_eq!(before, snapshot(dir.path()));

        let authorized = PamRequest {
            allow_sensitive: true,
            ..prompt_only
        };
        assert!(install_one_in(&only(dir.path()), &authorized).unwrap());
        assert_eq!(read(&dir, "system-auth"), SUDO_AFTER);
    }

    /// Removal is the safe direction, so it is not gated at all: a user who
    /// wired `system-auth` must be able to unwire it without arguing with the
    /// CLI about it.
    #[test]
    fn remove_is_never_gated_by_the_sensitive_list() {
        let dir = seeded(&[("system-auth", OMARCHY_PRESENT)]);

        assert_eq!(
            write_in(&only(dir.path()), &remove(&["system-auth"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(read(&dir, "system-auth"), OMARCHY_REMOVED);
    }

    // -- --if-present -------------------------------------------------------

    #[test]
    fn if_present_turns_a_missing_service_into_a_no_op_on_both_verbs() {
        for action in [PamAction::Add, PamAction::Remove] {
            let dir = tempfile::TempDir::new().unwrap();
            let request = PamRequest {
                action,
                services: vec!["omarchy-lock-face".to_string()],
                no_confirm: true,
                if_present: true,
                ..PamRequest::default()
            };

            assert_eq!(write_in(&only(dir.path()), &request).unwrap(), WRITE_OK);
            assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        }
    }

    #[test]
    fn a_missing_service_without_if_present_still_errors() {
        for request in [add(&["omarchy-lock-face"]), remove(&["omarchy-lock-face"])] {
            let dir = tempfile::TempDir::new().unwrap();

            let error = write_in(&only(dir.path()), &request)
                .unwrap_err()
                .to_string();

            assert!(
                error.contains("PAM service file not found:"),
                "got: {error}"
            );
            assert!(error.contains("omarchy-lock-face"), "got: {error}");
            assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        }
    }

    /// `--if-present` converts a missing file into success and nothing else.
    /// A directory where a service file should be is a read failure, not an
    /// absence, and stays fatal. (The old test used `.` for this; `.` is now
    /// rejected as a service name before any I/O, so the unreadable target has
    /// to be a validly-named one.)
    #[test]
    fn if_present_does_not_suppress_other_read_errors() {
        for request in [add(&["sudo"]), remove(&["sudo"])] {
            let dir = tempfile::TempDir::new().unwrap();
            fs::create_dir(dir.path().join("sudo")).unwrap();
            let request = PamRequest {
                if_present: true,
                ..request
            };

            let error = write_in(&only(dir.path()), &request)
                .unwrap_err()
                .to_string();

            assert!(error.contains("failed to read"), "got: {error}");
        }
    }

    // -- --dry-run ----------------------------------------------------------

    #[test]
    fn dry_run_writes_nothing_and_succeeds() {
        let dir = seeded(&[
            ("sudo", SUDO_BEFORE),
            ("omarchy-lock-face", OMARCHY_PRESENT),
        ]);
        let before = snapshot(dir.path());

        for action in [PamAction::Add, PamAction::Remove] {
            let request = PamRequest {
                action,
                services: vec!["sudo".to_string(), "omarchy-lock-face".to_string()],
                dry_run: true,
                no_confirm: true,
                ..PamRequest::default()
            };
            assert_eq!(write_in(&only(dir.path()), &request).unwrap(), WRITE_OK);
        }

        assert_eq!(before, snapshot(dir.path()));
    }

    #[test]
    fn dry_run_reports_the_action_it_would_take() {
        let dir = seeded(&[
            ("sudo", SUDO_BEFORE),
            ("omarchy-lock-face", OMARCHY_PRESENT),
        ]);
        let request = PamRequest {
            dry_run: true,
            json: true,
            ..add(&["sudo", "omarchy-lock-face"])
        };
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--x",
        };
        let targets = plan_writes(&only(dir.path()), &write).unwrap();

        let sink = Sink::verb(true);
        let words: Vec<&str> = targets
            .iter()
            .map(|t| report_plan(t, WriteAction::Add, &sink).word())
            .collect();

        assert_eq!(words, ["installed", "unchanged"]);
    }

    // -- status -------------------------------------------------------------

    #[test]
    fn status_exit_codes_follow_grep() {
        let dir = seeded(&[
            ("omarchy-lock-face", OMARCHY_PRESENT),
            ("sudo", SUDO_BEFORE),
        ]);
        let status = |services: &[&str]| {
            status_in(
                &only(dir.path()),
                &PamRequest {
                    action: PamAction::Status,
                    services: services.iter().map(|s| s.to_string()).collect(),
                    ..PamRequest::default()
                },
            )
        };

        assert_eq!(status(&["omarchy-lock-face"]), STATUS_PRESENT);
        assert_eq!(status(&["sudo"]), STATUS_MISSING);
        assert_eq!(status(&["not-a-service"]), STATUS_ERROR);
        assert_eq!(status(&["../escape"]), STATUS_ERROR, "usage error");
        // The worst outcome wins across services.
        assert_eq!(status(&["omarchy-lock-face", "sudo"]), STATUS_MISSING);
        assert_eq!(
            status(&["omarchy-lock-face", "not-a-service"]),
            STATUS_ERROR
        );
    }

    /// `status` never writes, whatever it is asked about.
    #[test]
    fn status_writes_nothing() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let before = snapshot(dir.path());

        status_in(
            &only(dir.path()),
            &PamRequest {
                action: PamAction::Status,
                services: vec!["sudo".into(), "absent".into(), "../escape".into()],
                ..PamRequest::default()
            },
        );

        assert_eq!(before, snapshot(dir.path()));
    }

    // -- JSON ---------------------------------------------------------------

    #[test]
    fn json_document_shape_is_the_contract() {
        let reports = [ServiceReport {
            service: "sudo".into(),
            path: Some("/etc/pam.d/sudo".into()),
            outcome: Outcome::Installed,
            backup: Some("/etc/pam.d/sudo.facelock-backup".into()),
            shadows: None,
        }];
        let value: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Add, false, &reports, &[])).unwrap();

        assert_eq!(value["command"], "add");
        assert_eq!(value["dry_run"], false);
        assert_eq!(value["services"][0]["service"], "sudo");
        assert_eq!(value["services"][0]["path"], "/etc/pam.d/sudo");
        assert_eq!(value["services"][0]["action"], "installed");
        assert_eq!(
            value["services"][0]["backup"],
            "/etc/pam.d/sudo.facelock-backup"
        );
        assert!(value["services"][0].get("error").is_none());
    }

    #[test]
    fn json_carries_the_error_only_when_there_is_one() {
        let reports = [
            ServiceReport {
                service: "sudo".into(),
                path: Some("/etc/pam.d/sudo".into()),
                outcome: Outcome::Failed("disk full".into()),
                backup: None,
                shadows: None,
            },
            ServiceReport {
                service: "polkit-1".into(),
                path: Some("/etc/pam.d/polkit-1".into()),
                outcome: Outcome::Unchanged,
                backup: None,
                shadows: None,
            },
        ];
        let value: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Add, false, &reports, &[])).unwrap();

        assert_eq!(value["services"][0]["action"], "failed");
        assert_eq!(value["services"][0]["error"], "disk full");
        assert_eq!(value["services"][0]["backup"], serde_json::Value::Null);
        assert!(value["services"][1].get("error").is_none());
    }

    /// **The `--json` `error` field must never carry localized text.** The
    /// human gets `PamInvalidServiceName` through the seam, on stderr, where
    /// gettext belongs; the machine field gets a fixed C-locale string. Pinned
    /// to that exact string, so re-routing `confined`'s localized error back
    /// into the row fails here instead of silently making a documented field
    /// change with `LC_MESSAGES`.
    #[test]
    fn a_rejected_name_reports_a_locale_independent_reason() {
        let dir = tempfile::TempDir::new().unwrap();
        let sink = Sink::verb(true);

        let reports = status_reports(&only(dir.path()), &["../escape".to_string()], &sink);

        assert_eq!(
            reports[0].outcome,
            Outcome::Unknown(INVALID_SERVICE_NAME.to_string())
        );
        assert_eq!(INVALID_SERVICE_NAME, "invalid service name");
        // ...and it is what lands in the document.
        let value: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Status, false, &reports, &[])).unwrap();
        assert_eq!(value["services"][0]["error"], "invalid service name");
        assert_eq!(value["services"][0]["action"], "unknown");
        // N3: a name that was rejected is never resolved, so nothing — not
        // even the backup probe — goes near the filesystem for it.
        assert_eq!(reports[0].backup, None);
    }

    /// E10: a service name reaches here from argv and confinement rejects `/`
    /// but not `"`, so the document has to be built by a serializer rather
    /// than by `format!`.
    #[test]
    fn json_escapes_a_service_name_containing_a_quote() {
        let reports = [ServiceReport {
            service: "a\"b".into(),
            path: Some("/etc/pam.d/a\"b".into()),
            outcome: Outcome::Missing,
            backup: None,
            shadows: None,
        }];
        let rendered = report_json(PamAction::Status, false, &reports, &[]);
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["services"][0]["service"], "a\"b");
    }

    /// Every word in the vocabulary is distinct and stable; this is the list
    /// `docs/contracts.md` documents.
    #[test]
    fn outcome_vocabulary_is_pinned() {
        let words: Vec<&str> = [
            Outcome::Installed,
            Outcome::Overridden,
            Outcome::VendorOnly,
            Outcome::Removed,
            Outcome::Unchanged,
            Outcome::Absent,
            Outcome::Declined,
            Outcome::Failed(String::new()),
            Outcome::CleanupFailed(String::new()),
            Outcome::Present,
            Outcome::Missing,
            Outcome::Unknown(String::new()),
        ]
        .iter()
        .map(Outcome::word)
        .collect();

        assert_eq!(
            words,
            [
                "installed",
                "overridden",
                "vendor-only",
                "removed",
                "unchanged",
                "absent",
                "declined",
                "failed",
                "cleanup-failed",
                "present",
                "missing",
                "unknown",
            ]
        );
    }

    // -- misc ---------------------------------------------------------------

    #[test]
    fn no_service_means_sudo_and_duplicates_collapse() {
        assert_eq!(requested_services(&[]), [DEFAULT_PAM_SERVICE]);
        assert_eq!(
            requested_services(&["sudo".into(), "polkit-1".into(), "sudo".into()]),
            ["sudo", "polkit-1"]
        );
    }

    #[test]
    fn facelock_line_detection_ignores_spacing_and_comments() {
        assert!(is_facelock_pam_line(PAM_LINE));
        assert!(is_facelock_pam_line("auth  sufficient  pam_facelock.so"));
        assert!(!is_facelock_pam_line("#auth sufficient pam_facelock.so"));
        assert!(!is_facelock_pam_line("auth include system-login"));
    }

    /// The shared auth stacks of every distribution this runs on, plus the two
    /// single doors. Gating only Arch's spelling of the shared stack made the
    /// gate a function of the operator's distribution, which is not a security
    /// boundary.
    #[test]
    fn sensitive_services_cover_every_distributions_shared_stack() {
        for shared in [
            "system-auth",
            "system-auth-ac",
            "password-auth",
            "password-auth-ac",
            "common-auth",
            "system-login",
        ] {
            assert!(
                SENSITIVE_SERVICES.contains(&shared),
                "{shared} is a shared auth stack and must be gated"
            );
        }
        assert!(SENSITIVE_SERVICES.contains(&"login"));
        assert!(SENSITIVE_SERVICES.contains(&"sshd"));
        assert!(!SENSITIVE_SERVICES.contains(&"sudo"));
        assert!(!SENSITIVE_SERVICES.contains(&"polkit-1"));
    }

    // -- the confirmation guard ---------------------------------------------

    /// dialoguer draws and reads the prompt through `Term::stderr()`, so a
    /// redirected stderr makes it unanswerable even with stdin on a TTY —
    /// which is what `sudo facelock pam add --service sudo 2>install.log` is,
    /// and it used to report `failed` having written nothing.
    #[test]
    fn a_prompt_is_asked_only_when_both_streams_are_a_terminal() {
        for (no_confirm, stdin_tty, stderr_tty, expected) in [
            (false, true, true, true),
            (false, true, false, false),
            (false, false, true, false),
            (false, false, false, false),
            // `--no-confirm` answers before either stream is consulted.
            (true, true, true, false),
            (true, true, false, false),
            (true, false, true, false),
            (true, false, false, false),
        ] {
            assert_eq!(
                should_prompt(no_confirm, stdin_tty, stderr_tty),
                expected,
                "no_confirm={no_confirm} stdin_tty={stdin_tty} stderr_tty={stderr_tty}"
            );
        }
    }

    /// Not being asked is not a licence to write into a gated service: the
    /// gate is decided in phase one, before a prompt exists to skip.
    #[test]
    fn skipping_the_prompt_never_unlocks_the_gate() {
        let dir = seeded(&[("system-auth", SUDO_BEFORE)]);
        let before = snapshot(dir.path());

        assert!(write_in(&only(dir.path()), &add(&["system-auth"])).is_err());
        assert_eq!(before, snapshot(dir.path()));
    }

    // -- what --quiet and --json may not silence -----------------------------

    /// The preview names the file, the line and the backup path; a "Proceed?"
    /// with that suppressed is a question with no subject. So whenever the
    /// question will be asked it leaves the sink `--quiet` can reach.
    #[test]
    fn a_prompt_that_will_be_asked_keeps_its_context() {
        // Nobody is being asked: ordinary progress, suppressible.
        assert_eq!(preview_route(false, false), Route::Info);
        assert_eq!(preview_route(true, false), Route::Dropped);

        // Being asked: stdout in human mode (same bytes a normal run always
        // printed, and `--quiet` does not reach `notice`), stderr under
        // `--json`, where the prompt itself is and where stdout is holding a
        // document.
        assert_eq!(preview_route(false, true), Route::Notice);
        assert_eq!(preview_route(true, true), Route::Error);
    }

    /// The rollback instructions are the one message that tells a locked-out
    /// operator how to get back in. `--quiet` must not take them; `--json`
    /// does, because the row's `backup` field is the same fact.
    #[test]
    fn the_rollback_advice_survives_quiet() {
        // `notice` is the unsuppressible stdout sink — see
        // `message::tests::quiet_silences_stdout_except_notice`, which pins
        // that `--quiet` does not reach it.
        assert_eq!(
            Sink::human().notice_route(),
            Route::Notice,
            "the rollback advice must not be silenceable by --quiet"
        );
        assert_eq!(Sink::verb(true).notice_route(), Route::Dropped);

        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        write_in(&dirs, &add(&["sudo"])).unwrap();
        // ...and the fact it carries is the one the JSON row carries.
        assert_eq!(
            latest_backup_bytes(&dirs, "sudo"),
            SUDO_BEFORE.as_bytes(),
            "the backup the advice names has to be the original file"
        );
    }

    // -- symlinked service files --------------------------------------------

    /// The authselect shape: `/etc/pam.d/system-auth` is a symlink into
    /// `/etc/authselect`. `read`/`copy`/`write` follow it, so without this the
    /// writer edits a generated file — which authselect regenerates away —
    /// and drops the backup beside the link rather than beside what changed.
    #[test]
    fn a_symlink_out_of_base_is_a_validation_failure() {
        for action in [PamAction::Add, PamAction::Remove] {
            let dir = tempfile::TempDir::new().unwrap();
            let base = dir.path().join("pam.d");
            fs::create_dir(&base).unwrap();
            let outside = dir.path().join("authselect-system-auth");
            fs::write(&outside, OMARCHY_PRESENT).unwrap();
            std::os::unix::fs::symlink(&outside, base.join("system-auth")).unwrap();

            let request = PamRequest {
                action,
                services: vec!["system-auth".to_string()],
                no_confirm: true,
                allow_sensitive: true,
                ..PamRequest::default()
            };
            let error = write_in(&only(&base), &request).unwrap_err().to_string();

            assert!(error.contains("it is a symlink to"), "got: {error}");
            assert!(
                error.contains(outside.to_str().unwrap()),
                "the refusal must name the target: {error}"
            );
            assert_eq!(
                fs::read_to_string(&outside).unwrap(),
                OMARCHY_PRESENT,
                "nothing outside the directory may be written"
            );
            assert!(
                !backup_path(&outside).exists(),
                "and nothing outside it may gain a backup"
            );
        }
    }

    /// `..` inside the *link* is the same escape as `..` inside the name, and
    /// `confined` cannot see it: the name is one component either way.
    #[test]
    fn a_symlink_traversing_out_of_base_is_rejected_too() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        fs::write(dir.path().join("shadow"), "root:!:1::::::\n").unwrap();
        std::os::unix::fs::symlink("../shadow", base.join("sudo")).unwrap();

        assert!(write_in(&only(&base), &add(&["sudo"])).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("shadow")).unwrap(),
            "root:!:1::::::\n"
        );
    }

    /// Even an in-directory symlink is refused: the write re-resolves the
    /// service basename with O_NOFOLLOW and never records a resolved target.
    #[test]
    fn a_symlink_inside_base_is_refused_without_following() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        std::os::unix::fs::symlink("sudo", dir.path().join("sudo-alias")).unwrap();

        assert!(write_in(&only(dir.path()), &add(&["sudo-alias"])).is_err());

        assert_eq!(read(&dir, "sudo"), SUDO_BEFORE, "the target is untouched");
        assert!(
            dir.path().join("sudo-alias").is_symlink(),
            "the link itself is untouched"
        );
    }

    /// A validation failure, so the two-phase rule covers it: a run naming a
    /// good service and an escaping one writes nothing at all.
    #[test]
    fn a_symlink_rejection_writes_nothing_for_the_whole_run() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let outside = dir.path().parent().unwrap().join("facelock-escape-target");
        fs::write(&outside, SUDO_BEFORE).unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("polkit-1")).unwrap();
        let before = snapshot(dir.path());

        assert!(write_in(&only(dir.path()), &add(&["sudo", "polkit-1"])).is_err());

        assert_eq!(before, snapshot(dir.path()));
        fs::remove_file(&outside).unwrap();
    }

    /// `status` has no all-or-nothing phase, so the same rejection is a row
    /// rather than an `Err` — with a fixed C-locale reason, like a rejected
    /// name, and exit 2.
    #[test]
    fn status_reports_an_escaping_symlink_as_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        let outside = dir.path().join("authselect-system-auth");
        fs::write(&outside, OMARCHY_PRESENT).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("system-auth")).unwrap();

        let sink = Sink::verb(true);
        let reports = status_reports(&only(&base), &["system-auth".to_string()], &sink);

        assert_eq!(
            reports[0].outcome,
            Outcome::Unknown(SYMLINKED_OUT_OF_DIR.to_string())
        );
        assert_eq!(SYMLINKED_OUT_OF_DIR, "symlinked outside /etc/pam.d");
        // The link is a real, confined path this did `lstat`, unlike a
        // rejected name — so it is reported rather than nulled.
        assert_eq!(
            reports[0].path,
            Some(base.join("system-auth").display().to_string())
        );
        assert_eq!(
            status_in(
                &only(&base),
                &PamRequest {
                    action: PamAction::Status,
                    services: vec!["system-auth".to_string()],
                    ..PamRequest::default()
                }
            ),
            STATUS_ERROR
        );
    }

    /// A second *hard* link cannot be followed and checked the way a symlink
    /// can: the link count says another name exists and not where it is, so an
    /// edit here changes a file this cannot name.
    /// The same rule, reached through an in-directory alias — which is how it
    /// was bypassable: the link count was checked on the entry as typed and
    /// not on the file the entry reached, exactly the hole `gate_sensitive`'s
    /// second call exists to close.
    ///
    /// The shape is the one `SENSITIVE_SERVICES`' own doc describes: RHEL's
    /// `authconfig` leaves `system-auth -> system-auth-ac`, and a dedupe pass
    /// (`jdupes -L`, a hard-link-preserving restore) links `password-auth-ac`
    /// onto the same inode. Under the atomic replace, `remove --service
    /// system-auth` would then report `removed` while `password-auth-ac` — the
    /// stack sshd reads on that distribution — kept the facelock line: a
    /// fail-open on the shared auth stack, reported as success, with no backup
    /// to show what happened.
    #[test]
    fn a_symlink_is_refused_before_its_target_link_count_is_considered() {
        for action in [PamAction::Add, PamAction::Remove] {
            let dir = tempfile::TempDir::new().unwrap();
            let base = dir.path().join("pam.d");
            fs::create_dir(&base).unwrap();
            let real = base.join("system-auth-ac");
            fs::write(&real, OMARCHY_PRESENT).unwrap();
            let sibling = base.join("password-auth-ac");
            fs::hard_link(&real, &sibling).unwrap();
            std::os::unix::fs::symlink("system-auth-ac", base.join("system-auth")).unwrap();
            let before = snapshot(&base);

            let request = PamRequest {
                action,
                services: vec!["system-auth".to_string()],
                no_confirm: true,
                allow_sensitive: true,
                ..PamRequest::default()
            };
            let error = write_in(&only(&base), &request).unwrap_err().to_string();

            assert!(error.contains("it is a symlink to"), "got: {error}");
            assert!(
                error.contains("system-auth-ac"),
                "the refusal names the link target without following it: {error}"
            );
            assert_eq!(
                before,
                snapshot(&base),
                "{action:?}: nothing may be written for the whole run"
            );
            assert_eq!(
                fs::read_to_string(&sibling).unwrap(),
                OMARCHY_PRESENT,
                "{action:?}: the other name still carries the facelock line, \
                 which is the fail-open this refusal prevents"
            );
        }

        // ...and `status`, which has no all-or-nothing phase, reports it as
        // the same `unknown` an entry it will not follow always got.
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        fs::write(base.join("real"), OMARCHY_PRESENT).unwrap();
        fs::hard_link(base.join("real"), base.join("second-name")).unwrap();
        std::os::unix::fs::symlink("real", base.join("alias")).unwrap();

        let sink = Sink::verb(true);
        let reports = status_reports(&only(&base), &["alias".to_string()], &sink);
        assert_eq!(
            reports[0].outcome,
            Outcome::Unknown(SYMLINKED_OUT_OF_DIR.to_string())
        );
        assert_eq!(status_code(&reports[0].outcome, false), STATUS_ERROR);
    }

    #[test]
    fn a_hard_linked_service_file_is_a_validation_failure() {
        for action in [PamAction::Add, PamAction::Remove] {
            let dir = tempfile::TempDir::new().unwrap();
            let base = dir.path().join("pam.d");
            fs::create_dir(&base).unwrap();
            let outside = dir.path().join("second-name");
            fs::write(&outside, OMARCHY_PRESENT).unwrap();
            fs::hard_link(&outside, base.join("sudo")).unwrap();

            let request = PamRequest {
                action,
                services: vec!["sudo".to_string()],
                no_confirm: true,
                ..PamRequest::default()
            };
            let error = write_in(&only(&base), &request).unwrap_err().to_string();

            assert!(error.contains("names for the same file"), "got: {error}");
            assert_eq!(
                fs::read_to_string(&outside).unwrap(),
                OMARCHY_PRESENT,
                "the file behind the other name must be untouched"
            );
            assert!(!backup_path(&base.join("sudo")).exists());
        }
    }

    /// One name is the normal case and must stay quiet — the check is
    /// `nlink > 1`, not "is a link".
    #[test]
    fn a_single_linked_service_file_is_untouched_by_the_rule() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        assert_eq!(
            write_in(&only(dir.path()), &add(&["sudo"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(read(&dir, "sudo"), SUDO_AFTER);
    }

    /// `status` answers with a row rather than an `Err`, with the fixed
    /// C-locale reason and exit 2.
    #[test]
    fn status_reports_a_hard_linked_service_as_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        fs::write(dir.path().join("second-name"), OMARCHY_PRESENT).unwrap();
        fs::hard_link(dir.path().join("second-name"), base.join("sudo")).unwrap();

        let sink = Sink::verb(true);
        let reports = status_reports(&only(&base), &["sudo".to_string()], &sink);

        assert_eq!(
            reports[0].outcome,
            Outcome::Unknown(HARD_LINKED.to_string())
        );
        assert_eq!(HARD_LINKED, "hard-linked service file");
        assert_eq!(
            status_in(
                &only(&base),
                &PamRequest {
                    action: PamAction::Status,
                    services: vec!["sudo".to_string()],
                    ..PamRequest::default()
                }
            ),
            STATUS_ERROR
        );
    }

    // -- the gate follows the file, not the name -----------------------------

    /// A symlink is rejected before the sensitive gate can turn it into an
    /// alternate name for a shared stack.
    #[test]
    fn a_symlink_cannot_alias_a_sensitive_service() {
        let make = || {
            let dir = seeded(&[("system-auth", SUDO_BEFORE)]);
            std::os::unix::fs::symlink("system-auth", dir.path().join("alias")).unwrap();
            dir
        };

        let refused = make();
        let before = snapshot(refused.path());
        let error = write_in(&only(refused.path()), &add(&["alias"]))
            .unwrap_err()
            .to_string();

        assert!(error.contains("it is a symlink to"), "got: {error}");
        assert_eq!(before, snapshot(refused.path()), "and writes nothing");

        // The sensitive opt-in cannot unlock filesystem indirection.
        let allowed = make();
        let request = PamRequest {
            allow_sensitive: true,
            ..add(&["alias"])
        };
        assert!(write_in(&only(allowed.path()), &request).is_err());
        assert_eq!(read(&allowed, "system-auth"), SUDO_BEFORE);
    }

    /// Removing does not weaken the no-follow filesystem boundary.
    #[test]
    fn remove_refuses_a_symlinked_service_too() {
        let dir = seeded(&[("system-auth", OMARCHY_PRESENT)]);
        std::os::unix::fs::symlink("system-auth", dir.path().join("alias")).unwrap();

        assert!(write_in(&only(dir.path()), &remove(&["alias"])).is_err());
        assert_eq!(read(&dir, "system-auth"), OMARCHY_PRESENT);
    }

    // -- status --if-present -------------------------------------------------

    /// The pair that could not be written before: install the optional
    /// integrations with `--if-present`, then verify with the same flag. Only
    /// absence is converted — an unreadable file or a bad name is still 2.
    #[test]
    fn status_if_present_stops_an_absent_service_forcing_exit_2() {
        let dir = seeded(&[
            ("omarchy-lock-face", OMARCHY_PRESENT),
            ("sudo", SUDO_BEFORE),
        ]);
        let status = |services: &[&str], if_present| {
            status_in(
                &only(dir.path()),
                &PamRequest {
                    action: PamAction::Status,
                    services: services.iter().map(|s| s.to_string()).collect(),
                    if_present,
                    ..PamRequest::default()
                },
            )
        };

        assert_eq!(status(&["not-a-service"], false), STATUS_ERROR);
        assert_eq!(status(&["not-a-service"], true), STATUS_PRESENT);
        // The services that do exist still decide the answer.
        assert_eq!(
            status(&["omarchy-lock-face", "not-a-service"], true),
            STATUS_PRESENT
        );
        assert_eq!(status(&["sudo", "not-a-service"], true), STATUS_MISSING);
        // ...and it converts absence only.
        assert_eq!(status(&["../escape"], true), STATUS_ERROR);
    }

    /// The alias honours `--if-present` on `add`, and forgives absence only.
    ///
    /// The verb's own coverage is `if_present_turns_a_missing_service_into_a_no_op_on_both_verbs`
    /// above; this is the alias entry point, which is where the bool used to
    /// stop. `install_one_in` rather than `install_for_setup` because the
    /// latter refuses non-root before it reaches phase one — the two differ by
    /// that check and the module check, and hand `plan_writes` the same
    /// request.
    #[test]
    fn the_alias_honours_if_present_on_add() {
        let dir = tempfile::TempDir::new().unwrap();
        let request = PamRequest {
            no_confirm: true,
            if_present: true,
            ..add(&["omarchy-lock-face"])
        };
        assert!(
            !install_one_in(&only(dir.path()), &request).unwrap(),
            "an absent service configured nothing, so it is not a configured service"
        );
        assert!(
            fs::read_dir(dir.path()).unwrap().next().is_none(),
            "an absent service must leave the directory empty"
        );

        // The positive control: the same call on a service that is there
        // answers `true`, so the bool tracks the file and not the flag.
        let real = seeded(&[("omarchy-lock-face", OMARCHY_REMOVED)]);
        assert!(install_one_in(&only(real.path()), &request).unwrap());
        assert_eq!(read(&real, "omarchy-lock-face"), OMARCHY_PRESENT);

        // Without the flag the same call is a failure, so the default still
        // catches a typo'd `--service`.
        let error = install_one_in(&only(dir.path()), &add(&["omarchy-lock-face"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("PAM service file not found:"),
            "got: {error}"
        );

        // ...and the flag converts absence and nothing else: an unreadable
        // target is still fatal through the alias, exactly as through the verb.
        fs::create_dir(dir.path().join("sudo")).unwrap();
        let error = install_one_in(
            &only(dir.path()),
            &PamRequest {
                no_confirm: true,
                if_present: true,
                allow_sensitive: true,
                ..add(&["sudo"])
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("failed to read"), "got: {error}");
    }

    /// The decision table behind `install_one_in`'s bool, every word given a
    /// verdict on purpose.
    ///
    /// `Declined` is reachable no other way from a test: it needs a terminal
    /// answering "no" at the per-file confirmation. It is the second word that
    /// is a success having configured nothing, so the wizard's summary and its
    /// hyprlock handoff must treat it the way they treat `absent`.
    #[test]
    fn only_the_add_words_that_leave_a_line_count_as_configured() {
        for outcome in [Outcome::Installed, Outcome::Overridden, Outcome::Unchanged] {
            assert!(add_left_the_line(&outcome), "{}", outcome.word());
        }
        for outcome in [
            Outcome::Absent,
            Outcome::Declined,
            Outcome::Removed,
            Outcome::VendorOnly,
            Outcome::Failed("e".to_string()),
            Outcome::CleanupFailed("e".to_string()),
            Outcome::Present,
            Outcome::Missing,
            Outcome::Unknown("e".to_string()),
        ] {
            assert!(!add_left_the_line(&outcome), "{}", outcome.word());
        }
    }

    /// `install_for_setup` edits `/etc/pam.d` and `run_with_plan` reaches it
    /// directly for a standalone `--pam`, so it must refuse non-root itself
    /// (C6: before any other check and any output). Regression: routing
    /// standalone `--pam` through it once let an unprivileged `facelock setup
    /// --pam` read and report on `/etc/pam.d/sudo`.
    #[test]
    fn setup_alias_refuses_without_root() {
        if nix::unistd::Uid::current().is_root() {
            return; // the check cannot fire; nothing to assert
        }
        for error in [
            install_for_setup(&PamRequest {
                allow_sensitive: true,
                ..add(&["sudo"])
            })
            .unwrap_err(),
            remove_for_setup(&PamRequest {
                if_present: true,
                ..remove(&["sudo"])
            })
            .unwrap_err(),
        ] {
            let error = error.to_string();
            assert!(
                error.contains("Root required"),
                "expected the root refusal before any other check, got: {error}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Vendor directories (P1)
    //
    // The bug these exist for: on current Arch `polkit` ships
    // /usr/lib/pam.d/polkit-1 and /etc/pam.d/polkit-1 does not exist, so a
    // writer that only ever looked in /etc/pam.d could not configure the
    // service at all. Every case below is driven against a tempdir *pair*,
    // which is what the search path being a parameter is for.
    // -----------------------------------------------------------------------

    /// An `/etc` directory and a vendor directory, in that order.
    fn pair() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::TempDir::new().unwrap();
        let etc = root.path().join("etc");
        let vendor = root.path().join("vendor");
        fs::create_dir(&etc).unwrap();
        fs::create_dir(&vendor).unwrap();
        (root, etc, vendor)
    }

    fn both(etc: &Path, vendor: &Path) -> PamDirs {
        PamDirs {
            dirs: vec![etc.to_path_buf(), vendor.to_path_buf()],
            backup_dir: etc.join(".facelock-pam-backups"),
        }
    }

    fn header_lines(content: &str) -> usize {
        content
            .lines()
            .filter(|line| line.starts_with("# Copied from "))
            .count()
    }

    /// The headline case. The vendor file is read, an `/etc` override is
    /// created from it with the line in place, and the package's own file is
    /// byte-identical afterwards — asserted on the bytes, because the failure
    /// this guards against is a successful-looking run that edited `/usr`.
    #[test]
    fn a_vendor_only_service_gets_an_etc_override() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let before = snapshot(&vendor);

        let sink = Sink::verb(false);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &add(&["polkit-1"]),
            remedy: "--allow-sensitive",
        };
        let dirs = both(&etc, &vendor);
        let targets = plan_writes(&dirs, &write).unwrap();
        let reports = apply_all(&dirs, &targets, &write, &sink);

        assert_eq!(reports[0].outcome, Outcome::Overridden);
        assert_eq!(
            reports[0].path.as_deref(),
            Some(etc.join("polkit-1").to_str().unwrap()),
            "the row is about the file that was created, not the one that was read"
        );
        assert_eq!(reports[0].backup, None, "a copy has nothing to back up");

        let written = fs::read_to_string(etc.join("polkit-1")).unwrap();
        assert_eq!(header_lines(&written), 1);
        assert!(
            written.ends_with(POLKIT_AFTER),
            "below the header the bytes are what an in-place edit would have \
             produced: {written}"
        );
        assert_eq!(before, snapshot(&vendor), "the vendor file is untouched");
    }

    /// Second `add`: the override now exists, so the service resolves in
    /// `/etc` and is edited in place — no copy, no second header, and the
    /// vendor file still untouched.
    #[test]
    fn a_second_add_edits_the_override_and_writes_no_second_header() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let before = snapshot(&vendor);

        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let first = fs::read_to_string(etc.join("polkit-1")).unwrap();
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let second = fs::read_to_string(etc.join("polkit-1")).unwrap();

        assert_eq!(first, second, "the second add is a no-op");
        assert_eq!(header_lines(&second), 1);
        assert_eq!(before, snapshot(&vendor));

        // ...and a service that was in `/etc` all along never gains a header
        // at all.
        let (_root2, etc2, vendor2) = pair();
        fs::write(etc2.join("sudo"), SUDO_BEFORE).unwrap();
        fs::write(vendor2.join("sudo"), POLKIT_BEFORE).unwrap();
        write_in(&both(&etc2, &vendor2), &add(&["sudo"])).unwrap();

        assert_eq!(
            fs::read_to_string(etc2.join("sudo")).unwrap(),
            SUDO_AFTER,
            "an /etc entry shadows the vendor one and is edited byte for byte"
        );
        assert_eq!(
            fs::read_to_string(vendor2.join("sudo")).unwrap(),
            POLKIT_BEFORE
        );
    }

    #[test]
    fn remove_deletes_an_unchanged_facelock_created_vendor_override() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let vendor_before = snapshot(&vendor);

        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        assert!(etc.join("polkit-1").exists());

        assert_eq!(write_in(&dirs, &remove(&["polkit-1"])).unwrap(), WRITE_OK);
        assert!(
            !etc.join("polkit-1").exists(),
            "removing the line must also retire Facelock's unchanged local copy"
        );
        assert_eq!(snapshot(&vendor), vendor_before);
    }

    #[test]
    fn vendor_override_header_parser_requires_the_exact_bounded_shape() {
        let vendor = Path::new("/usr/lib/pam.d/polkit-1");
        let payload = b"#%PAM-1.0\nauth required pam_unix.so\n";
        let valid = [
            b"# Copied from /usr/lib/pam.d/polkit-1 by facelock 0.1.4 on 2026-08-20.\n".as_slice(),
            VENDOR_OVERRIDE_HEADER_SUFFIX,
            payload,
        ]
        .concat();
        assert_eq!(
            vendor_override_payload(&valid, vendor),
            Some(payload.as_slice())
        );

        let valid_text = String::from_utf8(valid).unwrap();
        for invalid in [
            valid_text.replacen("/usr/lib/pam.d/polkit-1", "/tmp/polkit-1", 1),
            valid_text.replacen("2026-08-20", "2026/08/20", 1),
            valid_text.replacen("facelock 0.1.4", "facelock 0.1/4", 1),
            valid_text.replacen("will not track", "might not track", 1),
        ] {
            assert_eq!(vendor_override_payload(invalid.as_bytes(), vendor), None);
        }
    }

    #[test]
    fn remove_all_deletes_an_unchanged_facelock_created_vendor_override() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        let vendor_before = snapshot(&vendor);

        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);

        assert!(
            !etc.join("polkit-1").exists(),
            "package-safe cleanup must use the same vendor override retirement"
        );
        assert_eq!(snapshot(&vendor), vendor_before);
    }

    #[test]
    fn remove_all_finds_a_writer_accepted_nonconventional_vendor_override() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join(".custom"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);

        assert_eq!(write_in(&dirs, &add(&[".custom"])).unwrap(), WRITE_OK);
        assert!(etc.join(".custom").exists());

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
        assert!(!etc.join(".custom").exists());
        assert_eq!(
            fs::read_to_string(vendor.join(".custom")).unwrap(),
            POLKIT_BEFORE
        );
    }

    #[test]
    fn remove_all_finishes_deleting_an_exact_override_whose_line_is_already_absent() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let intermediate = with_line_removed(&fs::read(&path).unwrap());
        fs::write(&path, intermediate).unwrap();

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
        assert!(
            !path.exists(),
            "batch recovery must discover the exact no-line intermediate"
        );
    }

    #[test]
    fn remove_all_preserves_a_content_drifted_vendor_override_as_a_blocker() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let override_path = etc.join("polkit-1");
        let mut drifted = fs::read(&override_path).unwrap();
        drifted.extend_from_slice(b"# administrator customization\n");
        fs::write(&override_path, drifted).unwrap();
        let before = snapshot(&etc);

        let error = remove_all(&dirs).unwrap_err().to_string();

        assert!(error.contains("administrator"), "got: {error}");
        assert_eq!(snapshot(&etc), before, "preflight must preserve the file");
    }

    #[test]
    fn remove_all_preserves_a_metadata_drifted_vendor_override_as_a_blocker() {
        use std::os::unix::fs::PermissionsExt;

        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let override_path = etc.join("polkit-1");
        fs::set_permissions(&override_path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = snapshot(&etc);

        let error = remove_all(&dirs).unwrap_err().to_string();

        assert!(error.contains("administrator"), "got: {error}");
        assert_eq!(snapshot(&etc), before, "preflight must preserve the file");
        assert_eq!(fs::metadata(override_path).unwrap().mode() & 0o7777, 0o600);
    }

    #[test]
    fn remove_all_vendor_deletion_is_restartable_after_each_committed_unlink() {
        let (_root, etc, vendor) = pair();
        for service in ["alpha", "beta"] {
            fs::write(vendor.join(service), POLKIT_BEFORE).unwrap();
        }
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["alpha", "beta"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::AfterOverrideDelete(0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "crash after committed vendor override unlink",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("crash after committed"));
        assert_eq!(
            [etc.join("alpha").exists(), etc.join("beta").exists()]
                .into_iter()
                .filter(|exists| *exists)
                .count(),
            1
        );

        recover_remove_all_in(&dirs).unwrap();

        assert!(!etc.join("alpha").exists());
        assert!(!etc.join("beta").exists());
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn remove_all_vendor_deletion_recovers_both_batch_state_unlink_boundaries() {
        for crash_at in [
            RemoveAllPoint::JournalUnlinked,
            RemoveAllPoint::CommitUnlinked,
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
            let request = PamRequest {
                action: PamAction::Remove,
                all: true,
                no_confirm: true,
                ..PamRequest::default()
            };

            let error = remove_all_in_with_hook(&dirs, &request, |point| {
                if point == crash_at {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "crash at remove-all state unlink boundary",
                    ));
                }
                Ok(())
            })
            .unwrap_err();
            assert!(
                error.to_string().contains("state unlink"),
                "{crash_at:?}: {error}"
            );
            assert!(!etc.join("polkit-1").exists(), "{crash_at:?}");
            assert!(!fs::read_dir(&etc).unwrap().flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".facelock-vendor-retire-")
            }));

            let store = BackupStore::open_existing(dirs.backup_dir())
                .unwrap()
                .unwrap();
            let (journal, commit) = load_remove_all_state(&store).unwrap();
            match crash_at {
                RemoveAllPoint::JournalUnlinked => {
                    assert!(journal.is_none());
                    assert!(commit.is_some());
                }
                RemoveAllPoint::CommitUnlinked => {
                    assert!(journal.is_none());
                    assert!(commit.is_none());
                }
                _ => unreachable!(),
            }

            recover_remove_all_in(&dirs).unwrap();
            assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
        }
    }

    #[test]
    fn remove_all_rolls_back_vendor_overrides_before_the_commit_marker() {
        let (_root, etc, vendor) = pair();
        for service in ["alpha", "beta"] {
            fs::write(vendor.join(service), POLKIT_BEFORE).unwrap();
        }
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["alpha", "beta"])).unwrap(), WRITE_OK);
        let before = snapshot(&etc);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::AfterMutation(0) {
                return Err(std::io::Error::other("later remove-all mutation failed"));
            }
            Ok(())
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("later remove-all mutation failed")
        );
        assert_eq!(snapshot(&etc), before);
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn named_vendor_override_retirement_preserves_a_canonical_final_gap_replacement() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        assert!(
            matches!(target.plan, Plan::DeleteOverride { .. }),
            "{target:?}"
        );
        let path = etc.join("polkit-1");
        let administrator = b"# replacement during vendor retirement\n";

        let outcome = apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| {
            if point == VendorRetirePoint::BeforeFinalValidation {
                fs::write(&path, administrator)?;
            }
            Ok(())
        });

        assert!(matches!(outcome, Outcome::Failed(_)), "got: {outcome:?}");
        assert_eq!(fs::read(&path).unwrap(), administrator);
        assert!(
            fs::read_dir(&etc).unwrap().flatten().any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-vendor-retire-")),
            "the authenticated displaced override remains classified by durable evidence"
        );
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);
        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(&path).unwrap(), administrator);
        assert!(fs::read_dir(&etc).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-vendor-retire-")
        }));
    }

    #[test]
    fn named_vendor_retirement_retains_evidence_when_the_root_cannot_be_reopened() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc-pam.d");
        let vendor = root.path().join("vendor-pam.d");
        let state = root.path().join("state");
        fs::create_dir(&etc).unwrap();
        fs::create_dir(&vendor).unwrap();
        let dirs = PamDirs {
            dirs: vec![etc.clone(), vendor.clone()],
            backup_dir: state.clone(),
        };
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);

        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let Plan::DeleteOverride { content } = &target.plan else {
            panic!("expected vendor override deletion plan: {target:?}");
        };
        let installed = with_line_removed(content);
        let expected = target.identity.as_ref().unwrap();
        let store = BackupStore::open(&state).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let mutation = transaction
            .plan_mutation(&target.service, content, &installed)
            .unwrap();
        let operation = mutation.operation.clone();
        let held_root = root.path().join("held-etc-pam.d");

        let error = transaction
            .remove_pam_with_intent_and_published_hook(
                &mutation,
                &target.path,
                expected,
                &installed,
                |installed_identity| {
                    fs::rename(&etc, &held_root)?;
                    fs::write(&etc, b"not a directory\n")?;
                    retire_vendor_override_with_hook(
                        &dirs,
                        &target.service,
                        &operation,
                        installed_identity,
                        |_| Ok(()),
                    )
                },
            )
            .unwrap_err();

        fs::remove_file(&etc).unwrap();
        fs::rename(&held_root, &etc).unwrap();
        assert!(is_ambiguous_publication(&error), "got: {error}");
        assert!(
            state
                .join(intent_name(IntentRole::PamRemove, &operation))
                .exists()
        );
        assert!(
            state
                .join(publication_name(PublicationRole::PamRemove, &operation))
                .exists()
        );
    }

    #[test]
    fn named_vendor_override_retirement_restores_when_vendor_drifts_in_the_final_gap() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let path = etc.join("polkit-1");

        let outcome = apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| {
            if point == VendorRetirePoint::BeforeFinalValidation {
                fs::write(
                    vendor.join("polkit-1"),
                    b"# package update\nauth required pam_unix.so\n",
                )?;
            }
            Ok(())
        });

        assert!(matches!(outcome, Outcome::Failed(_)), "got: {outcome:?}");
        assert!(
            path.exists(),
            "the local override is restored without overwrite"
        );
        assert!(!PamDocument::new(&fs::read(path).unwrap()).has_facelock_rule());
    }

    #[test]
    fn named_vendor_override_retirement_recovers_every_quarantine_boundary() {
        for crash_at in [
            VendorRetirePoint::Quarantined,
            VendorRetirePoint::BeforeFinalValidation,
            VendorRetirePoint::Unlinked,
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
            let request = remove(&["polkit-1"]);
            let write = WriteRequest {
                action: WriteAction::Remove,
                request: &request,
                remedy: "--allow-sensitive",
            };
            let target = plan_writes(&dirs, &write).unwrap().remove(0);

            let outcome = apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| {
                if point == crash_at {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "crash at named vendor-retirement boundary",
                    ));
                }
                Ok(())
            });
            assert!(matches!(outcome, Outcome::Failed(_)), "{crash_at:?}");

            let store = BackupStore::open_existing(dirs.backup_dir())
                .unwrap()
                .unwrap();
            store
                .recover(&dirs)
                .unwrap_or_else(|error| panic!("{crash_at:?}: {error}"));

            assert!(!etc.join("polkit-1").exists(), "{crash_at:?}");
            assert_eq!(
                fs::read_dir(dirs.backup_dir()).unwrap().count(),
                0,
                "{crash_at:?}"
            );
        }
    }

    #[test]
    fn vendor_quarantine_sync_failure_retains_evidence_and_recovers() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);

        install_rename_noreplace_sync_test_hook(|source, destination| {
            if source == "polkit-1" && destination.starts_with(".facelock-vendor-retire-") {
                return Err(std::io::Error::other(
                    "injected quarantine directory sync failure",
                ));
            }
            Ok(())
        });
        let result = write_in(&dirs, &remove(&["polkit-1"]));
        clear_rename_noreplace_sync_test_hook();

        assert_eq!(result.unwrap(), WRITE_FAILED);
        assert!(!etc.join("polkit-1").exists());
        assert!(fs::read_dir(&etc).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-vendor-retire-")
        }));
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);

        BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap()
            .recover(&dirs)
            .unwrap();
        assert!(!etc.join("polkit-1").exists());
        assert!(!fs::read_dir(&etc).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-vendor-retire-")
        }));
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn vendor_restore_sync_failure_retains_evidence_and_recovers() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);

        install_rename_noreplace_sync_test_hook(|source, destination| {
            if source.starts_with(".facelock-vendor-retire-") && destination == "polkit-1" {
                return Err(std::io::Error::other(
                    "injected restore directory sync failure",
                ));
            }
            Ok(())
        });
        let outcome = apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| {
            if point == VendorRetirePoint::BeforeFinalValidation {
                fs::write(
                    vendor.join("polkit-1"),
                    b"# package update\nauth required pam_unix.so\n",
                )?;
            }
            Ok(())
        });
        clear_rename_noreplace_sync_test_hook();

        assert!(matches!(outcome, Outcome::Failed(_)), "got: {outcome:?}");
        assert!(etc.join("polkit-1").exists());
        assert!(!fs::read_dir(&etc).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-vendor-retire-")
        }));
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);

        BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap()
            .recover(&dirs)
            .unwrap();
        assert!(etc.join("polkit-1").exists());
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn named_vendor_retirement_holds_the_state_lock_through_quarantine_cleanup() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = RefCell::new(None);

        let outcome = apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| {
            if point == VendorRetirePoint::BeforeFinalValidation {
                let competing_dirs = dirs.clone();
                let attempted_tx = attempted_tx.clone();
                let acquired_tx = acquired_tx.clone();
                *worker.borrow_mut() = Some(std::thread::spawn(move || {
                    let store = BackupStore::open(competing_dirs.backup_dir()).unwrap();
                    attempted_tx.send(()).unwrap();
                    let transaction = store.transaction(&competing_dirs).unwrap();
                    acquired_tx.send(()).unwrap();
                    drop(transaction);
                }));
                attempted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                assert!(
                    acquired_rx
                        .recv_timeout(Duration::from_millis(100))
                        .is_err(),
                    "competing recovery acquired the lock during vendor quarantine cleanup"
                );
            }
            Ok(())
        });

        assert_eq!(outcome, Outcome::Removed);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.into_inner().unwrap().join().unwrap();
    }

    #[test]
    fn named_vendor_retirement_recovers_after_the_restore_boundary() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);

        let outcome =
            apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| match point {
                VendorRetirePoint::BeforeFinalValidation => fs::write(
                    vendor.join("polkit-1"),
                    b"# package update\nauth required pam_unix.so\n",
                ),
                VendorRetirePoint::Restored => Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "crash after vendor-override restore",
                )),
                _ => Ok(()),
            });
        assert!(matches!(outcome, Outcome::Failed(_)));
        let restored = fs::read(etc.join("polkit-1")).unwrap();
        assert!(!PamDocument::new(&restored).has_facelock_rule());

        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        store.recover(&dirs).unwrap();
        assert_eq!(fs::read(etc.join("polkit-1")).unwrap(), restored);
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn batch_vendor_retirement_recovers_after_the_restore_boundary() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| match point {
            RemoveAllPoint::BeforeOverrideFinalValidation(0) => fs::write(
                vendor.join("polkit-1"),
                b"# package update\nauth required pam_unix.so\n",
            ),
            RemoveAllPoint::OverrideRestored(0) => Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "crash after batch vendor-override restore",
            )),
            _ => Ok(()),
        })
        .unwrap_err();
        assert!(error.to_string().contains("crash after batch"));
        assert!(etc.join("polkit-1").exists());
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);

        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        recover_remove_all_in(&dirs).unwrap();
        assert!(!etc.join("polkit-1").exists());
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn remove_all_rechecks_a_committed_vendor_override_before_unlink() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let holding = etc.join("facelock-committed");
        let administrator = b"# administrator replacement\n";
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::BeforeOverrideDelete(0) {
                fs::rename(&path, &holding)?;
                fs::write(&path, administrator)?;
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "got: {error}");
        assert_eq!(fs::read(&path).unwrap(), administrator);
        assert!(holding.exists());
        assert!(
            fs::read_dir(dirs.backup_dir()).unwrap().count() > 0,
            "the durable commit remains for explicit recovery"
        );
    }

    #[test]
    fn remove_all_vendor_retirement_preserves_a_canonical_final_gap_replacement() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let administrator = b"# replacement during batch retirement\n";
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::BeforeOverrideFinalValidation(0) {
                fs::write(&path, administrator)?;
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("ambiguous"), "got: {error}");
        assert_eq!(fs::read(&path).unwrap(), administrator);
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);
        assert!(recover_remove_all_in(&dirs).is_err());
        assert_eq!(fs::read(&path).unwrap(), administrator);
        assert!(fs::read_dir(&etc).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-vendor-retire-")
        }));
    }

    #[test]
    fn remove_all_vendor_retirement_restores_when_vendor_drifts_in_the_final_gap() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::BeforeOverrideFinalValidation(0) {
                fs::write(
                    vendor.join("polkit-1"),
                    b"# package update\nauth required pam_unix.so\n",
                )?;
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("vendor"), "got: {error}");
        let restored = fs::read(etc.join("polkit-1")).unwrap();
        assert!(!PamDocument::new(&restored).has_facelock_rule());
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);
    }

    #[test]
    fn remove_all_vendor_retirement_recovers_every_quarantine_boundary() {
        for crash_at in [
            RemoveAllPoint::OverrideQuarantined(0),
            RemoveAllPoint::BeforeOverrideFinalValidation(0),
            RemoveAllPoint::AfterOverrideDelete(0),
        ] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
            let request = PamRequest {
                action: PamAction::Remove,
                all: true,
                no_confirm: true,
                ..PamRequest::default()
            };

            let error = remove_all_in_with_hook(&dirs, &request, |point| {
                if point == crash_at {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "crash at batch vendor-retirement boundary",
                    ));
                }
                Ok(())
            })
            .unwrap_err();
            assert!(
                error.to_string().contains("crash at batch"),
                "{crash_at:?}: {error}"
            );

            recover_remove_all_in(&dirs).unwrap_or_else(|error| panic!("{crash_at:?}: {error}"));

            assert!(!etc.join("polkit-1").exists(), "{crash_at:?}");
            assert_eq!(
                fs::read_dir(dirs.backup_dir()).unwrap().count(),
                0,
                "{crash_at:?}"
            );
        }
    }

    #[test]
    fn remove_all_does_not_substitute_a_different_later_root_for_the_vendor_source() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        let vendor = root.path().join("vendor");
        let detection_only = root.path().join("detection-only");
        for directory in [&etc, &vendor, &detection_only] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = PamDirs {
            dirs: vec![etc.clone(), vendor.clone(), detection_only.clone()],
            backup_dir: root.path().join("state"),
        };
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::BeforeOverrideDelete(0) {
                fs::remove_file(vendor.join("polkit-1"))?;
                fs::write(detection_only.join("polkit-1"), POLKIT_BEFORE)?;
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("vendor"), "got: {error}");
        assert!(etc.join("polkit-1").exists());
        assert!(detection_only.join("polkit-1").exists());
    }

    #[test]
    fn named_vendor_retirement_rejects_a_new_higher_priority_vendor_entry() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        let higher = root.path().join("higher");
        let lower = root.path().join("lower");
        for directory in [&etc, &higher, &lower] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(lower.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = PamDirs {
            dirs: vec![etc.clone(), higher.clone(), lower.clone()],
            backup_dir: root.path().join("state"),
        };
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);

        let outcome = apply_remove_with_vendor_hook(&target, &Sink::silent(), &dirs, |point| {
            if point == VendorRetirePoint::BeforeFinalValidation {
                fs::write(higher.join("polkit-1"), POLKIT_BEFORE)?;
            }
            Ok(())
        });

        assert!(matches!(outcome, Outcome::Failed(_)), "got: {outcome:?}");
        assert!(etc.join("polkit-1").exists());
        assert!(higher.join("polkit-1").exists());
    }

    #[test]
    fn batch_vendor_validation_stops_at_a_new_higher_priority_entry() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        let higher = root.path().join("higher");
        let lower = root.path().join("lower");
        for directory in [&etc, &higher, &lower] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(lower.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = PamDirs {
            dirs: vec![etc.clone(), higher.clone(), lower.clone()],
            backup_dir: root.path().join("state"),
        };
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::CommitMarked {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "inspect committed batch",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        let (journal, _) = load_remove_all_state(&store).unwrap();
        let journal = journal.unwrap();
        fs::write(higher.join("polkit-1"), POLKIT_BEFORE).unwrap();

        assert!(!journal_vendor_service_matches(&store, &dirs, &journal.value.targets[0]).unwrap());
    }

    #[test]
    fn batch_vendor_validation_rejects_backup_substitution_after_prepared_validation() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::CommitMarked {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "inspect committed batch",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        let (journal, _) = load_remove_all_state(&store).unwrap();
        let journal = journal.unwrap();
        let target = &journal.value.targets[0];
        let backup_path = dirs.backup_dir().join(&target.backup);
        let displaced = dirs.backup_dir().join("held-backup");

        let error = journal_vendor_service_matches_with_hook(&store, &dirs, target, || {
            let bytes = fs::read(&backup_path)?;
            fs::rename(&backup_path, &displaced)?;
            fs::write(&backup_path, bytes)?;
            fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))?;
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("backup changed"), "got: {error}");
        assert!(backup_path.exists());
        assert!(displaced.exists());
    }

    #[test]
    fn batch_vendor_validation_rejects_unemitted_rule_shapes_and_hash_mismatch() {
        for case in ["duplicate", "custom", "installed-hash"] {
            let (_root, etc, vendor) = pair();
            fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
            let dirs = both(&etc, &vendor);
            assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
            let exact = fs::read(etc.join("polkit-1")).unwrap();
            let marker = exact
                .windows(PAM_LINE.len())
                .position(|window| window == PAM_LINE.as_bytes())
                .unwrap();
            let original = match case {
                "duplicate" => {
                    let mut bytes = exact.clone();
                    bytes.splice(marker..marker, format!("{PAM_LINE}\n").bytes());
                    bytes
                }
                "custom" => {
                    let mut bytes = exact.clone();
                    bytes.splice(
                        marker..marker + PAM_LINE.len(),
                        b"auth required pam_facelock.so debug".iter().copied(),
                    );
                    bytes
                }
                "installed-hash" => exact.clone(),
                _ => unreachable!(),
            };
            let installed = with_line_removed(&original);
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let prepared = transaction.plan("polkit-1", &original, &installed).unwrap();
            transaction.persist(&prepared, &original).unwrap();
            let vendor_metadata = fs::metadata(vendor.join("polkit-1")).unwrap();
            let target = RemoveAllJournalTarget {
                service: "polkit-1".to_owned(),
                backup: prepared.backup.clone(),
                original: FileIdentity {
                    device: 1,
                    inode: 2,
                    links: 1,
                    sha256: sha256_hex(&original),
                    mode: vendor_metadata.mode(),
                    uid: vendor_metadata.uid(),
                    gid: vendor_metadata.gid(),
                },
                installed_sha256: if case == "installed-hash" {
                    sha256_hex(b"different installed bytes")
                } else {
                    sha256_hex(&installed)
                },
                delete_override: Some(true),
            };
            if case == "installed-hash" {
                // The prepared record is otherwise strict and internally
                // consistent with the forged journal value.
                let record_path = dirs.backup_dir().join(format!("{}.json", prepared.backup));
                let mut record: ProvenanceRecord =
                    serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
                record.installed_sha256 = target.installed_sha256.clone();
                let encoded = serde_json::to_vec_pretty(&record).unwrap();
                fs::write(&record_path, encoded).unwrap();
                fs::set_permissions(&record_path, fs::Permissions::from_mode(0o600)).unwrap();
            }

            assert!(
                !journal_vendor_service_matches(&store, &dirs, &target).unwrap_or(false),
                "{case}"
            );
        }
    }

    #[test]
    fn remove_all_preserves_the_override_when_the_vendor_bytes_drift_before_unlink() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::BeforeOverrideDelete(0) {
                fs::write(
                    vendor.join("polkit-1"),
                    b"# vendor update\nauth required pam_unix.so\n",
                )?;
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("vendor"), "got: {error}");
        assert!(etc.join("polkit-1").exists());
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().count() > 0);
    }

    #[test]
    fn remove_all_v2_requires_the_delete_override_field_but_v1_forbids_it() {
        let identity = FileIdentity {
            device: 1,
            inode: 2,
            links: 1,
            sha256: sha256_hex(b"original"),
            mode: libc::S_IFREG | 0o644,
            uid: 0,
            gid: 0,
        };
        let target = serde_json::json!({
            "service": "sudo",
            "backup": "sudo.1-000000001",
            "original": identity,
            "installed_sha256": sha256_hex(b"installed")
        });
        let document = |version, target| {
            serde_json::json!({
                "version": version,
                "operation": "1-000000001",
                "keep_backup": false,
                "targets": [target]
            })
        };

        let v2_missing: RemoveAllJournal =
            serde_json::from_value(document(REMOVE_ALL_VERSION, target.clone())).unwrap();
        assert!(!valid_remove_all_journal(&v2_missing));

        let mut v2_target = target.clone();
        v2_target["delete_override"] = serde_json::Value::Bool(true);
        let v2: RemoveAllJournal =
            serde_json::from_value(document(REMOVE_ALL_VERSION, v2_target)).unwrap();
        assert!(valid_remove_all_journal(&v2));

        let v1: RemoveAllJournal =
            serde_json::from_value(document(REMOVE_ALL_LEGACY_VERSION, target.clone())).unwrap();
        assert!(valid_remove_all_journal(&v1));

        let mut v1_target = target;
        v1_target["delete_override"] = serde_json::Value::Bool(false);
        let v1_with_v2_field: RemoveAllJournal =
            serde_json::from_value(document(REMOVE_ALL_LEGACY_VERSION, v1_target)).unwrap();
        assert!(!valid_remove_all_journal(&v1_with_v2_field));

        let mut null_target = serde_json::to_value(&v2.targets[0]).unwrap();
        null_target["delete_override"] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<RemoveAllJournal>(document(REMOVE_ALL_VERSION, null_target,))
                .is_err(),
            "the strict v2 boolean must not accept null as an absent field"
        );
    }

    #[test]
    fn vendor_override_messages_make_delete_and_drift_explicit() {
        let removed = PamMessage::PamVendorOverrideRemoved {
            path: "/etc/pam.d/polkit-1".to_owned(),
            vendor: "/usr/lib/pam.d/polkit-1".to_owned(),
        }
        .localized();
        let retained = PamMessage::PamVendorOverrideRetained {
            path: "/etc/pam.d/polkit-1".to_owned(),
            vendor: "/usr/lib/pam.d/polkit-1".to_owned(),
        }
        .localized();

        assert!(removed.contains("Deleted unchanged local override"));
        assert!(removed.contains("/usr/lib/pam.d/polkit-1"));
        assert!(retained.contains("Kept local override"));
        assert!(retained.contains("administrator or vendor drift"));
    }

    #[test]
    fn malformed_vendor_header_selects_the_explicit_retained_message() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let mut modified = fs::read(&path).unwrap();
        modified[0] = b'!';
        fs::write(&path, &modified).unwrap();
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let Plan::Rewrite { content } = &target.plan else {
            panic!("malformed provenance must be retained, got {target:?}");
        };
        let disposition =
            classify_vendor_override(&dirs, &target, content, target.identity.as_ref().unwrap());

        assert_eq!(disposition, VendorOverrideDisposition::Drifted);
        assert!(matches!(
            remove_success_message(&target, target.path_string(), disposition),
            PamMessage::PamVendorOverrideRetained { .. }
        ));
    }

    #[test]
    fn named_remove_reports_an_absent_configured_vendor_source_after_removing_the_line() {
        let (_root, etc, vendor) = pair();
        let vendor_path = vendor.join("polkit-1");
        fs::write(&vendor_path, POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        fs::remove_file(&vendor_path).unwrap();

        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let Plan::Rewrite { content } = &target.plan else {
            panic!("missing vendor source must retain the override: {target:?}");
        };
        let disposition =
            classify_vendor_override(&dirs, &target, content, target.identity.as_ref().unwrap());
        assert_eq!(
            disposition,
            VendorOverrideDisposition::SourceAbsent(vendor_path.clone())
        );
        assert!(matches!(
            remove_success_message(&target, target.path_string(), disposition),
            PamMessage::PamVendorOverrideSourceAbsent { vendor, .. }
                if vendor == vendor_path.display().to_string()
        ));

        assert_eq!(
            apply_remove(&target, &Sink::silent(), &dirs),
            Outcome::Removed
        );
        let retained = fs::read(etc.join("polkit-1")).unwrap();
        assert!(!PamDocument::new(&retained).has_facelock_rule());
    }

    #[test]
    fn named_remove_reports_an_absent_configured_vendor_source_for_a_no_line_restart() {
        let (_root, etc, vendor) = pair();
        let vendor_path = vendor.join("polkit-1");
        fs::write(&vendor_path, POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let local = etc.join("polkit-1");
        let restart = with_line_removed(&fs::read(&local).unwrap());
        fs::write(&local, &restart).unwrap();
        fs::remove_file(&vendor_path).unwrap();

        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        assert!(matches!(
            &target.plan,
            Plan::RetainVendorOverride { vendor }
                if vendor == &vendor_path
        ));
        assert_eq!(
            apply_remove(&target, &Sink::silent(), &dirs),
            Outcome::Unchanged
        );
        assert_eq!(fs::read(&local).unwrap(), restart);
        assert!(
            PamMessage::PamVendorOverrideSourceAbsentNoLine {
                path: local.display().to_string(),
                vendor: vendor_path.display().to_string(),
            }
            .localized()
            .contains("vendor source is absent")
        );
    }

    #[test]
    fn absent_vendor_source_recognition_rejects_an_unconfigured_header_path() {
        let (_root, etc, vendor) = pair();
        let vendor_path = vendor.join("polkit-1");
        fs::write(&vendor_path, POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let local = etc.join("polkit-1");
        let modified = String::from_utf8(fs::read(&local).unwrap())
            .unwrap()
            .replacen(&vendor_path.display().to_string(), "/tmp/admin-source", 1);
        fs::write(&local, modified).unwrap();
        fs::remove_file(&vendor_path).unwrap();

        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let target = plan_writes(&dirs, &write).unwrap().remove(0);
        let Plan::Rewrite { content } = &target.plan else {
            panic!("unconfigured header path must not own the local override");
        };
        assert_eq!(
            classify_vendor_override(&dirs, &target, content, target.identity.as_ref().unwrap(),),
            VendorOverrideDisposition::NotFacelock
        );
    }

    #[test]
    fn oversized_local_vendor_override_is_rejected_without_rewrite() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_BACKUP_BYTES as u64 + 1).unwrap();
        drop(file);
        let before = fs::metadata(&path).unwrap();

        let error = write_in(&dirs, &remove(&["polkit-1"])).unwrap_err();

        assert!(error.to_string().contains("failed to read"), "got: {error}");
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (after.dev(), after.ino(), after.len()),
            (before.dev(), before.ino(), before.len())
        );
    }

    #[test]
    fn oversized_vendor_and_batch_backup_block_cleanup_and_preserve_evidence() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let vendor_file = fs::OpenOptions::new()
            .write(true)
            .open(vendor.join("polkit-1"))
            .unwrap();
        vendor_file.set_len(MAX_BACKUP_BYTES as u64 + 1).unwrap();
        drop(vendor_file);
        let before = snapshot(&etc);
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        assert!(remove_all_in(&dirs, &request).is_err());
        assert_eq!(snapshot(&etc), before);

        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::CommitMarked {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "inspect committed batch",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        let (journal, _) = load_remove_all_state(&store).unwrap();
        let backup = dirs
            .backup_dir()
            .join(&journal.as_ref().unwrap().value.targets[0].backup);
        let backup_file = fs::OpenOptions::new().write(true).open(&backup).unwrap();
        backup_file.set_len(MAX_BACKUP_BYTES as u64 + 1).unwrap();
        drop(backup_file);
        let backup_before = fs::metadata(&backup).unwrap();
        let journal_name = journal.as_ref().unwrap().name.clone();

        assert!(recover_remove_all_in(&dirs).is_err());
        let backup_after = fs::metadata(&backup).unwrap();
        assert_eq!(
            (backup_after.dev(), backup_after.ino(), backup_after.len()),
            (
                backup_before.dev(),
                backup_before.ino(),
                backup_before.len()
            )
        );
        assert!(dirs.backup_dir().join(journal_name).exists());
        assert!(
            fs::read_dir(dirs.backup_dir())
                .unwrap()
                .flatten()
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".facelock-remove-all-commit-"))
        );
    }

    #[test]
    fn a_deleted_vendor_override_report_no_longer_claims_to_shadow_it() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let request = remove(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Remove,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let targets = plan_writes(&dirs, &write).unwrap();

        let reports = apply_all(&dirs, &targets, &write, &Sink::silent());

        assert_eq!(reports[0].outcome, Outcome::Removed);
        assert_eq!(reports[0].shadows, None);
        assert!(!etc.join("polkit-1").exists());
    }

    #[test]
    fn named_remove_keeps_content_drift_but_removes_the_module_rule() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let mut drifted = fs::read(&path).unwrap();
        drifted.extend_from_slice(b"# administrator customization\n");
        fs::write(&path, drifted).unwrap();

        assert_eq!(write_in(&dirs, &remove(&["polkit-1"])).unwrap(), WRITE_OK);

        let retained = fs::read(&path).unwrap();
        assert!(!PamDocument::new(&retained).has_facelock_rule());
        assert!(retained.ends_with(b"# administrator customization\n"));
    }

    #[test]
    fn named_remove_preserves_a_vendor_override_with_an_extra_module_rule() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let mut drifted = fs::read(&path).unwrap();
        drifted.extend_from_slice(PAM_LINE.as_bytes());
        drifted.push(b'\n');
        fs::write(&path, drifted).unwrap();

        assert_eq!(write_in(&dirs, &remove(&["polkit-1"])).unwrap(), WRITE_OK);

        let retained = fs::read(&path).unwrap();
        assert!(!PamDocument::new(&retained).has_facelock_rule());
        assert!(vendor_override_payload(&retained, &vendor.join("polkit-1")).is_some());
    }

    #[test]
    fn remove_finishes_deleting_an_exact_override_whose_line_is_already_absent() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let path = etc.join("polkit-1");
        let intermediate = with_line_removed(&fs::read(&path).unwrap());
        fs::write(&path, intermediate).unwrap();

        assert_eq!(write_in(&dirs, &remove(&["polkit-1"])).unwrap(), WRITE_OK);
        assert!(!path.exists());
    }

    #[test]
    fn dry_run_previews_vendor_override_deletion_without_writing() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        assert_eq!(write_in(&dirs, &add(&["polkit-1"])).unwrap(), WRITE_OK);
        let before = snapshot(&etc);
        let request = PamRequest {
            dry_run: true,
            ..remove(&["polkit-1"])
        };

        assert_eq!(write_in(&dirs, &request).unwrap(), WRITE_OK);
        assert_eq!(snapshot(&etc), before);
    }

    /// `remove` resolves the same way so it can *report* a vendor-only
    /// service, and then writes nothing: exit 0, no `/etc` file invented, the
    /// package's file untouched.
    #[test]
    fn remove_on_a_vendor_only_service_is_a_no_op() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), OMARCHY_PRESENT).unwrap();
        let before = snapshot(&vendor);

        let code = write_in(&both(&etc, &vendor), &remove(&["polkit-1"])).unwrap();

        assert_eq!(code, WRITE_OK);
        assert!(snapshot(&etc).is_empty(), "removal creates nothing");
        assert_eq!(
            before,
            snapshot(&vendor),
            "not even a vendor file that carries the line is edited"
        );
    }

    /// The not-found refusal names **every** path tried. Naming only
    /// `/etc/pam.d` would send an operator to create a file that a vendor
    /// directory may already hold.
    #[test]
    fn an_absent_service_names_every_directory_searched() {
        let (_root, etc, vendor) = pair();

        let error = write_in(&both(&etc, &vendor), &add(&["polkit-1"]))
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("PAM service file not found:"),
            "got: {error}"
        );
        for dir in [&etc, &vendor] {
            let path = dir.join("polkit-1");
            assert!(
                error.contains(path.to_str().unwrap()),
                "the refusal must name {}: {error}",
                path.display()
            );
        }
    }

    /// `status` on a vendor-only service reports it as itself — not `absent`,
    /// which would be false about the file, and not `missing`, which would be
    /// misleading about what `add` is going to do. Exit 1: the service exists
    /// and does not carry the line.
    #[test]
    fn status_reports_a_vendor_only_service_as_vendor_only() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();

        let sink = Sink::verb(true);
        let reports = status_reports(&both(&etc, &vendor), &["polkit-1".to_string()], &sink);

        assert_eq!(reports[0].outcome, Outcome::VendorOnly);
        assert_eq!(reports[0].outcome.word(), "vendor-only");
        assert_eq!(
            reports[0].path.as_deref(),
            Some(vendor.join("polkit-1").to_str().unwrap()),
            "the row names the file that exists"
        );
        assert_eq!(status_code(&reports[0].outcome, false), STATUS_MISSING);
        // `--if-present` converts absence and nothing else; this is not one.
        assert_eq!(status_code(&reports[0].outcome, true), STATUS_MISSING);
    }

    /// ...unless the vendor file already carries the line, which is a
    /// distribution shipping face auth in its own PAM stack. That machine *is*
    /// configured, and reporting `vendor-only` would send an integrator off to
    /// create an override that adds nothing.
    #[test]
    fn a_vendor_file_that_carries_the_line_is_present() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("omarchy-lock-face"), OMARCHY_PRESENT).unwrap();
        let dirs = both(&etc, &vendor);

        let sink = Sink::verb(true);
        let reports = status_reports(&dirs, &["omarchy-lock-face".to_string()], &sink);
        assert_eq!(reports[0].outcome, Outcome::Present);
        assert!(is_configured(&dirs, "omarchy-lock-face"));

        // ...and `add` writes no override for it, because there is nothing an
        // override would add.
        assert_eq!(
            write_in(&dirs, &add(&["omarchy-lock-face"])).unwrap(),
            WRITE_OK
        );
        assert!(snapshot(&etc).is_empty());
    }

    /// First hit wins, and only the first directory is ever written to.
    #[test]
    fn the_first_directory_holding_the_name_answers() {
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_BEFORE).unwrap();
        fs::write(vendor.join("sudo"), OMARCHY_PRESENT).unwrap();
        let dirs = both(&etc, &vendor);

        let target = Target::locate(&dirs, "sudo").unwrap();

        assert_eq!(target.path, etc.join("sudo"));
        assert_eq!(target.origin, Origin::Local);
        assert_eq!(target.write_path(), etc.join("sudo"));
    }

    /// The multi-directory restatement of #194's symlink rule: the target must
    /// be under the directory the *entry* was found in, so an `/etc` entry
    /// pointing into a vendor directory is out of base and refused. Decided
    /// deliberately, and written down in contracts.md: following it would put
    /// an edit in a package-owned file, which is the one thing this must never
    /// do.
    #[test]
    fn an_etc_entry_symlinked_into_a_vendor_directory_is_refused() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        std::os::unix::fs::symlink(vendor.join("polkit-1"), etc.join("polkit-1")).unwrap();
        let before = snapshot(&vendor);

        let error = write_in(&both(&etc, &vendor), &add(&["polkit-1"]))
            .unwrap_err()
            .to_string();

        assert!(error.contains("it is a symlink to"), "got: {error}");
        assert!(
            error.contains(etc.to_str().unwrap()),
            "the refusal names the directory that was violated: {error}"
        );
        assert_eq!(before, snapshot(&vendor));
        assert!(!backup_path(&vendor.join("polkit-1")).exists());
    }

    /// A vendor symlink is refused rather than used as an alias for a shared
    /// stack, while the shared stack's own name still reaches the gate.
    #[test]
    fn the_sensitive_gate_reaches_a_vendor_file() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("system-auth"), SUDO_BEFORE).unwrap();
        std::os::unix::fs::symlink("system-auth", vendor.join("harmless")).unwrap();
        let dirs = both(&etc, &vendor);

        for service in ["system-auth", "harmless"] {
            let error = write_in(&dirs, &add(&[service])).unwrap_err().to_string();
            if service == "system-auth" {
                assert!(
                    error.contains("sensitive PAM service"),
                    "{service}: got {error}"
                );
            } else {
                assert!(
                    error.contains("it is a symlink to"),
                    "{service}: got {error}"
                );
            }
        }
        assert!(snapshot(&etc).is_empty(), "a refusal writes nothing");
    }

    /// `--dry-run` previews the copy and writes nothing — including the
    /// override it says it would create.
    #[test]
    fn dry_run_on_a_vendor_only_service_writes_nothing() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let before = snapshot(&vendor);

        let request = PamRequest {
            dry_run: true,
            ..add(&["polkit-1"])
        };
        let sink = Sink::verb(false);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let dirs = both(&etc, &vendor);
        let targets = plan_writes(&dirs, &write).unwrap();
        let reports = apply_all(&dirs, &targets, &write, &sink);

        assert_eq!(reports[0].outcome, Outcome::Overridden);
        assert!(snapshot(&etc).is_empty());
        assert_eq!(before, snapshot(&vendor));
    }

    /// The wizard's menu asks the resolver, so a candidate that ships only in
    /// a vendor directory is offered — which is the half of this bug that
    /// would otherwise have kept `polkit-1` out of `setup`'s multi-select on
    /// exactly the machines where `pam add` had just started working.
    #[test]
    fn a_vendor_only_candidate_is_visible_to_the_wizard() {
        let (_root, etc, vendor) = pair();
        let dirs = both(&etc, &vendor);

        assert!(!service_exists(&dirs, "polkit-1"));
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        assert!(service_exists(&dirs, "polkit-1"));

        // An entry the writer would refuse is not offered: the menu must not
        // propose a service that then fails.
        std::os::unix::fs::symlink(vendor.join("polkit-1"), etc.join("hyprlock")).unwrap();
        assert!(!service_exists(&dirs, "hyprlock"));
    }

    // -- the atomic replace -------------------------------------------------

    /// Mode and owner are carried across, because the replace writes a *new*
    /// inode: a service file that came back 0644 when it was 0640, or owned by
    /// the wrong group, is a permissions regression on the file that decides
    /// whether the machine can be logged into.
    #[test]
    fn an_in_place_edit_keeps_the_files_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        let path = dir.path().join("sudo");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        write_in(&only(dir.path()), &add(&["sudo"])).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(read(&dir, "sudo"), SUDO_AFTER);
    }

    /// A vendor copy takes the vendor file's mode: it is the only provenance
    /// there is for a file that did not exist a moment ago.
    #[test]
    fn a_vendor_copy_takes_the_vendor_files_mode() {
        use std::os::unix::fs::PermissionsExt;

        let (_root, etc, vendor) = pair();
        let source = vendor.join("polkit-1");
        fs::write(&source, POLKIT_BEFORE).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();

        write_in(&both(&etc, &vendor), &add(&["polkit-1"])).unwrap();

        assert_eq!(
            fs::metadata(etc.join("polkit-1"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    /// A failed replace leaves no debris. The temp file is what a reader of
    /// `/etc/pam.d` would otherwise find sitting next to the service files.
    #[test]
    fn a_failed_replace_removes_its_temp_file() {
        let dir = tempfile::TempDir::new().unwrap();
        // A directory where the destination file should be: the rename cannot
        // land, so the write fails after the temp file has been created.
        fs::create_dir(dir.path().join("sudo")).unwrap();

        assert!(replace_atomically(&dir.path().join("sudo"), b"x\n", dir.path()).is_err());

        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "sudo")
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    /// A successful run leaves none either — the assertion the goldens cannot
    /// make, since `snapshot` compares two runs and a temp file present in
    /// both would cancel out.
    #[test]
    fn a_successful_write_leaves_no_temp_file() {
        let dir = seeded(&[("sudo", SUDO_BEFORE)]);
        write_in(&only(dir.path()), &add(&["sudo"])).unwrap();

        let names: Vec<String> = snapshot(dir.path()).into_keys().collect();
        assert_eq!(names, ["sudo"]);
    }

    /// An override that cannot be created is refused in **phase one**, so a
    /// run naming a good service and a vendor-only one on a read-only `/etc`
    /// writes nothing at all.
    #[test]
    fn an_unwritable_override_directory_is_a_phase_one_refusal() {
        use std::os::unix::fs::PermissionsExt;

        // Root ignores the mode bits, so the check this asserts cannot fail
        // for root and the test would be vacuous.
        if nix::unistd::Uid::effective().is_root() {
            return;
        }

        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_BEFORE).unwrap();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        fs::set_permissions(&etc, fs::Permissions::from_mode(0o555)).unwrap();
        let before = snapshot(&etc);

        let error = write_in(&both(&etc, &vendor), &add(&["sudo", "polkit-1"]))
            .unwrap_err()
            .to_string();

        assert!(error.contains("not writable"), "got: {error}");
        assert_eq!(before, snapshot(&etc), "phase one writes nothing at all");
        fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// An `overridden` row never reports a backup, even when a
    /// `.facelock-backup` is lying at the override path from an earlier run:
    /// the copy preserved nothing, so that file is not this run's rollback and
    /// offering it would promise a restore of a file this did not touch.
    #[test]
    fn an_override_reports_no_backup_even_when_a_stale_one_exists() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        // What an earlier add-then-`rm`-the-override leaves behind.
        fs::write(etc.join("polkit-1.facelock-backup"), SUDO_BEFORE).unwrap();

        let sink = Sink::verb(false);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &add(&["polkit-1"]),
            remedy: "--allow-sensitive",
        };
        let dirs = both(&etc, &vendor);
        let targets = plan_writes(&dirs, &write).unwrap();
        let reports = apply_all(&dirs, &targets, &write, &sink);

        assert_eq!(reports[0].outcome, Outcome::Overridden);
        assert_eq!(reports[0].backup, None);
        assert_eq!(
            fs::read_to_string(etc.join("polkit-1.facelock-backup")).unwrap(),
            SUDO_BEFORE,
            "and the stale file is left exactly as it was found"
        );
    }

    /// `status` answers "where did you look?" the way `add` does. The row's
    /// machine `path` stays the single first-directory path, which is where an
    /// override would go.
    #[test]
    fn status_on_an_absent_service_names_every_directory_searched() {
        let (_root, etc, vendor) = pair();
        let dirs = both(&etc, &vendor);

        let target = Target::locate(&dirs, "polkit-1").unwrap();
        let tried = target.tried_paths();

        for dir in [&etc, &vendor] {
            let path = dir.join("polkit-1");
            assert!(
                tried.contains(path.to_str().unwrap()),
                "{} unnamed: {tried}",
                path.display()
            );
        }

        let sink = Sink::verb(true);
        let reports = status_reports(&dirs, &["polkit-1".to_string()], &sink);
        assert_eq!(reports[0].outcome, Outcome::Absent);
        assert_eq!(
            reports[0].path.as_deref(),
            Some(etc.join("polkit-1").to_str().unwrap()),
            "the machine field is one path: where an override would go"
        );
    }

    /// A relative `config_dirs` entry poisons the whole list. A relative first
    /// entry would resolve the write target against the invoking shell's
    /// working directory, so `cd /tmp && sudo facelock pam add` would edit
    /// `/tmp/<dir>/sudo` and report it as though it were `/etc/pam.d/sudo`.
    #[test]
    fn a_relative_config_dir_falls_back_to_the_default_list() {
        for list in [
            vec![PathBuf::from("pam.d")],
            // ...and a relative entry anywhere poisons it, not just first: a
            // list with a hole in it is not the search order anyone wrote.
            vec![PathBuf::from("/etc/pam.d"), PathBuf::from("vendor/pam.d")],
        ] {
            assert_eq!(
                PamDirs::new(list.clone()),
                PamDirs::default(),
                "{list:?} must not be used as a search path"
            );
        }

        assert_eq!(
            PamDirs::new(vec![PathBuf::from("/a"), PathBuf::from("/b")]),
            PamDirs::new(vec![PathBuf::from("/a"), PathBuf::from("/b")]),
            "an absolute list is taken as written"
        );
        assert_eq!(
            PamDirs::new(vec![PathBuf::from("/a")]).overrides(),
            Path::new("/a")
        );
    }

    /// Add no longer touches the adjacent legacy name, including when it is a
    /// symlink; the dedicated state backup is confined elsewhere.
    #[test]
    fn a_symlinked_legacy_backup_is_ignored_without_following() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        fs::write(base.join("sudo"), SUDO_BEFORE).unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "SECRET-CONTENT\n").unwrap();
        std::os::unix::fs::symlink(&victim, backup_path(&base.join("sudo"))).unwrap();

        assert_eq!(write_in(&only(&base), &add(&["sudo"])).unwrap(), WRITE_OK);

        assert_eq!(
            fs::read_to_string(&victim).unwrap(),
            "SECRET-CONTENT\n",
            "the file the planted link pointed at must not be written"
        );
        let backup = backup_path(&base.join("sudo"));
        assert!(backup.is_symlink(), "add leaves the legacy name untouched");
        assert_eq!(
            latest_backup_bytes(&only(&base), "sudo"),
            SUDO_BEFORE.as_bytes()
        );
        assert_eq!(fs::read_to_string(base.join("sudo")).unwrap(), SUDO_AFTER);
    }

    /// An entry that exists but cannot be examined is **not** an absence. It
    /// stops the search where it is, so the read that follows reports the
    /// error — the answer the single-directory writer gave for the same input.
    /// Falling through instead made `status` report `vendor-only` for a
    /// service the unreadable override configures: an honest failure turned
    /// into a confident wrong answer, and on `add` it would have renamed a
    /// copy of the vendor file over the override without taking a backup.
    #[test]
    fn an_unreadable_first_directory_is_not_an_absence() {
        use std::os::unix::fs::PermissionsExt;

        // Root traverses a 0000 directory, so the stat would succeed and the
        // assertion would be vacuous.
        if nix::unistd::Uid::effective().is_root() {
            return;
        }

        let (_root, etc, vendor) = pair();
        fs::write(etc.join("polkit-1"), OMARCHY_PRESENT).unwrap();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        let dirs = both(&etc, &vendor);
        fs::set_permissions(&etc, fs::Permissions::from_mode(0o000)).unwrap();

        let sink = Sink::verb(true);
        let reports = status_reports(&dirs, &["polkit-1".to_string()], &sink);
        let outcome = reports[0].outcome.clone();
        let path = reports[0].path.clone();
        // Restore before asserting, so a failure does not leave a 0000
        // directory behind for the tempdir teardown to trip over.
        fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            matches!(outcome, Outcome::Unknown(_)),
            "expected an honest non-answer, got {outcome:?}"
        );
        assert_eq!(status_code(&outcome, false), STATUS_ERROR);
        assert_eq!(
            path.as_deref(),
            Some(etc.join("polkit-1").to_str().unwrap()),
            "and it is about the entry it could not examine, not the vendor copy"
        );
    }

    /// The override directory may not also be one of the read-only ones,
    /// however it is spelled: that would make "never write to a vendor
    /// directory" false without anyone editing this module.
    #[test]
    fn an_override_directory_that_aliases_a_search_directory_is_rejected() {
        let (_root, etc, vendor) = pair();
        let link = etc.parent().unwrap().join("etclink");
        std::os::unix::fs::symlink(&vendor, &link).unwrap();

        for list in [
            // Spelled twice.
            vec![etc.clone(), vendor.clone(), etc.clone()],
            // ...and reached through a symlink, which is the same directory.
            vec![link.clone(), vendor.clone()],
        ] {
            assert_eq!(
                PamDirs::new(list.clone()),
                PamDirs::default(),
                "{list:?} must not be used as a search path"
            );
        }

        // A distinct pair is still taken as written.
        assert_eq!(
            PamDirs::new(vec![etc.clone(), vendor.clone()]).overrides(),
            etc.as_path()
        );
    }

    /// The search path is a value with an invariant: there is always a
    /// directory to write to, and it is the first one.
    #[test]
    fn the_write_target_is_the_first_directory() {
        assert_eq!(PamDirs::default().overrides(), Path::new(PAM_DIR));
        assert_eq!(
            PamDirs::default(),
            PamDirs::new(Vec::new()),
            "an empty list is a mistake, not a request to disable the writer"
        );
        assert_eq!(
            PamDirs::default().iter().collect::<Vec<&Path>>(),
            [Path::new("/etc/pam.d"), Path::new("/usr/lib/pam.d")],
            "Linux-PAM's own precedence: /etc first, the vendor directory second"
        );
    }

    #[test]
    fn remove_all_uses_compiled_system_roots_not_configured_search_dirs() {
        let configured = PamDirs::new(vec![PathBuf::from("/srv/custom-pam")]);
        let cleanup = PamDirs::system_cleanup();

        assert_ne!(cleanup, configured);
        assert_eq!(cleanup.overrides(), Path::new(PAM_DIR));
        assert_eq!(
            cleanup.iter().collect::<Vec<_>>(),
            [
                Path::new("/etc/pam.d"),
                Path::new("/usr/lib/pam.d"),
                Path::new("/etc/authselect")
            ]
        );
        assert_eq!(cleanup.backup_dir(), Path::new(PAM_BACKUPS_DIR));
    }

    // -----------------------------------------------------------------------
    // The module probe (P1b, #170)
    // -----------------------------------------------------------------------

    /// First hit wins, and the *order* is the contract: `/lib/security` stays
    /// first so the answer on usrmerged Arch does not change.
    #[test]
    fn the_module_probe_takes_the_first_candidate_that_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let candidates: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|name| dir.path().join(name).display().to_string())
            .collect();
        let list: Vec<&str> = candidates.iter().map(String::as_str).collect();

        assert_eq!(first_existing(&list), None, "a miss is None, not a guess");

        fs::write(dir.path().join("b"), "").unwrap();
        fs::write(dir.path().join("c"), "").unwrap();
        assert_eq!(
            first_existing(&list),
            Some(dir.path().join("b")),
            "the earlier of two hits wins"
        );

        fs::write(dir.path().join("a"), "").unwrap();
        assert_eq!(first_existing(&list), Some(dir.path().join("a")));
    }

    /// The regression that matters: the module installed **only** at the last
    /// candidate. `dist/facelock.spec` puts it at `%{_libdir}/security`, which
    /// is `/usr/lib64/security` on x86-64 Fedora and RHEL, and the old single
    /// path made `pam add` refuse on the distribution this repository ships a
    /// spec file for.
    #[test]
    fn a_module_at_the_last_candidate_is_still_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let candidates: Vec<String> = ["lib", "usr-lib", "usr-lib64"]
            .iter()
            .map(|name| dir.path().join(name).display().to_string())
            .collect();
        let list: Vec<&str> = candidates.iter().map(String::as_str).collect();
        fs::write(dir.path().join("usr-lib64"), "").unwrap();

        assert_eq!(first_existing(&list), Some(dir.path().join("usr-lib64")));
    }

    /// The refusal names every candidate, so an operator on an unlisted layout
    /// can see what to add rather than guess which single path was wanted.
    #[test]
    fn the_module_refusal_names_every_candidate() {
        let message = PamMessage::PamModuleNotInstalled {
            paths: PAM_MODULE_PATHS.join(", "),
            path: PAM_MODULE_PATHS[0].to_string(),
        }
        .localized();

        for candidate in PAM_MODULE_PATHS {
            assert!(
                message.contains(candidate),
                "{candidate} unnamed: {message}"
            );
        }
    }

    /// The probe order, pinned. `/lib/security` first keeps Arch's answer the
    /// answer it was; `/usr/lib64/security` is Fedora's; there is deliberately
    /// no Debian multiarch triple, because Debian's path is `pam-auth-update`
    /// and this command does not do that.
    #[test]
    fn the_module_probe_order_is_the_contract() {
        assert_eq!(
            PAM_MODULE_PATHS,
            [
                "/lib/security/pam_facelock.so",
                "/usr/lib/security/pam_facelock.so",
                "/usr/lib64/security/pam_facelock.so",
            ]
        );
        // One list, and there is deliberately nothing to assert about it:
        // `health::PAM_MODULE_PATHS` is a `pub use` of this const, not a
        // second declaration, so an equality check here could not fail. The
        // re-export *is* the guarantee that `status` cannot say "installed"
        // where `pam add` says "not installed" — re-declaring the list would
        // have to delete it, which is a diff a reader sees.
    }

    /// `pam status --json` carries the resolved module path as a new
    /// **top-level** key: it is a property of the machine, not of a service,
    /// and "the line is present but the module it names is at a path nothing
    /// looks at" is the state an integrator could not otherwise see. `add` and
    /// `remove` refuse before writing when it is missing, so their documents
    /// do not carry it.
    #[test]
    fn status_json_carries_the_module_path_and_the_write_verbs_do_not() {
        let rows = [ServiceReport {
            service: "sudo".to_string(),
            path: Some("/etc/pam.d/sudo".to_string()),
            outcome: Outcome::Present,
            backup: None,
            shadows: None,
        }];

        let status: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Status, false, &rows, &[])).unwrap();
        assert!(
            status.get("module_path").is_some(),
            "the key is always present on status, null when nothing hit"
        );
        assert_eq!(
            status["module_path"],
            installed_module_path()
                .map(|path| serde_json::json!(path.display().to_string()))
                .unwrap_or(serde_json::Value::Null)
        );

        for action in [PamAction::Add, PamAction::Remove] {
            let document: serde_json::Value =
                serde_json::from_str(&report_json(action, false, &rows, &[])).unwrap();
            assert!(document.get("module_path").is_none(), "{action:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Enumeration — `pam status --all` (P3)
    //
    // The blind spot these exist for: `status` could only answer about names
    // it was given, so a configured `omarchy-lock-face` was invisible and
    // "not configured" and "not checked" rendered identically.
    // -----------------------------------------------------------------------

    fn status_all(dirs: &PamDirs, if_present: bool) -> i32 {
        status_in(
            dirs,
            &PamRequest {
                action: PamAction::Status,
                all: true,
                if_present,
                ..PamRequest::default()
            },
        )
    }

    fn remove_all(dirs: &PamDirs) -> anyhow::Result<i32> {
        remove_all_in(
            dirs,
            &PamRequest {
                action: PamAction::Remove,
                all: true,
                no_confirm: true,
                ..PamRequest::default()
            },
        )
    }

    #[test]
    fn remove_all_cleans_provenance_owned_arbitrary_services() {
        let dir = seeded(&[("custom-greeter", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        assert_eq!(
            write_in(&dirs, &add(&["custom-greeter"])).unwrap(),
            WRITE_OK
        );

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
        assert_eq!(
            fs::read(dir.path().join("custom-greeter")).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        assert!(
            BackupStore::open_existing(dirs.backup_dir())
                .unwrap()
                .unwrap()
                .validated_records("custom-greeter")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn remove_all_finds_provenance_owned_writer_accepted_artifact_names() {
        let dir = seeded(&[(".custom", SUDO_BEFORE), ("custom.pacsave", SUDO_BEFORE)]);
        let dirs = only(dir.path());

        assert_eq!(
            write_in(&dirs, &add(&[".custom", "custom.pacsave"])).unwrap(),
            WRITE_OK
        );
        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);

        assert_eq!(read(&dir, ".custom"), SUDO_BEFORE);
        assert_eq!(read(&dir, "custom.pacsave"), SUDO_BEFORE);
    }

    #[test]
    fn provenance_with_remove_all_prefix_survives_the_next_named_transaction() {
        for service in [
            ".facelock-remove-all-user",
            ".facelock-remove-all-commit-user",
        ] {
            let dir = seeded(&[(service, SUDO_BEFORE)]);
            let dirs = only(dir.path());

            assert_eq!(write_in(&dirs, &add(&[service])).unwrap(), WRITE_OK);
            assert_eq!(write_in(&dirs, &remove(&[service])).unwrap(), WRITE_OK);
            assert_eq!(read(&dir, service), SUDO_BEFORE, "{service}");
        }
    }

    #[test]
    fn provenance_with_remove_all_prefix_round_trips_through_remove_all() {
        for service in [
            ".facelock-remove-all-user",
            ".facelock-remove-all-commit-user",
        ] {
            let dir = seeded(&[(service, SUDO_BEFORE)]);
            let dirs = only(dir.path());

            assert_eq!(write_in(&dirs, &add(&[service])).unwrap(), WRITE_OK);
            assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
            assert_eq!(read(&dir, service), SUDO_BEFORE, "{service}");
        }
    }

    #[test]
    fn remove_all_preserves_unowned_administrator_artifacts() {
        let dir = seeded(&[
            (".custom", SUDO_AFTER),
            ("custom.pacsave", SUDO_AFTER),
            ("custom.rpmsave", SUDO_AFTER),
            ("custom~", SUDO_AFTER),
        ]);
        let dirs = only(dir.path());
        let before = snapshot(dir.path());

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);

        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn remove_all_preserves_pam_auth_update_backup() {
        let dir = seeded(&[("common-auth.pam-old", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let before = snapshot(dir.path());

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn remove_all_cleans_exact_legacy_emission_without_provenance() {
        let dir = seeded(&[("pre-0.2-service", SUDO_AFTER)]);
        let dirs = only(dir.path());

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
        assert_eq!(
            fs::read(dir.path().join("pre-0.2-service")).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
    }

    #[test]
    fn remove_all_json_reports_the_committed_transaction() {
        let dir = seeded(&[("pre-0.2-service", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            json: true,
            ..PamRequest::default()
        };
        let document = RefCell::new(None);

        assert_eq!(
            remove_all_in_with_report_hook(
                &dirs,
                &request,
                |_| Ok(()),
                |reports| {
                    *document.borrow_mut() =
                        Some(report_json(PamAction::Remove, false, reports, &[]));
                },
            )
            .unwrap(),
            WRITE_OK
        );
        let value: serde_json::Value =
            serde_json::from_str(document.borrow().as_deref().unwrap()).unwrap();
        assert_eq!(value["command"], "remove");
        assert_eq!(value["dry_run"], false);
        assert_eq!(value["services"][0]["service"], "pre-0.2-service");
        assert_eq!(value["services"][0]["action"], "removed");
        assert_eq!(value["services"][0]["backup"], serde_json::Value::Null);
    }

    #[test]
    fn remove_all_keep_backup_preserves_versioned_and_legacy_rollback_state() {
        let dir = seeded(&[("custom-greeter", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        assert_eq!(
            write_in(&dirs, &add(&["custom-greeter"])).unwrap(),
            WRITE_OK
        );
        fs::write(
            dir.path().join("custom-greeter.facelock-backup"),
            SUDO_BEFORE,
        )
        .unwrap();
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            keep_backup: true,
            ..PamRequest::default()
        };

        assert_eq!(remove_all_in(&dirs, &request).unwrap(), WRITE_OK);
        assert_eq!(
            fs::read(dir.path().join("custom-greeter")).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        assert!(dir.path().join("custom-greeter.facelock-backup").exists());
        assert!(
            !BackupStore::open_existing(dirs.backup_dir())
                .unwrap()
                .unwrap()
                .validated_records("custom-greeter")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn remove_all_rejects_corrupt_provenance_before_mutation() {
        let dir = seeded(&[("custom-greeter", SUDO_BEFORE)]);
        let dirs = only(dir.path());
        assert_eq!(
            write_in(&dirs, &add(&["custom-greeter"])).unwrap(),
            WRITE_OK
        );
        let record = fs::read_dir(dirs.backup_dir())
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .unwrap()
            .path();
        fs::write(&record, b"{not provenance}").unwrap();
        let before = snapshot(dir.path());

        assert!(remove_all(&dirs).is_err());
        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn remove_all_preserves_linked_entries_and_outside_sentinels() {
        let root = tempfile::tempdir().unwrap();
        let pam = root.path().join("pam.d");
        fs::create_dir(&pam).unwrap();
        let symlink_sentinel = root.path().join("symlink-sentinel");
        let hardlink_sentinel = root.path().join("hardlink-sentinel");
        fs::write(&symlink_sentinel, SUDO_AFTER).unwrap();
        fs::write(&hardlink_sentinel, SUDO_AFTER).unwrap();
        std::os::unix::fs::symlink(&symlink_sentinel, pam.join("linked-service")).unwrap();
        fs::hard_link(&hardlink_sentinel, pam.join("hardlinked-service")).unwrap();
        fs::write(pam.join("owned-service"), SUDO_AFTER).unwrap();
        let dirs = only(&pam);

        assert!(remove_all(&dirs).is_err());
        assert_eq!(fs::read(&symlink_sentinel).unwrap(), SUDO_AFTER.as_bytes());
        assert_eq!(fs::read(&hardlink_sentinel).unwrap(), SUDO_AFTER.as_bytes());
        assert_eq!(
            fs::read(pam.join("owned-service")).unwrap(),
            SUDO_AFTER.as_bytes(),
            "a link blocker must be found in preflight before the owned target changes"
        );
    }

    #[test]
    fn remove_all_skips_only_links_covered_by_a_separately_scanned_root() {
        let root = tempfile::tempdir().unwrap();
        let pam = root.path().join("pam.d");
        let vendor = root.path().join("vendor-pam.d");
        let authselect = root.path().join("authselect");
        fs::create_dir(&pam).unwrap();
        fs::create_dir(&vendor).unwrap();
        fs::create_dir(&authselect).unwrap();
        fs::create_dir(authselect.join("custom")).unwrap();
        fs::write(
            authselect.join("system-auth"),
            b"# generated\nauth required pam_unix.so\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(authselect.join("system-auth"), pam.join("system-auth"))
            .unwrap();
        fs::write(pam.join("owned-service"), SUDO_AFTER).unwrap();
        let dirs = PamDirs {
            dirs: vec![pam.clone(), vendor, authselect.clone()],
            backup_dir: root.path().join("pam-backups"),
        };

        assert_eq!(remove_all(&dirs).unwrap(), WRITE_OK);
        assert_eq!(
            fs::read(pam.join("owned-service")).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        assert!(
            pam.join("system-auth")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );

        fs::write(pam.join("owned-service"), SUDO_AFTER).unwrap();
        fs::write(authselect.join("system-auth"), SUDO_AFTER).unwrap();
        let before = snapshot(&pam);
        assert!(remove_all(&dirs).is_err());
        assert_eq!(snapshot(&pam), before);
    }

    #[test]
    fn remove_all_preserves_customized_and_vendor_root_references_as_blockers() {
        let (_root, etc, vendor) = pair();
        let customized =
            b"#%PAM-1.0\nauth required pam_facelock.so debug\nauth include system-auth\n";
        fs::write(etc.join("admin-service"), customized).unwrap();
        fs::write(vendor.join("vendor-service"), SUDO_AFTER).unwrap();
        let dirs = both(&etc, &vendor);
        let before_etc = snapshot(&etc);
        let before_vendor = snapshot(&vendor);

        assert!(remove_all(&dirs).is_err());
        assert_eq!(snapshot(&etc), before_etc);
        assert_eq!(snapshot(&vendor), before_vendor);
    }

    #[test]
    fn remove_all_journals_the_complete_set_before_first_pam_mutation() {
        let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::Journaled {
                let journals = fs::read_dir(dirs.backup_dir())?
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry.file_name().to_str().is_some_and(|name| {
                            name.starts_with(".facelock-remove-all-") && name.ends_with(".json")
                        })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(journals.len(), 1);
                let value: serde_json::Value =
                    serde_json::from_slice(&fs::read(journals[0].path())?).unwrap();
                let services = value["targets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|target| target["service"].as_str().unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(services, ["alpha", "beta"]);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "stop after durable journal",
                ));
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("stop after durable journal"));
        assert_eq!(
            fs::read(dir.path().join("alpha")).unwrap(),
            SUDO_AFTER.as_bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("beta")).unwrap(),
            POLKIT_AFTER.as_bytes()
        );
    }

    #[test]
    fn remove_all_holds_the_state_lock_across_preflight_and_commit() {
        use std::cell::RefCell;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = seeded(&[("alpha", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = RefCell::new(None);

        assert_eq!(
            remove_all_in_with_hook(&dirs, &request, |point| {
                if point == RemoveAllPoint::Locked {
                    let competing_dirs = dirs.clone();
                    let attempted_tx = attempted_tx.clone();
                    let acquired_tx = acquired_tx.clone();
                    *worker.borrow_mut() = Some(std::thread::spawn(move || {
                        let store = BackupStore::open(competing_dirs.backup_dir()).unwrap();
                        attempted_tx.send(()).unwrap();
                        let transaction = store.transaction(&competing_dirs).unwrap();
                        acquired_tx.send(()).unwrap();
                        drop(transaction);
                    }));
                    attempted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                    assert!(
                        acquired_rx
                            .recv_timeout(Duration::from_millis(100))
                            .is_err(),
                        "a competing recovery acquired the lock during remove-all preflight"
                    );
                }
                Ok(())
            })
            .unwrap(),
            WRITE_OK
        );
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.into_inner().unwrap().join().unwrap();
    }

    #[test]
    fn remove_all_dry_run_does_not_recover_or_change_an_interrupted_batch() {
        let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::AfterMutation(0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "leave an interrupted batch",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        let pam_before = snapshot(dir.path());
        let state_before = snapshot(dirs.backup_dir());
        let dry_run = PamRequest {
            dry_run: true,
            ..request
        };

        let _ = remove_all_in(&dirs, &dry_run);

        assert_eq!(snapshot(dir.path()), pam_before);
        assert_eq!(snapshot(dirs.backup_dir()), state_before);
    }

    #[test]
    fn remove_all_dry_run_does_not_repair_an_untrusted_state_directory() {
        let dir = seeded(&[("alpha", SUDO_AFTER)]);
        let dirs = only(dir.path());
        BackupStore::open(dirs.backup_dir()).unwrap();
        fs::write(
            dirs.backup_dir().join("administrator-evidence"),
            b"retain exact state bytes\n",
        )
        .unwrap();
        fs::set_permissions(
            dirs.backup_dir(),
            fs::Permissions::from_mode(PAM_BACKUPS_DIR_MODE | 0o055),
        )
        .unwrap();
        let before = directory_snapshot(dirs.backup_dir());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            dry_run: true,
            ..PamRequest::default()
        };

        let error = remove_all_in(&dirs, &request).unwrap_err();

        assert!(error.to_string().contains("owner or mode is not trusted"));
        assert_eq!(directory_snapshot(dirs.backup_dir()), before);
    }

    #[test]
    fn remove_all_rolls_back_earlier_files_when_a_later_recheck_fails() {
        let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
        let dirs = only(dir.path());
        let alpha_inode = fs::metadata(dir.path().join("alpha")).unwrap().ino();
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        let changed = b"# administrator changed beta after preflight\n";

        assert!(
            remove_all_in_with_hook(&dirs, &request, |point| {
                if point == RemoveAllPoint::BeforeMutation(1) {
                    fs::write(dir.path().join("beta"), changed)?;
                }
                Ok(())
            })
            .is_err()
        );

        assert_eq!(
            fs::read(dir.path().join("alpha")).unwrap(),
            SUDO_AFTER.as_bytes(),
            "the first successful removal must be restored"
        );
        assert_eq!(
            fs::metadata(dir.path().join("alpha")).unwrap().ino(),
            alpha_inode,
            "rollback must exchange the exact displaced inode back"
        );
        assert_eq!(fs::read(dir.path().join("beta")).unwrap(), changed);
    }

    #[test]
    fn remove_all_final_rescan_rolls_back_when_a_new_reference_appears() {
        let dir = seeded(&[("alpha", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let alpha_inode = fs::metadata(dir.path().join("alpha")).unwrap().ino();
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        assert!(
            remove_all_in_with_hook(&dirs, &request, |point| {
                if point == RemoveAllPoint::AfterMutation(0) {
                    fs::write(
                        dir.path().join("late-admin-reference"),
                        b"auth required pam_facelock.so debug\n",
                    )?;
                }
                Ok(())
            })
            .is_err()
        );

        assert_eq!(
            fs::read(dir.path().join("alpha")).unwrap(),
            SUDO_AFTER.as_bytes()
        );
        assert_eq!(
            fs::metadata(dir.path().join("alpha")).unwrap().ino(),
            alpha_inode,
            "the preflight inode must be restored when the final scan fails"
        );
        assert_eq!(
            fs::read(dir.path().join("late-admin-reference")).unwrap(),
            b"auth required pam_facelock.so debug\n"
        );
    }

    #[test]
    fn remove_all_recovery_rolls_back_a_crash_after_an_earlier_file() {
        let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };

        let error = remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::AfterMutation(0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "crash after alpha",
                ));
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("crash after alpha"));

        recover_remove_all_in(&dirs).unwrap();
        assert_eq!(
            fs::read(dir.path().join("alpha")).unwrap(),
            SUDO_AFTER.as_bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("beta")).unwrap(),
            POLKIT_AFTER.as_bytes()
        );
    }

    #[test]
    fn remove_all_recovery_resumes_every_binding_first_rollback_publication_boundary() {
        for crash_at in [
            RemoveAllRollbackPoint::ReverseExchange,
            RemoveAllRollbackPoint::TempUnlink,
            RemoveAllRollbackPoint::BindingUnlink,
            RemoveAllRollbackPoint::IntentUnlink,
        ] {
            let dir = seeded(&[("alpha", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let request = PamRequest {
                action: PamAction::Remove,
                all: true,
                no_confirm: true,
                ..PamRequest::default()
            };
            let error = remove_all_in_with_hook(&dirs, &request, |point| {
                if point == RemoveAllPoint::AfterMutation(0) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "leave a published remove-all replacement",
                    ));
                }
                Ok(())
            })
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("leave a published remove-all replacement"),
                "{crash_at:?}: {error}"
            );

            let store = BackupStore::open_existing(dirs.backup_dir())
                .unwrap()
                .unwrap();
            let (journal, commit) = load_remove_all_state(&store).unwrap();
            assert!(commit.is_none(), "{crash_at:?}");
            let journal = journal.unwrap();
            let rollback_error = rollback_remove_all_with_hook(
                &store,
                &dirs,
                &journal.value,
                &journal.name,
                &journal.identity,
                |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "crash at rollback publication cleanup boundary",
                        ));
                    }
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(
                rollback_error.kind(),
                std::io::ErrorKind::Interrupted,
                "{crash_at:?}: {rollback_error}"
            );
            drop(store);

            recover_remove_all_in(&dirs).unwrap_or_else(|error| panic!("{crash_at:?}: {error}"));

            assert_eq!(
                fs::read(dir.path().join("alpha")).unwrap(),
                SUDO_AFTER.as_bytes(),
                "{crash_at:?}"
            );
            assert_eq!(
                fs::read_dir(dirs.backup_dir()).unwrap().count(),
                0,
                "{crash_at:?}"
            );
        }
    }

    #[test]
    fn remove_all_recovery_cleans_an_exact_intent_only_pam_replacement() {
        let dir = seeded(&[("alpha", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let store = BackupStore::open(dirs.backup_dir()).unwrap();
        let transaction = store.transaction(&dirs).unwrap();
        let path = dir.path().join("alpha");
        let (original, expected) = read_regular_nofollow(&path).unwrap();
        let installed = with_line_removed(&original);
        let prepared = transaction.plan("alpha", &original, &installed).unwrap();
        transaction.persist(&prepared, &original).unwrap();
        create_remove_all_journal(
            &store,
            false,
            vec![RemoveAllJournalTarget {
                service: "alpha".to_owned(),
                backup: prepared.backup.clone(),
                original: expected.clone(),
                installed_sha256: sha256_hex(&installed),
                delete_override: Some(false),
            }],
        )
        .unwrap();

        let error = transaction
            .replace_pam_with_intent_hook(&prepared, &path, &expected, &installed, |point| {
                if point == PamReplaceCrashPoint::Intent {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "crash after remove-all PAM intent",
                    ));
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        drop(transaction);

        recover_remove_all_in(&dirs).unwrap();

        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(dirs.backup_dir()).unwrap().count(), 0);
    }

    #[test]
    fn remove_all_recovery_resumes_every_rollback_pair_cleanup_boundary() {
        for crash_at in [
            CleanupCrashPoint::Intent,
            CleanupCrashPoint::BackupQuarantine,
            CleanupCrashPoint::RecordQuarantine,
            CleanupCrashPoint::BackupUnlink,
            CleanupCrashPoint::RecordUnlink,
        ] {
            let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let path = dir.path().join("alpha");
            let (original, expected) = read_regular_nofollow(&path).unwrap();
            let installed = with_line_removed(&original);
            let planned = transaction.plan("alpha", &original, &installed).unwrap();
            transaction.persist(&planned, &original).unwrap();
            let target = RemoveAllJournalTarget {
                service: "alpha".to_owned(),
                backup: planned.backup.clone(),
                original: expected,
                installed_sha256: sha256_hex(&installed),
                delete_override: Some(false),
            };
            let beta_path = dir.path().join("beta");
            let (beta_original, beta_expected) = read_regular_nofollow(&beta_path).unwrap();
            let beta_installed = with_line_removed(&beta_original);
            let beta_planned = transaction
                .plan("beta", &beta_original, &beta_installed)
                .unwrap();
            transaction.persist(&beta_planned, &beta_original).unwrap();
            let beta_target = RemoveAllJournalTarget {
                service: "beta".to_owned(),
                backup: beta_planned.backup,
                original: beta_expected,
                installed_sha256: sha256_hex(&beta_installed),
                delete_override: Some(false),
            };
            create_remove_all_journal(&store, false, vec![target.clone(), beta_target]).unwrap();
            let prepared = prepared_for_remove_all_target(&store, &target).unwrap();
            let directory = open_directory_nofollow(dirs.backup_dir()).unwrap();

            let error = store
                .cleanup_one_at(&directory, &prepared, |point| {
                    if point == crash_at {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "crash during remove-all pair cleanup",
                        ));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            drop(transaction);

            recover_remove_all_in(&dirs).unwrap();

            assert_eq!(fs::read(&path).unwrap(), original, "{crash_at:?}");
            assert_eq!(fs::read(&beta_path).unwrap(), beta_original, "{crash_at:?}");
            assert_eq!(
                fs::read_dir(dirs.backup_dir()).unwrap().count(),
                0,
                "{crash_at:?}"
            );
        }
    }

    #[test]
    fn remove_all_recovery_preserves_partial_substituted_and_conflicting_pairs() {
        for state in ["partial", "substituted", "conflicting"] {
            let dir = seeded(&[("alpha", SUDO_AFTER)]);
            let dirs = only(dir.path());
            let store = BackupStore::open(dirs.backup_dir()).unwrap();
            let transaction = store.transaction(&dirs).unwrap();
            let path = dir.path().join("alpha");
            let (original, expected) = read_regular_nofollow(&path).unwrap();
            let installed = with_line_removed(&original);
            let planned = transaction.plan("alpha", &original, &installed).unwrap();
            transaction.persist(&planned, &original).unwrap();
            create_remove_all_journal(
                &store,
                false,
                vec![RemoveAllJournalTarget {
                    service: "alpha".to_owned(),
                    backup: planned.backup.clone(),
                    original: expected,
                    installed_sha256: sha256_hex(&installed),
                    delete_override: Some(false),
                }],
            )
            .unwrap();
            let backup = dirs.backup_dir().join(&planned.backup);
            let record = dirs.backup_dir().join(format!("{}.json", planned.backup));
            match state {
                "partial" => fs::remove_file(&backup).unwrap(),
                "substituted" => fs::write(&record, b"administrator state\n").unwrap(),
                "conflicting" => fs::write(
                    dirs.backup_dir()
                        .join(quarantine_name("backup", &planned.backup)),
                    &original,
                )
                .unwrap(),
                _ => unreachable!(),
            }
            drop(transaction);
            let before = snapshot(dirs.backup_dir());

            assert!(recover_remove_all_in(&dirs).is_err(), "{state}");

            assert_eq!(snapshot(dirs.backup_dir()), before, "{state}");
            assert_eq!(fs::read(&path).unwrap(), original, "{state}");
        }
    }

    #[test]
    fn every_pam_transaction_recovers_remove_all_before_generic_state() {
        let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::AfterMutation(0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "crash after alpha",
                ));
            }
            Ok(())
        })
        .unwrap_err();

        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        drop(store.transaction(&dirs).unwrap());

        assert_eq!(
            fs::read(dir.path().join("alpha")).unwrap(),
            SUDO_AFTER.as_bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("beta")).unwrap(),
            POLKIT_AFTER.as_bytes()
        );
    }

    #[test]
    fn remove_all_recovery_completes_a_durable_commit_without_rolling_back() {
        let dir = seeded(&[("alpha", SUDO_AFTER), ("beta", POLKIT_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::CommitMarked {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "crash after commit marker",
                ));
            }
            Ok(())
        })
        .unwrap_err();

        recover_remove_all_in(&dirs).unwrap();
        assert_eq!(
            fs::read(dir.path().join("alpha")).unwrap(),
            SUDO_BEFORE.as_bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("beta")).unwrap(),
            POLKIT_BEFORE.as_bytes()
        );
        assert!(fs::read_dir(dirs.backup_dir()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".facelock-remove-all-")
        }));
    }

    #[test]
    fn remove_all_recovery_preserves_an_orphan_commit_with_duplicate_services() {
        let dir = seeded(&[("alpha", SUDO_AFTER)]);
        let dirs = only(dir.path());
        let request = PamRequest {
            action: PamAction::Remove,
            all: true,
            no_confirm: true,
            ..PamRequest::default()
        };
        remove_all_in_with_hook(&dirs, &request, |point| {
            if point == RemoveAllPoint::CommitMarked {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "leave a self-contained commit",
                ));
            }
            Ok(())
        })
        .unwrap_err();

        let store = BackupStore::open_existing(dirs.backup_dir())
            .unwrap()
            .unwrap();
        let (journal, commit) = load_remove_all_state(&store).unwrap();
        let journal = journal.unwrap();
        let commit = commit.unwrap();
        let mut duplicate = commit.value;
        duplicate.targets.push(duplicate.targets[0].clone());
        fs::write(
            dirs.backup_dir().join(&commit.name),
            serde_json::to_vec_pretty(&duplicate).unwrap(),
        )
        .unwrap();
        fs::remove_file(dirs.backup_dir().join(&journal.name)).unwrap();
        let pam_before = snapshot(dir.path());
        let state_before = snapshot(dirs.backup_dir());

        let error = recover_remove_all_in(&dirs).unwrap_err();

        assert!(error.to_string().contains("remove-all commit is invalid"));
        assert_eq!(snapshot(dir.path()), pam_before);
        assert_eq!(snapshot(dirs.backup_dir()), state_before);
    }

    #[test]
    fn remove_all_enumerates_the_open_root_descriptor_after_path_replacement() {
        let root = tempfile::tempdir().unwrap();
        let pam = root.path().join("pam.d");
        let held = root.path().join("held-pam.d");
        fs::create_dir(&pam).unwrap();
        fs::write(pam.join("original-service"), SUDO_AFTER).unwrap();
        let directory = open_directory_nofollow(&pam).unwrap();

        fs::rename(&pam, &held).unwrap();
        fs::create_dir(&pam).unwrap();
        fs::write(pam.join("replacement-service"), SUDO_AFTER).unwrap();

        let names = directory_entry_names(&directory).unwrap();
        assert!(
            names
                .iter()
                .any(|name| name == OsStr::new("original-service"))
        );
        assert!(
            !names
                .iter()
                .any(|name| name == OsStr::new("replacement-service"))
        );
    }

    /// The headline: every configured service is listed, from both
    /// directories, whether or not anyone named it.
    #[test]
    fn all_lists_every_configured_service() {
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_AFTER).unwrap();
        fs::write(etc.join("uninvolved"), SUDO_BEFORE).unwrap();
        fs::write(vendor.join("omarchy-lock-face"), OMARCHY_PRESENT).unwrap();

        let scan = scan_directories(&both(&etc, &vendor));
        assert_eq!(
            scan.names,
            ["omarchy-lock-face", "sudo"],
            "sorted, and the \
             service with no facelock line is not in the report at all"
        );

        let reports = status_reports(&both(&etc, &vendor), &scan.names, &Sink::verb(true));
        assert!(reports.iter().all(|row| row.outcome == Outcome::Present));
        assert_eq!(status_all(&both(&etc, &vendor), false), STATUS_PRESENT);
    }

    /// A service configured through an `/etc` copy of a package's file is
    /// reported as such: still `present`, and the row names the vendor file it
    /// hides — which is what says the copy will not follow the package's
    /// updates.
    #[test]
    fn an_etc_override_is_reported_as_one() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        fs::write(etc.join("polkit-1"), POLKIT_AFTER).unwrap();

        let reports = status_reports(
            &both(&etc, &vendor),
            &["polkit-1".to_string()],
            &Sink::verb(true),
        );

        assert_eq!(reports[0].outcome, Outcome::Present);
        assert_eq!(
            reports[0].shadows.as_deref(),
            Some(vendor.join("polkit-1").to_str().unwrap())
        );
        let value: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Status, false, &reports, &[])).unwrap();
        assert_eq!(
            value["services"][0]["shadows"],
            vendor.join("polkit-1").to_str().unwrap()
        );
    }

    /// ...and a service that shadows nothing carries no key at all, so a
    /// machine with no vendor directory emits the document it always did.
    #[test]
    fn a_service_that_shadows_nothing_carries_no_shadows_key() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let reports = status_reports(&only(dir.path()), &["sudo".to_string()], &Sink::verb(true));

        assert_eq!(reports[0].shadows, None);
        let value: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Status, false, &reports, &[])).unwrap();
        assert!(value["services"][0].get("shadows").is_none());
    }

    /// A vendor file carrying the line while `/etc` shadows it without one is
    /// **not configured**, and the name still has to appear: the file
    /// Linux-PAM reads has no line in it, and dropping the name would hide
    /// exactly the machine an operator cannot otherwise explain.
    #[test]
    fn a_shadowed_vendor_line_is_reported_as_missing_not_as_configured() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), OMARCHY_PRESENT).unwrap();
        fs::write(etc.join("polkit-1"), POLKIT_BEFORE).unwrap();

        let dirs = both(&etc, &vendor);
        let scan = scan_directories(&dirs);
        assert_eq!(scan.names, ["polkit-1"]);

        let reports = status_reports(&dirs, &scan.names, &Sink::verb(true));
        assert_eq!(
            reports[0].outcome,
            Outcome::Missing,
            "the /etc file is the one PAM reads, and it has no line"
        );
        assert_eq!(status_all(&dirs, false), STATUS_MISSING);
    }

    /// Nothing configured is exit 1, not 0 — a machine with no facelock line
    /// anywhere is not "fine", it is not set up — and `--if-present` does not
    /// convert it, because there is no `absent` row here to forgive.
    #[test]
    fn nothing_configured_is_exit_one_with_and_without_if_present() {
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_BEFORE).unwrap();

        let dirs = both(&etc, &vendor);
        assert!(scan_directories(&dirs).names.is_empty());
        assert_eq!(status_all(&dirs, false), STATUS_MISSING);
        assert_eq!(status_all(&dirs, true), STATUS_MISSING);
    }

    /// **The distinction the gap exists for.** A directory that could not be
    /// listed is reported as unread and forces exit 2; it never reads as "no
    /// services here". Skipped as root, where the mode bits are ignored and
    /// the assertion would be vacuous.
    #[test]
    fn an_unreadable_directory_is_not_checked_rather_than_not_configured() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_AFTER).unwrap();
        fs::set_permissions(&vendor, fs::Permissions::from_mode(0o000)).unwrap();

        let dirs = both(&etc, &vendor);
        let scan = scan_directories(&dirs);
        assert_eq!(scan.names, ["sudo"], "what could be read is still reported");
        assert_eq!(
            scan.unreadable().map(|(path, _)| path).collect::<Vec<_>>(),
            [vendor.as_path()]
        );
        assert_eq!(
            status_all(&dirs, false),
            STATUS_ERROR,
            "a configured service and an unread directory is still an \
             incomplete answer"
        );

        let document: serde_json::Value = serde_json::from_str(&report_json(
            PamAction::Status,
            false,
            &[],
            &scan.directories,
        ))
        .unwrap();
        assert_eq!(document["directories"][0]["status"], "scanned");
        assert!(document["directories"][0].get("error").is_none());
        assert_eq!(document["directories"][1]["status"], "unreadable");
        assert!(document["directories"][1]["error"].is_string());

        fs::set_permissions(&vendor, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A directory that is *not there* is a different answer from one that
    /// would not open: it demonstrably holds no service files, so it is
    /// reported as absent and does not raise the exit code. The default search
    /// path names a vendor directory many machines do not have, and treating
    /// that as unanswerable would make every one of them exit 2 forever.
    #[test]
    fn a_missing_directory_is_absent_rather_than_unreadable() {
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_AFTER).unwrap();
        fs::remove_dir(&vendor).unwrap();

        let dirs = both(&etc, &vendor);
        let scan = scan_directories(&dirs);
        assert_eq!(scan.directories[1].state, DirState::Absent);
        assert_eq!(scan.unreadable().count(), 0);
        assert_eq!(status_all(&dirs, false), STATUS_PRESENT);
    }

    /// The `--json` document is the same document, with one additive
    /// top-level key. A named request does not carry it — it never claimed to
    /// have looked everywhere.
    #[test]
    fn only_all_carries_the_directories_key() {
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_AFTER).unwrap();

        let dirs = both(&etc, &vendor);
        let scan = scan_directories(&dirs);
        let reports = status_reports(&dirs, &scan.names, &Sink::verb(true));

        let named: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Status, false, &reports, &[])).unwrap();
        assert!(named.get("directories").is_none());

        let enumerated: serde_json::Value = serde_json::from_str(&report_json(
            PamAction::Status,
            false,
            &reports,
            &scan.directories,
        ))
        .unwrap();
        assert_eq!(enumerated["command"], "status");
        assert_eq!(enumerated["dry_run"], false);
        assert_eq!(enumerated["services"][0]["action"], "present");
        assert_eq!(
            enumerated["directories"][0]["path"],
            etc.to_str().unwrap(),
            "the directories are in search order"
        );
        assert_eq!(
            enumerated["directories"][1]["path"],
            vendor.to_str().unwrap()
        );
    }

    /// A `.facelock-backup` is a byte copy of a configured file and is not a
    /// service. Neither is a `.pacsave`, pam-auth-update's `.pam-old`, a `~`
    /// file, or this module's own in-flight temp file. Reporting one as
    /// configured would be the report being confidently wrong, which is what
    /// enumeration is for removing.
    #[test]
    fn backups_and_package_manager_leftovers_are_not_services() {
        let dir = seeded(&[
            ("sudo", SUDO_AFTER),
            ("sudo.facelock-backup", SUDO_AFTER),
            ("polkit-1.pacsave", SUDO_AFTER),
            ("hyprlock.rpmsave", SUDO_AFTER),
            ("login.dpkg-old", SUDO_AFTER),
            ("common-auth.pam-old", SUDO_AFTER),
            ("swaylock~", SUDO_AFTER),
            (".sudo.facelock-1234-5678", SUDO_AFTER),
        ]);

        assert_eq!(scan_directories(&only(dir.path())).names, ["sudo"]);
    }

    #[test]
    fn pam_auth_update_backup_is_not_a_service() {
        let dir = seeded(&[("sudo", SUDO_AFTER), ("common-auth.pam-old", SUDO_AFTER)]);

        assert_eq!(scan_directories(&only(dir.path())).names, ["sudo"]);
    }

    /// A service file that could not be read is carried into the report, not
    /// omitted: leaving it out would report "not configured" for a machine
    /// this could not check. It lands as `unknown`, which is exit 2. Skipped
    /// as root, which ignores the mode bits.
    #[test]
    fn an_unreadable_service_file_is_reported_rather_than_skipped() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        fs::set_permissions(dir.path().join("sudo"), fs::Permissions::from_mode(0o000)).unwrap();

        let dirs = only(dir.path());
        assert_eq!(scan_directories(&dirs).names, ["sudo"]);
        let reports = status_reports(&dirs, &["sudo".to_string()], &Sink::verb(true));
        assert!(matches!(reports[0].outcome, Outcome::Unknown(_)));
        assert_eq!(status_all(&dirs, false), STATUS_ERROR);

        fs::set_permissions(dir.path().join("sudo"), fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// The scan reports what the resolver would accept: an entry symlinked out
    /// of its directory is neither followed nor silently dropped — it becomes
    /// the same `unknown` row a named request gets for it.
    #[test]
    fn an_escaping_symlink_found_by_the_scan_is_reported_as_unknown() {
        let (_root, etc, vendor) = pair();
        let outside = vendor.join("elsewhere");
        fs::write(&outside, SUDO_AFTER).unwrap();
        std::os::unix::fs::symlink(&outside, etc.join("hyprlock")).unwrap();

        // The scan reads *through* the link to decide the name is worth
        // reporting; the resolver is what refuses to act on it.
        let dirs = PamDirs::new(vec![etc.clone()]);
        assert_eq!(scan_directories(&dirs).names, ["hyprlock"]);

        let reports = status_reports(&dirs, &["hyprlock".to_string()], &Sink::verb(true));
        assert_eq!(
            reports[0].outcome,
            Outcome::Unknown(SYMLINKED_OUT_OF_DIR.to_string())
        );
        assert_eq!(status_all(&dirs, false), STATUS_ERROR);
    }

    /// `--all` and a named request answer identically about one service. They
    /// share [`status_reports`], and this is the assertion that says so —
    /// a second row builder for the enumerating form is the drift this
    /// forbids.
    #[test]
    fn all_and_a_named_request_agree_about_one_service() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();
        fs::write(etc.join("polkit-1"), POLKIT_AFTER).unwrap();

        let dirs = both(&etc, &vendor);
        let enumerated = status_reports(&dirs, &scan_directories(&dirs).names, &Sink::verb(true));
        let named = status_reports(&dirs, &["polkit-1".to_string()], &Sink::verb(true));

        assert_eq!(enumerated, named);
    }

    /// **Nothing found is not "nothing configured" when something could not be
    /// read.** The unqualified sentence names every directory on the search
    /// path, so under `2>/dev/null` — the ordinary way to take the human
    /// answer — it asserted the one thing this flag exists to stop it
    /// asserting. Skipped as root, which ignores the mode bits.
    #[test]
    fn an_empty_answer_is_qualified_by_what_could_not_be_read() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("plain"), SUDO_BEFORE).unwrap();
        fs::set_permissions(&vendor, fs::Permissions::from_mode(0o000)).unwrap();

        let dirs = both(&etc, &vendor);
        let scan = scan_directories(&dirs);
        assert!(scan.names.is_empty());
        assert_eq!(
            scan.answered(),
            [etc.display().to_string()],
            "only the directory that produced an answer may be spoken for"
        );
        assert_eq!(status_all(&dirs, false), STATUS_ERROR);

        // The two sentences are different, and the qualified one names the
        // unread directory as unread rather than as searched.
        let qualified = PamMessage::PamStatusNoServicesIncomplete {
            dirs: etc.display().to_string(),
            unchecked: vendor.display().to_string(),
        }
        .localized();
        assert!(qualified.contains("could not be checked"), "{qualified}");
        assert!(
            !PamMessage::PamStatusNoServices {
                dirs: dirs.display()
            }
            .localized()
            .contains("could not be checked")
        );

        fs::set_permissions(&vendor, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// ...and when *no* directory could be read there is no set to scope an
    /// emptiness to, so the line says only that — never that nothing is
    /// configured. stdout still carries one, because a human reading it alone
    /// would otherwise get a sentence in the other two branches and silence in
    /// the one where the machine is worst off.
    #[test]
    fn nothing_readable_at_all_says_so_without_claiming_nothing_is_configured() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (_root, etc, vendor) = pair();
        for dir in [&etc, &vendor] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).unwrap();
        }

        let dirs = both(&etc, &vendor);
        let scan = scan_directories(&dirs);
        assert!(scan.answered().is_empty());
        assert_eq!(scan.unreadable().count(), 2);
        assert_eq!(status_all(&dirs, false), STATUS_ERROR);

        let line = PamMessage::PamStatusNothingReadable {
            dirs: dirs.display(),
        }
        .localized();
        assert!(
            line.contains("No directory on the search path could be read"),
            "{line}"
        );
        assert!(
            !line.contains("carries the facelock PAM line"),
            "it must not assert anything about what is configured: {line}"
        );

        for dir in [&etc, &vendor] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// **An entry that exists and cannot be examined must not vanish.** The
    /// followed-metadata guard first treated every `stat` failure as "not a
    /// regular file", so a symlink into a directory the caller may not
    /// traverse dropped out of `--all` and out of `facelock status` while
    /// `--service` on the same name still said `unknown`, exit 2 — the two
    /// forms disagreeing about one service, which is what sharing a row
    /// builder is supposed to make impossible. Skipped as root, which
    /// traverses anything.
    #[test]
    fn a_symlink_into_an_untraversable_directory_is_unknown_not_invisible() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_AFTER).unwrap();
        fs::write(vendor.join("real"), SUDO_AFTER).unwrap();
        std::os::unix::fs::symlink(vendor.join("real"), etc.join("hyprlock")).unwrap();
        fs::set_permissions(&vendor, fs::Permissions::from_mode(0o000)).unwrap();

        let dirs = PamDirs::new(vec![etc.clone()]);
        let scan = scan_directories(&dirs);
        assert_eq!(
            scan.names,
            ["hyprlock", "sudo"],
            "the entry this could not examine is carried, not dropped"
        );

        let reports = status_reports(&dirs, &scan.names, &Sink::verb(true));
        assert!(matches!(reports[0].outcome, Outcome::Unknown(_)));
        assert_eq!(status_all(&dirs, false), STATUS_ERROR);

        // The invariant the hole falsified: both forms answer the same.
        let named = status_reports(&dirs, &["hyprlock".to_string()], &Sink::verb(true));
        assert_eq!(reports[0], named[0]);

        fs::set_permissions(&vendor, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The same hole reached by a route root cannot escape either: a symlink
    /// loop, whose `stat` fails with `ELOOP` for every caller.
    #[test]
    fn a_symlink_loop_is_unknown_not_invisible() {
        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        std::os::unix::fs::symlink(dir.path().join("loop-b"), dir.path().join("loop-a")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("loop-a"), dir.path().join("loop-b")).unwrap();

        let dirs = only(dir.path());
        let scan = scan_directories(&dirs);
        assert_eq!(scan.names, ["loop-a", "loop-b", "sudo"]);

        let reports = status_reports(&dirs, &scan.names, &Sink::verb(true));
        assert!(matches!(reports[0].outcome, Outcome::Unknown(_)));
        assert_eq!(status_all(&dirs, false), STATUS_ERROR);

        let named = status_reports(&dirs, &["loop-a".to_string()], &Sink::verb(true));
        assert_eq!(reports[0], named[0]);
    }

    /// **A FIFO in a scanned directory must not hang the command.**
    /// `fs::read` on one blocks until a writer appears, which is forever
    /// on a `/etc/pam.d` nobody is writing to — and this scan is what
    /// `facelock status` runs, so the diagnostic command would hang on exactly
    /// the broken machine it exists to describe. The test *is* the assertion:
    /// before the followed-metadata check it did not return.
    #[test]
    fn a_fifo_entry_is_skipped_rather_than_read() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let fifo = dir.path().join("fifo-service");
        let c_fifo = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path that outlives the call; `mkfifo` only
        // reads it.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o644) }, 0);

        // A symlink onto the FIFO too: `file_type` at the call site is an
        // lstat, so a link is the way a non-regular file gets past it.
        std::os::unix::fs::symlink(&fifo, dir.path().join("linked-fifo")).unwrap();

        assert_eq!(scan_directories(&only(dir.path())).names, ["sudo"]);
    }

    /// A directory reached through a symlink is not an unreadable service
    /// file. It used to survive the `lstat` skip, fail the read with EISDIR,
    /// and land as `unknown` — a real directory reported as a service this
    /// could not answer for.
    #[test]
    fn a_symlink_to_a_directory_is_not_a_service() {
        let (_root, etc, vendor) = pair();
        fs::write(etc.join("sudo"), SUDO_AFTER).unwrap();
        std::os::unix::fs::symlink(&vendor, etc.join("linked-dir")).unwrap();

        let dirs = PamDirs::new(vec![etc.clone()]);
        assert_eq!(scan_directories(&dirs).names, ["sudo"]);
        assert_eq!(status_all(&dirs, false), STATUS_PRESENT);
    }

    /// A name this cannot spell is a name it cannot resolve. `to_string_lossy`
    /// substituted U+FFFD and handed the resolver a name no file has, so a
    /// configured service was reported `absent` at a path that does not exist
    /// — and under `--if-present` that scored 0, which is "everything fine".
    #[test]
    fn a_non_utf8_entry_name_is_skipped_not_mangled() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = seeded(&[("sudo", SUDO_AFTER)]);
        let bad = dir.path().join(OsStr::from_bytes(b"bad\xffname"));
        fs::write(&bad, SUDO_AFTER).unwrap();

        let dirs = only(dir.path());
        assert_eq!(
            scan_directories(&dirs).names,
            ["sudo"],
            "the unspellable name is skipped, not reported at a path that does not exist"
        );
        assert_eq!(status_all(&dirs, true), STATUS_PRESENT);
    }

    /// The row that *creates* the shadow carries it. `Origin::Vendor` has
    /// nothing to hide at resolve time, so without this the `overridden` row —
    /// the one that makes the fact true — was the only row without the key,
    /// while `status` reported it a second later.
    #[test]
    fn an_overridden_row_names_the_vendor_file_it_now_shadows() {
        let (_root, etc, vendor) = pair();
        fs::write(vendor.join("polkit-1"), POLKIT_BEFORE).unwrap();

        let request = add(&["polkit-1"]);
        let write = WriteRequest {
            action: WriteAction::Add,
            request: &request,
            remedy: "--allow-sensitive",
        };
        let dirs = both(&etc, &vendor);
        let targets = plan_writes(&dirs, &write).unwrap();
        let reports = apply_all(&dirs, &targets, &write, &Sink::verb(true));

        assert_eq!(reports[0].outcome, Outcome::Overridden);
        assert_eq!(
            reports[0].shadows.as_deref(),
            Some(vendor.join("polkit-1").to_str().unwrap())
        );
        let value: serde_json::Value =
            serde_json::from_str(&report_json(PamAction::Add, false, &reports, &[])).unwrap();
        assert_eq!(
            value["services"][0]["shadows"],
            vendor.join("polkit-1").to_str().unwrap()
        );

        // ...and the next `status` agrees, now through the resolver.
        let after = status_reports(&dirs, &["polkit-1".to_string()], &Sink::verb(true));
        assert_eq!(after[0].shadows, reports[0].shadows);
    }
}
