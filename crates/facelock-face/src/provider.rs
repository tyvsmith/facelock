use std::collections::VecDeque;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use object::read::elf::Dyn;
use object::{Architecture, Object};
use ort::session::builder::SessionBuilder;

const EXPECTED_SONAME: &str = "libonnxruntime.so.1";
const MAX_RUNTIME_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivilegeContext {
    Unprivileged,
    Privileged,
}

const PROCESS_CAPABILITY_FIELDS: [&str; 4] = ["CapInh", "CapPrm", "CapEff", "CapAmb"];
const CALLING_THREAD_STATUS_PATH: &str = "/proc/thread-self/status";

fn parse_process_capabilities(status: &str) -> std::result::Result<bool, String> {
    let mut values = [None; PROCESS_CAPABILITY_FIELDS.len()];
    for line in status.lines() {
        let Some((name, raw_value)) = line.split_once(':') else {
            continue;
        };
        let Some(index) = PROCESS_CAPABILITY_FIELDS
            .iter()
            .position(|field| *field == name)
        else {
            continue;
        };
        if values[index].is_some() {
            return Err(format!("duplicate {name} in {CALLING_THREAD_STATUS_PATH}"));
        }
        let value = raw_value.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("invalid {name} in {CALLING_THREAD_STATUS_PATH}"));
        }
        values[index] = Some(value.bytes().any(|byte| byte != b'0'));
    }

    let mut any = false;
    for (index, value) in values.into_iter().enumerate() {
        let Some(value) = value else {
            return Err(format!(
                "missing {} in {CALLING_THREAD_STATUS_PATH}",
                PROCESS_CAPABILITY_FIELDS[index]
            ));
        };
        any |= value;
    }
    Ok(any)
}

fn current_thread_capabilities_with(
    read_status: impl FnOnce(&Path) -> std::result::Result<String, String>,
) -> std::result::Result<bool, String> {
    let status = read_status(Path::new(CALLING_THREAD_STATUS_PATH))?;
    parse_process_capabilities(&status)
}

fn current_thread_capabilities() -> std::result::Result<bool, String> {
    current_thread_capabilities_with(|path| {
        fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
    })
}

fn privilege_context_from_facts(
    uid: u32,
    euid: u32,
    gid: u32,
    egid: u32,
    at_secure: bool,
    capabilities: std::result::Result<bool, String>,
) -> PrivilegeContext {
    if uid == 0
        || euid == 0
        || gid == 0
        || egid == 0
        || uid != euid
        || gid != egid
        || at_secure
        || capabilities.unwrap_or(true)
    {
        PrivilegeContext::Privileged
    } else {
        PrivilegeContext::Unprivileged
    }
}

impl PrivilegeContext {
    fn current() -> Self {
        // A set-id/capability-secure process must be treated like euid 0 even
        // when its numeric IDs happen to match. PAM and the daemon are root;
        // this also covers a future privileged non-root entry point.
        let (uid, euid, gid, egid, at_secure) = unsafe {
            (
                libc::getuid(),
                libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
                libc::getauxval(libc::AT_SECURE) != 0,
            )
        };
        privilege_context_from_facts(
            uid,
            euid,
            gid,
            egid,
            at_secure,
            current_thread_capabilities(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateSource {
    ExplicitOverride,
    ConfiguredGpu,
    PackageManager,
    Bundle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeCandidate {
    path: PathBuf,
    source: CandidateSource,
    trust_root: Option<PathBuf>,
    relative_path: Option<PathBuf>,
}

impl RuntimeCandidate {
    fn override_path(path: &OsStr) -> Self {
        Self {
            path: PathBuf::from(path),
            source: CandidateSource::ExplicitOverride,
            trust_root: None,
            relative_path: None,
        }
    }

    fn trusted(source: CandidateSource, root: &str, relative_path: &str) -> Self {
        let trust_root = PathBuf::from(root);
        let relative_path = PathBuf::from(relative_path);
        Self {
            path: trust_root.join(&relative_path),
            source,
            trust_root: Some(trust_root),
            relative_path: Some(relative_path),
        }
    }
}

/// Build the deterministic loader policy without touching the filesystem.
///
/// The unversioned candidates exist only for an explicitly configured GPU
/// provider. Fedora's base runtime intentionally ships no development link,
/// so ordinary CPU/system resolution probes its stable SONAME instead.
fn runtime_candidates(
    provider: &str,
    override_path: Option<&OsStr>,
    privilege: PrivilegeContext,
) -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();
    if privilege == PrivilegeContext::Unprivileged {
        if let Some(path) = override_path.filter(|path| !path.is_empty()) {
            candidates.push(RuntimeCandidate::override_path(path));
        }
    }

    if provider != "cpu" {
        if provider == "rocm" {
            candidates.push(RuntimeCandidate::trusted(
                CandidateSource::ConfiguredGpu,
                "/usr/lib64/rocm/lib",
                EXPECTED_SONAME,
            ));
            candidates.push(RuntimeCandidate::trusted(
                CandidateSource::ConfiguredGpu,
                "/usr/lib/rocm/lib",
                EXPECTED_SONAME,
            ));
        }
        candidates.push(RuntimeCandidate::trusted(
            CandidateSource::ConfiguredGpu,
            "/usr/lib64",
            "libonnxruntime.so",
        ));
        candidates.push(RuntimeCandidate::trusted(
            CandidateSource::ConfiguredGpu,
            "/usr/lib",
            "libonnxruntime.so",
        ));
    }

    candidates.extend([
        RuntimeCandidate::trusted(
            CandidateSource::PackageManager,
            "/usr/lib64",
            EXPECTED_SONAME,
        ),
        RuntimeCandidate::trusted(CandidateSource::PackageManager, "/usr/lib", EXPECTED_SONAME),
        RuntimeCandidate::trusted(
            CandidateSource::Bundle,
            "/usr/lib64/facelock",
            EXPECTED_SONAME,
        ),
        RuntimeCandidate::trusted(
            CandidateSource::Bundle,
            "/usr/lib/facelock",
            EXPECTED_SONAME,
        ),
        // Existing Debian packages use this package-owned compatibility name.
        // It stays behind both stable-SONAME bundle candidates and is still
        // subject to the same root/inode/ELF trust gate.
        RuntimeCandidate::trusted(
            CandidateSource::Bundle,
            "/usr/lib64/facelock",
            "libonnxruntime.so",
        ),
        RuntimeCandidate::trusted(
            CandidateSource::Bundle,
            "/usr/lib/facelock",
            "libonnxruntime.so",
        ),
    ]);
    candidates
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ElfArchitecture {
    X86_64,
    Aarch64,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ElfIdentity {
    architecture: ElfArchitecture,
    soname: Option<String>,
    rpath: Vec<String>,
    runpath: Vec<String>,
}

type DynamicStrings = (Option<String>, Vec<String>, Vec<String>);

fn expected_elf_architecture() -> ElfArchitecture {
    #[cfg(target_arch = "x86_64")]
    {
        ElfArchitecture::X86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        ElfArchitecture::Aarch64
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        ElfArchitecture::Other(std::env::consts::ARCH.to_string())
    }
}

fn dynamic_strings(
    file: &object::read::elf::ElfFile64<'_>,
    data: &[u8],
) -> std::result::Result<DynamicStrings, String> {
    let endian = file.endian();
    let sections = file.elf_section_table();
    let Some((dynamic, string_index)) = sections
        .dynamic(endian, data)
        .map_err(|error| format!("invalid ELF dynamic section: {error}"))?
    else {
        return Err("ELF has no dynamic section".into());
    };
    let strings = sections
        .strings(endian, data, string_index)
        .map_err(|error| format!("invalid ELF dynamic string table: {error}"))?;
    let mut soname = None;
    let mut rpath = Vec::new();
    let mut runpath = Vec::new();
    for entry in dynamic {
        let Some(tag) = entry.tag32(endian) else {
            continue;
        };
        if !matches!(
            tag,
            object::elf::DT_SONAME | object::elf::DT_RPATH | object::elf::DT_RUNPATH
        ) {
            continue;
        }
        let value = entry
            .string(endian, strings)
            .map_err(|error| format!("invalid ELF dynamic string: {error}"))?;
        let value = std::str::from_utf8(value)
            .map_err(|_| "ELF dynamic string is not UTF-8".to_string())?;
        match tag {
            object::elf::DT_SONAME => soname = Some(value.to_string()),
            object::elf::DT_RPATH => rpath.extend(value.split(':').map(str::to_string)),
            object::elf::DT_RUNPATH => runpath.extend(value.split(':').map(str::to_string)),
            _ => {}
        }
    }
    Ok((soname, rpath, runpath))
}

fn inspect_elf(data: &[u8]) -> std::result::Result<ElfIdentity, String> {
    let file = object::File::parse(data).map_err(|error| format!("invalid ELF file: {error}"))?;
    let architecture = match file.architecture() {
        Architecture::X86_64 => ElfArchitecture::X86_64,
        Architecture::Aarch64 => ElfArchitecture::Aarch64,
        other => ElfArchitecture::Other(format!("{other:?}")),
    };
    let object::File::Elf64(elf) = file else {
        return Err("ONNX Runtime must be a 64-bit ELF shared object".into());
    };
    let (soname, rpath, runpath) = dynamic_strings(&elf, data)?;
    Ok(ElfIdentity {
        architecture,
        soname,
        rpath,
        runpath,
    })
}

fn validate_search_path(kind: &str, entries: &[String]) -> std::result::Result<(), String> {
    for entry in entries {
        // Upstream ORT uses exactly $ORIGIN to find package-owned provider
        // libraries beside the validated main runtime. Any expansion,
        // traversal, empty component or unrelated absolute root is rejected.
        if entry != "$ORIGIN" && entry != "${ORIGIN}" {
            return Err(format!("unsafe ONNX Runtime {kind} entry {entry:?}"));
        }
    }
    Ok(())
}

fn validate_elf_identity(identity: &ElfIdentity) -> std::result::Result<(), String> {
    let expected = expected_elf_architecture();
    if identity.architecture != expected {
        return Err(format!(
            "ONNX Runtime architecture mismatch: expected {expected:?}, found {:?}",
            identity.architecture
        ));
    }
    if identity.soname.as_deref() != Some(EXPECTED_SONAME) {
        return Err(format!(
            "ONNX Runtime SONAME mismatch: expected {EXPECTED_SONAME}, found {:?}",
            identity.soname
        ));
    }
    validate_search_path("RPATH", &identity.rpath)?;
    validate_search_path("RUNPATH", &identity.runpath)?;
    Ok(())
}

#[derive(Debug)]
struct OpenedRuntime {
    file: File,
    display_path: PathBuf,
}

impl OpenedRuntime {
    fn mapping_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }
}

fn validate_owner_and_mode(
    path: &Path,
    metadata: &fs::Metadata,
    required_uid: u32,
) -> std::result::Result<(), String> {
    if metadata.uid() != required_uid {
        return Err(format!(
            "{} is owned by uid {}, expected uid {required_uid}",
            path.display(),
            metadata.uid()
        ));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(format!("{} is group- or world-writable", path.display()));
    }
    Ok(())
}

fn validate_directory(path: &Path, required_uid: u32) -> std::result::Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot examine trusted directory {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "trusted directory {} is a symbolic link",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "trusted path {} is not a directory",
            path.display()
        ));
    }
    validate_owner_and_mode(path, &metadata, required_uid)
}

fn validate_root_ancestors(root: &Path, required_uid: u32) -> std::result::Result<(), String> {
    if !root.is_absolute() {
        return Err(format!("trusted root {} is not absolute", root.display()));
    }
    for ancestor in root.ancestors() {
        validate_directory(ancestor, required_uid)?;
    }
    Ok(())
}

fn valid_relative_candidate(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    !bytes.is_empty()
        && !bytes.contains(&0)
        && bytes
            .split(|byte| *byte == b'/')
            .all(|segment| !segment.is_empty() && segment != b"." && segment != b"..")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StableFileIdentity {
    device: u64,
    inode: u64,
    links: u64,
    size: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl From<&fs::Metadata> for StableFileIdentity {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            links: metadata.nlink(),
            size: metadata.len(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

fn validate_stable_file_identity(
    path: &Path,
    before: &StableFileIdentity,
    after: &StableFileIdentity,
) -> std::result::Result<(), String> {
    if before != after {
        return Err(format!("{} changed while it was validated", path.display()));
    }
    Ok(())
}

fn validate_symlink_owner(
    path: &Path,
    metadata: &fs::Metadata,
    required_uid: u32,
) -> std::result::Result<(), String> {
    if !metadata.file_type().is_symlink() {
        return Err(format!("{} is not a symbolic link", path.display()));
    }
    if metadata.uid() != required_uid {
        return Err(format!(
            "symbolic link {} is owned by uid {}, expected uid {required_uid}",
            path.display(),
            metadata.uid()
        ));
    }
    if metadata.nlink() != 1 {
        return Err(format!(
            "symbolic link {} has multiple hard links",
            path.display()
        ));
    }
    Ok(())
}

fn validate_privileged_file_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    required_uid: u32,
    has_file_capability: bool,
) -> std::result::Result<(), String> {
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    validate_owner_and_mode(path, metadata, required_uid)?;
    if metadata.nlink() != 1 {
        return Err(format!("{} has multiple hard links", path.display()));
    }
    if metadata.mode() & 0o6000 != 0 {
        return Err(format!(
            "{} has a set-user-ID or set-group-ID bit",
            path.display()
        ));
    }
    if has_file_capability {
        return Err(format!(
            "{} has a Linux file capability xattr",
            path.display()
        ));
    }
    Ok(())
}

fn has_linux_file_capability(file: &File, path: &Path) -> std::result::Result<bool, String> {
    let name = b"security.capability\0";
    let size = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            0,
        )
    };
    if size >= 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENODATA) | Some(libc::EOPNOTSUPP) => Ok(false),
        _ => Err(format!(
            "cannot inspect Linux file capabilities on {}: {error}",
            path.display()
        )),
    }
}

fn openat_component(parent: &File, name: &OsStr, flags: i32) -> std::io::Result<File> {
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))?;
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), c_name.as_ptr(), flags) };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn read_symlink_descriptor(file: &File) -> std::io::Result<PathBuf> {
    let empty = b"\0";
    let mut bytes = vec![0_u8; 256];
    loop {
        let length = unsafe {
            libc::readlinkat(
                file.as_raw_fd(),
                empty.as_ptr().cast(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if length < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let length = length as usize;
        if length < bytes.len() {
            bytes.truncate(length);
            return Ok(PathBuf::from(OsString::from_vec(bytes)));
        }
        if bytes.len() >= libc::PATH_MAX as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "symbolic link target is too long",
            ));
        }
        bytes.resize((bytes.len() * 2).min(libc::PATH_MAX as usize), 0);
    }
}

fn confined_components(path: &Path) -> std::result::Result<VecDeque<OsString>, String> {
    if !valid_relative_candidate(path) || path.as_os_str().is_empty() {
        return Err("candidate must be a confined relative path".into());
    }
    Ok(path
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect())
}

/// Resolve one package-owned candidate entirely by held descriptors.
///
/// Directory symlinks, absolute link targets, `..`, and links with an
/// untrusted owner are rejected. The only links followed are relative SONAME
/// chains whose every inode remains under the already-opened trusted root.
/// This single implementation is used on kernels both with and without
/// `openat2`, avoiding a weaker ENOSYS policy.
fn open_confined_candidate(
    root: &File,
    relative_path: &Path,
    required_uid: u32,
) -> std::result::Result<File, String> {
    let mut parent = root
        .try_clone()
        .map_err(|error| format!("cannot duplicate trusted root: {error}"))?;
    let mut components = confined_components(relative_path)?;
    let mut followed_links = 0_u8;
    let mut walked = PathBuf::new();

    while let Some(component) = components.pop_front() {
        let is_last = components.is_empty();
        let display_path = walked.join(&component);
        let entry = openat_component(
            &parent,
            &component,
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
        .map_err(|error| format!("cannot inspect {}: {error}", display_path.display()))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("cannot examine {}: {error}", display_path.display()))?;

        if metadata.file_type().is_symlink() {
            if !is_last {
                return Err(format!(
                    "directory component {} is a symbolic link",
                    display_path.display()
                ));
            }
            validate_symlink_owner(&display_path, &metadata, required_uid)?;
            followed_links = followed_links.saturating_add(1);
            if followed_links > 16 {
                return Err(format!(
                    "symbolic link chain for {} is too deep",
                    relative_path.display()
                ));
            }
            let target = read_symlink_descriptor(&entry).map_err(|error| {
                format!(
                    "cannot read symbolic link {}: {error}",
                    display_path.display()
                )
            })?;
            let mut target_components = confined_components(&target).map_err(|_| {
                format!(
                    "symbolic link {} has an escaping or absolute target {}",
                    display_path.display(),
                    target.display()
                )
            })?;
            target_components.append(&mut components);
            components = target_components;
            continue;
        }

        if metadata.is_dir() {
            if is_last {
                return Err(format!("{} is not a regular file", display_path.display()));
            }
            validate_owner_and_mode(&display_path, &metadata, required_uid)?;
            parent = entry;
            walked.push(component);
            continue;
        }

        if !metadata.is_file() {
            return Err(format!("{} is not a regular file", display_path.display()));
        }
        if !is_last {
            return Err(format!(
                "non-directory component {} appears before the candidate",
                display_path.display()
            ));
        }

        let file = openat_component(
            &parent,
            &component,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
        .map_err(|error| format!("cannot open {}: {error}", display_path.display()))?;
        let opened_metadata = file
            .metadata()
            .map_err(|error| format!("cannot examine {}: {error}", display_path.display()))?;
        validate_stable_file_identity(
            &display_path,
            &StableFileIdentity::from(&metadata),
            &StableFileIdentity::from(&opened_metadata),
        )?;
        return Ok(file);
    }

    Err("candidate path resolved to no file".into())
}

fn validate_opened_elf(
    file: &File,
    display_path: &Path,
    expected: Option<&StableFileIdentity>,
) -> std::result::Result<StableFileIdentity, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot examine {}: {error}", display_path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", display_path.display()));
    }
    if metadata.len() > MAX_RUNTIME_BYTES {
        return Err(format!("{} is unreasonably large", display_path.display()));
    }
    let before = StableFileIdentity::from(&metadata);
    if let Some(expected) = expected {
        validate_stable_file_identity(display_path, expected, &before)?;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.try_clone()
        .map_err(|error| format!("cannot duplicate {}: {error}", display_path.display()))?
        .take(MAX_RUNTIME_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", display_path.display()))?;
    let after_metadata = file
        .metadata()
        .map_err(|error| format!("cannot re-examine {}: {error}", display_path.display()))?;
    let after = StableFileIdentity::from(&after_metadata);
    validate_stable_file_identity(display_path, &before, &after)?;
    if bytes.len() as u64 != after.size {
        return Err(format!(
            "{} changed while it was read",
            display_path.display()
        ));
    }
    let identity = inspect_elf(&bytes)?;
    validate_elf_identity(&identity)?;
    Ok(after)
}

fn open_privileged_candidate(
    trust_root: &Path,
    relative_path: &Path,
    required_uid: u32,
) -> std::result::Result<OpenedRuntime, String> {
    let c_root = CString::new(trust_root.as_os_str().as_bytes())
        .map_err(|_| format!("trusted root {} contains NUL", trust_root.display()))?;
    let root_descriptor = unsafe {
        libc::open(
            c_root.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if root_descriptor < 0 {
        let error = std::io::Error::last_os_error();
        let kind = fs::symlink_metadata(trust_root)
            .ok()
            .filter(|metadata| metadata.file_type().is_symlink())
            .map(|_| "trusted root is a symbolic link: ")
            .unwrap_or("");
        return Err(format!(
            "cannot open trusted root {}: {kind}{error}",
            trust_root.display()
        ));
    }
    let root = unsafe { File::from_raw_fd(root_descriptor) };
    let root_metadata = root.metadata().map_err(|error| {
        format!(
            "cannot examine trusted root {}: {error}",
            trust_root.display()
        )
    })?;
    if !root_metadata.is_dir() {
        return Err(format!(
            "trusted root {} is not a directory",
            trust_root.display()
        ));
    }
    validate_owner_and_mode(trust_root, &root_metadata, required_uid)?;
    let file = open_confined_candidate(&root, relative_path, required_uid).map_err(|error| {
        format!(
            "candidate {} escapes or cannot be opened beneath {}: {error}",
            relative_path.display(),
            trust_root.display()
        )
    })?;
    let display_path = trust_root.join(relative_path);
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot examine {}: {error}", display_path.display()))?;
    let capability = has_linux_file_capability(&file, &display_path)?;
    validate_privileged_file_metadata(&display_path, &metadata, required_uid, capability)?;
    let expected = StableFileIdentity::from(&metadata);
    let validated = validate_opened_elf(&file, &display_path, Some(&expected))?;
    let final_metadata = file
        .metadata()
        .map_err(|error| format!("cannot re-examine {}: {error}", display_path.display()))?;
    validate_stable_file_identity(
        &display_path,
        &validated,
        &StableFileIdentity::from(&final_metadata),
    )?;
    let final_capability = has_linux_file_capability(&file, &display_path)?;
    validate_privileged_file_metadata(
        &display_path,
        &final_metadata,
        required_uid,
        final_capability,
    )?;
    Ok(OpenedRuntime { file, display_path })
}

fn open_unprivileged_override(path: &Path) -> std::result::Result<OpenedRuntime, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "cannot open explicit ONNX Runtime {}: {error}",
            path.display()
        )
    })?;
    validate_opened_elf(&file, path, None)?;
    Ok(OpenedRuntime {
        file,
        display_path: path.to_path_buf(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeProvider {
    Auto,
    Configured(ProviderKind),
}

impl RuntimeProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Configured(kind) => kind.as_str(),
        }
    }
}

/// Load the ONNX Runtime shared library.
///
/// Search order:
/// 1. `ORT_DYLIB_PATH` in an unprivileged process only
/// 2. A configured GPU provider's trusted system location
/// 3. Package-manager-owned stable-SONAME paths
/// 4. Facelock's package-owned CPU bundle
fn load_ort(provider: RuntimeProvider) -> std::result::Result<(), String> {
    use std::sync::OnceLock;

    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| initialize_ort(provider)).clone()
}

fn initialize_ort(provider: RuntimeProvider) -> std::result::Result<(), String> {
    let privilege = PrivilegeContext::current();
    let override_path = if privilege == PrivilegeContext::Unprivileged {
        std::env::var_os("ORT_DYLIB_PATH")
    } else {
        None
    };
    let candidates = runtime_candidates(provider.as_str(), override_path.as_deref(), privilege);
    let mut failures = Vec::new();

    for candidate in candidates {
        match fs::symlink_metadata(&candidate.path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(
                    "Could not examine ONNX Runtime candidate {}: {error}",
                    candidate.path.display()
                );
                failures.push(format!("{}: {error}", candidate.path.display()));
                continue;
            }
        }
        let opened = if candidate.source == CandidateSource::ExplicitOverride {
            open_unprivileged_override(&candidate.path)
        } else {
            match (
                candidate.trust_root.as_deref(),
                candidate.relative_path.as_deref(),
            ) {
                (Some(trust_root), Some(relative_path)) => validate_root_ancestors(trust_root, 0)
                    .and_then(|()| open_privileged_candidate(trust_root, relative_path, 0)),
                _ => Err("trusted ONNX Runtime candidate has no confinement root".into()),
            }
        };
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) => {
                tracing::warn!(
                    "Rejected ONNX Runtime candidate {}: {error}",
                    candidate.path.display()
                );
                failures.push(format!("{}: {error}", candidate.path.display()));
                continue;
            }
        };
        let mapping_path = opened.mapping_path();
        match ort::init_from(&mapping_path) {
            Ok(builder) => {
                builder.commit();
                tracing::info!(
                    "Loaded {:?} ONNX Runtime from {}",
                    candidate.source,
                    opened.display_path.display()
                );
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(
                    "Validated ONNX Runtime candidate {} failed to load: {error}",
                    opened.display_path.display()
                );
                failures.push(format!("{}: {error}", opened.display_path.display()));
            }
        }
    }

    let detail = if failures.is_empty() {
        String::new()
    } else {
        format!(" Rejected candidates: {}", failures.join("; "))
    };
    Err(format!(
        "Could not find a trusted {EXPECTED_SONAME}. Ensure the package is installed correctly. \
         ORT_DYLIB_PATH is accepted only in an unprivileged process.{detail}"
    ))
}

/// Ensure the ONNX Runtime shared library is loaded before any session builders
/// are created. Some ORT builds can deadlock if session construction re-enters
/// runtime initialization.
pub(crate) fn ensure_runtime_loaded(provider: &str) -> std::result::Result<(), String> {
    with_valid_provider(provider, |kind| load_ort(RuntimeProvider::Configured(kind))).map(|_| ())
}

/// An execution provider facelock knows how to register.
///
/// This enum is the single source of truth for the set of valid provider
/// names. Both `register_execution_provider` (which turns a config string into
/// an ORT session) and `detect_execution_provider` (which picks one for
/// `--execution-provider=auto`) go through it, so the two cannot drift into
/// disagreeing about which names are legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Cpu,
    Cuda,
    Rocm,
    OpenVino,
}

impl ProviderKind {
    /// Every provider, in the order the "valid values" error message lists them.
    ///
    /// Public so user-facing text that enumerates providers can be checked
    /// against it instead of hardcoding a parallel list — see
    /// [`ProviderKind::all_names`].
    pub const ALL: [ProviderKind; 4] = [
        ProviderKind::Cpu,
        ProviderKind::Cuda,
        ProviderKind::Rocm,
        ProviderKind::OpenVino,
    ];

    /// The config-file spellings of every provider, in message order.
    pub fn all_names() -> impl Iterator<Item = &'static str> {
        ProviderKind::ALL.into_iter().map(ProviderKind::as_str)
    }

    /// GPU providers in auto-selection priority order: the first one the
    /// installed ONNX Runtime actually supports wins. CPU is not listed —
    /// it is the fallback, and is always available.
    const AUTO_PRIORITY: [ProviderKind; 3] = [
        ProviderKind::Cuda,
        ProviderKind::Rocm,
        ProviderKind::OpenVino,
    ];

    /// The config-file / CLI spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Cpu => "cpu",
            ProviderKind::Cuda => "cuda",
            ProviderKind::Rocm => "rocm",
            ProviderKind::OpenVino => "openvino",
        }
    }

    /// Parse a config-file / CLI value. `None` for anything unrecognized.
    pub fn parse(name: &str) -> Option<Self> {
        ProviderKind::ALL.into_iter().find(|k| k.as_str() == name)
    }

    /// Whether the loaded ONNX Runtime was *built* with support for this
    /// provider. Answers "could this ORT ever use CUDA", not "will this
    /// specific model run on it" — see `ExecutionProvider::is_available`.
    ///
    /// A load or FFI failure is reported as "not available" rather than an
    /// error: an unavailable provider and an unanswerable question lead to the
    /// same decision, and detection must always be able to fall back to CPU.
    fn is_available(self) -> bool {
        use ort::ep::ExecutionProvider;

        // `ort::api()` panics if the dylib cannot be loaded. `load_ort` uses a
        // `Once`, so a failure on a previous call is not re-reported here.
        // Detection is best-effort by contract, so contain the panic rather
        // than let it escape into the setup wizard.
        std::panic::catch_unwind(|| match self {
            ProviderKind::Cpu => Ok(true),
            ProviderKind::Cuda => ort::ep::CUDA::default().is_available(),
            ProviderKind::Rocm => ort::ep::ROCm::default().is_available(),
            ProviderKind::OpenVino => ort::ep::OpenVINO::default().is_available(),
        })
        .unwrap_or_else(|_| {
            tracing::warn!(
                "ONNX Runtime panicked while querying {} availability",
                self.as_str()
            );
            Ok(false)
        })
        .unwrap_or_else(|e| {
            tracing::warn!("Could not query {} availability: {e}", self.as_str());
            false
        })
    }
}

/// Result of `detect_execution_provider`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDetection {
    /// The chosen provider. Always a value `register_execution_provider` accepts.
    pub provider: ProviderKind,
    /// GPU providers the installed ONNX Runtime reports it was built with,
    /// in priority order. Empty on a CPU-only ORT build.
    pub available: Vec<ProviderKind>,
}

impl ProviderDetection {
    /// One line explaining the choice, suitable for the setup wizard. The
    /// point of `auto` is that a user on a CPU-only ORT learns *why* they got
    /// CPU rather than silently getting it.
    pub fn explain(&self) -> String {
        if self.available.is_empty() {
            "the installed ONNX Runtime has no GPU execution providers compiled in; \
             selecting cpu"
                .to_string()
        } else {
            let names: Vec<&str> = self.available.iter().map(|k| k.as_str()).collect();
            format!(
                "the installed ONNX Runtime supports {}; selecting {}",
                names.join(", "),
                self.provider.as_str()
            )
        }
    }
}

/// Pick a provider from a set of known-available GPU providers.
///
/// Split out from the ORT query so the priority rule (cuda > rocm > openvino >
/// cpu) is testable without a GPU-enabled ONNX Runtime on the test machine.
fn select_by_priority(available: &[ProviderKind]) -> ProviderKind {
    ProviderKind::AUTO_PRIORITY
        .into_iter()
        .find(|k| available.contains(k))
        .unwrap_or(ProviderKind::Cpu)
}

/// Detect the best execution provider the installed ONNX Runtime can use.
///
/// Selection order is cuda > rocm > openvino > cpu. Availability is a property
/// of the *ORT build*, not of the hardware: the bundled CPU-only runtime
/// reports no GPU providers even on a machine with an NVIDIA card, while a
/// system `onnxruntime-opt-cuda` reports CUDA. That is the distinction this
/// exists to make — a driver device node proves nothing about what ORT can do.
///
/// Errors only when the ONNX Runtime shared library cannot be loaded at all,
/// in which case nothing can be queried. Never panics.
pub fn detect_execution_provider() -> std::result::Result<ProviderDetection, String> {
    // Auto-detection must inspect a package-manager GPU runtime before the
    // CPU-only bundle, just like an explicitly configured GPU provider.
    load_ort(RuntimeProvider::Auto)?;

    let available: Vec<ProviderKind> = ProviderKind::AUTO_PRIORITY
        .into_iter()
        .filter(|k| k.is_available())
        .collect();

    let provider = select_by_priority(&available);
    tracing::info!("Detected execution provider: {}", provider.as_str());
    Ok(ProviderDetection {
        provider,
        available,
    })
}

/// Register an execution provider on the session builder based on config.
///
/// All providers load the ONNX Runtime shared library at runtime.
/// GPU providers (cuda, rocm, openvino) require a system ORT built
/// with the corresponding support — install the appropriate package
/// (e.g. `onnxruntime-opt-cuda`) and it will be picked up automatically.
fn with_valid_provider<T>(
    provider: &str,
    loader: impl FnOnce(ProviderKind) -> std::result::Result<T, String>,
) -> std::result::Result<(ProviderKind, T), String> {
    let Some(kind) = ProviderKind::parse(provider) else {
        let valid: Vec<&str> = ProviderKind::ALL.iter().map(|kind| kind.as_str()).collect();
        return Err(format!(
            "Unknown execution provider '{provider}'. Valid values: {}",
            valid.join(", ")
        ));
    };
    let loaded = loader(kind)?;
    Ok((kind, loaded))
}

pub(crate) fn register_execution_provider(
    builder: SessionBuilder,
    provider: &str,
) -> std::result::Result<SessionBuilder, String> {
    let (kind, ()) =
        with_valid_provider(provider, |kind| load_ort(RuntimeProvider::Configured(kind)))?;

    tracing::info!("Using execution provider: {provider}");
    match kind {
        ProviderKind::Cpu => Ok(builder),

        ProviderKind::Cuda => builder
            .with_execution_providers([ort::ep::CUDA::default().build()])
            .map_err(|e| format!("CUDA execution provider: {e}")),

        ProviderKind::Rocm => builder
            .with_execution_providers([ort::ep::ROCm::default().build()])
            .map_err(|e| format!("ROCm execution provider: {e}")),

        ProviderKind::OpenVino => builder
            .with_execution_providers([ort::ep::OpenVINO::default().build()])
            .map_err(|e| format!("OpenVINO execution provider: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    fn candidate_paths(candidates: &[RuntimeCandidate]) -> Vec<PathBuf> {
        candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect()
    }

    fn capability_status(values: [&str; 4]) -> String {
        format!(
            "Name:\tfacelock\nCapInh:\t{}\nCapPrm:\t{}\nCapEff:\t{}\nCapBnd:\t000001ffffffffff\nCapAmb:\t{}\n",
            values[0], values[1], values[2], values[3]
        )
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "facelock-provider-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("create scratch directory");
        path
    }

    // -- runtime candidate policy -----------------------------------------

    #[test]
    #[ignore = "run in a disposable nonroot container with CAP_CHOWN"]
    fn current_nonroot_capability_thread_is_privileged() {
        let status =
            fs::read_to_string(CALLING_THREAD_STATUS_PATH).expect("read calling-thread status");
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(str::trim)
                .expect("required process status field")
        };
        assert_eq!(field("Uid:"), "1000\t1000\t1000\t1000");
        assert_eq!(field("Gid:"), "1000\t1000\t1000\t1000");
        assert_eq!(field("CapEff:"), "0000000000000001");
        assert_eq!(field("CapAmb:"), "0000000000000001");
        assert_eq!(unsafe { libc::getauxval(libc::AT_SECURE) }, 0);

        let privilege = PrivilegeContext::current();
        assert_eq!(privilege, PrivilegeContext::Privileged);
        assert!(
            runtime_candidates(
                "cpu",
                Some(OsStr::new("/tmp/attacker/libonnxruntime.so.1")),
                privilege,
            )
            .iter()
            .all(|candidate| candidate.source != CandidateSource::ExplicitOverride)
        );
    }

    #[test]
    fn nonroot_process_with_chown_capability_is_privileged() {
        let status = capability_status([
            "0000000000000001",
            "0000000000000001",
            "0000000000000001",
            "0000000000000001",
        ]);
        let capabilities = parse_process_capabilities(&status);

        // Reproduces the disposable Fedora container report exactly: uid and
        // euid 1000, gid and egid 0, AT_SECURE clear, CHOWN effective/ambient.
        assert_eq!(
            privilege_context_from_facts(1000, 1000, 0, 0, false, capabilities.clone()),
            PrivilegeContext::Privileged
        );
        // Keep the capability branch independently covered rather than
        // allowing the root group in the exact report to mask a regression.
        assert_eq!(
            privilege_context_from_facts(1000, 1000, 1000, 1000, false, capabilities),
            PrivilegeContext::Privileged
        );
    }

    #[test]
    fn every_active_capability_set_is_privileged() {
        for active in 0..4 {
            let mut values = ["0000000000000000"; 4];
            values[active] = "0000000000000001";
            assert_eq!(
                parse_process_capabilities(&capability_status(values)),
                Ok(true),
                "capability field index {active}"
            );
        }
    }

    #[test]
    fn capability_inspection_reads_the_calling_thread_when_leader_differs() {
        let leader = capability_status([
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
        ]);
        let worker = capability_status([
            "0000000000000000",
            "0000000000000001",
            "0000000000000001",
            "0000000000000000",
        ]);
        let seen_path = Cell::new(None);

        let capabilities = current_thread_capabilities_with(|path| {
            seen_path.set(Some(path.to_path_buf()));
            match path.to_str() {
                Some("/proc/self/status") => Ok(leader),
                Some("/proc/thread-self/status") => Ok(worker),
                _ => Err(format!("unexpected status path {}", path.display())),
            }
        });

        assert_eq!(
            seen_path.into_inner().as_deref(),
            Some(Path::new("/proc/thread-self/status"))
        );
        assert_eq!(capabilities, Ok(true));
    }

    #[test]
    fn zero_capability_sets_are_unprivileged_for_equal_nonroot_ids() {
        let capabilities = parse_process_capabilities(&capability_status([
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
            "0000000000000000",
        ]));

        assert_eq!(capabilities, Ok(false));
        assert_eq!(
            privilege_context_from_facts(1000, 1000, 1000, 1000, false, capabilities),
            PrivilegeContext::Unprivileged
        );
    }

    #[test]
    fn root_real_or_effective_group_is_privileged() {
        for (gid, egid) in [(0, 0), (0, 1000), (1000, 0)] {
            assert_eq!(
                privilege_context_from_facts(1000, 1000, gid, egid, false, Ok(false)),
                PrivilegeContext::Privileged,
                "gid={gid}, egid={egid}"
            );
        }
    }

    #[test]
    fn malformed_or_unreadable_capability_status_fails_privileged() {
        let malformed = [
            "CapInh:\t0\nCapPrm:\t0\nCapEff:\t0\nCapBnd:\tffff\n",
            "CapInh:\t0\nCapPrm:\t0\nCapEff:\t0\nCapEff:\t0\nCapAmb:\t0\n",
            "CapInh:\t0\nCapPrm:\t0x0\nCapEff:\t0\nCapAmb:\t0\n",
            "CapInh:\t0\nCapPrm:\t\nCapEff:\t0\nCapAmb:\t0\n",
        ];

        for status in malformed {
            let capabilities = parse_process_capabilities(status);
            assert!(capabilities.is_err(), "accepted {status:?}");
            let error = capabilities.as_ref().unwrap_err();
            assert!(
                error.contains(CALLING_THREAD_STATUS_PATH),
                "stale capability status path: {error}"
            );
            assert_eq!(
                privilege_context_from_facts(1000, 1000, 1000, 1000, false, capabilities),
                PrivilegeContext::Privileged
            );
        }
        assert_eq!(
            privilege_context_from_facts(
                1000,
                1000,
                1000,
                1000,
                false,
                Err(format!("cannot read {CALLING_THREAD_STATUS_PATH}")),
            ),
            PrivilegeContext::Privileged
        );
    }

    #[test]
    fn invalid_provider_is_rejected_before_loader_seam() {
        let loader_touched = Cell::new(false);
        let error = with_valid_provider("bogus", |_| {
            loader_touched.set(true);
            Ok(())
        })
        .unwrap_err();

        assert!(!loader_touched.get());
        assert!(
            error.contains("Unknown execution provider 'bogus'"),
            "{error}"
        );
    }

    #[test]
    fn internal_auto_runtime_preserves_the_gpu_candidate_lane() {
        let candidates = runtime_candidates(
            RuntimeProvider::Auto.as_str(),
            None,
            PrivilegeContext::Privileged,
        );

        assert_eq!(candidates[0].source, CandidateSource::ConfiguredGpu);
        assert_eq!(candidates[1].source, CandidateSource::ConfiguredGpu);
        assert_eq!(candidates[2].source, CandidateSource::PackageManager);
    }

    #[test]
    fn unprivileged_override_precedes_configured_system_and_bundle_candidates() {
        let candidates = runtime_candidates(
            "rocm",
            Some(OsStr::new("/home/alice/libonnxruntime.so.1")),
            PrivilegeContext::Unprivileged,
        );

        assert_eq!(candidates[0].source, CandidateSource::ExplicitOverride);
        assert_eq!(
            candidates[0].path,
            Path::new("/home/alice/libonnxruntime.so.1")
        );
        assert_eq!(candidates[1].source, CandidateSource::ConfiguredGpu);
        assert!(
            candidates
                .iter()
                .position(|candidate| candidate.source == CandidateSource::PackageManager)
                < candidates
                    .iter()
                    .position(|candidate| candidate.source == CandidateSource::Bundle)
        );
    }

    #[test]
    fn privileged_context_ignores_environment_override_and_usr_local() {
        let candidates = runtime_candidates(
            "cuda",
            Some(OsStr::new("/tmp/attacker/libonnxruntime.so.1")),
            PrivilegeContext::Privileged,
        );
        let paths = candidate_paths(&candidates);

        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.source != CandidateSource::ExplicitOverride)
        );
        assert!(paths.iter().all(|path| !path.starts_with("/usr/local")));
        assert!(paths.iter().all(|path| !path.starts_with("/tmp")));
    }

    #[test]
    fn cpu_uses_versioned_package_manager_soname_before_bundle() {
        let candidates = runtime_candidates("cpu", None, PrivilegeContext::Privileged);
        let paths = candidate_paths(&candidates);

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/usr/lib64/libonnxruntime.so.1"),
                PathBuf::from("/usr/lib/libonnxruntime.so.1"),
                PathBuf::from("/usr/lib64/facelock/libonnxruntime.so.1"),
                PathBuf::from("/usr/lib/facelock/libonnxruntime.so.1"),
                PathBuf::from("/usr/lib64/facelock/libonnxruntime.so"),
                PathBuf::from("/usr/lib/facelock/libonnxruntime.so"),
            ]
        );
    }

    // -- ELF identity policy ----------------------------------------------

    #[test]
    fn rejects_corrupt_elf_before_mapping() {
        let error = inspect_elf(b"not an ELF shared object").unwrap_err();
        assert!(error.contains("ELF"), "{error}");
    }

    #[test]
    fn rejects_wrong_architecture() {
        let error = validate_elf_identity(&ElfIdentity {
            architecture: ElfArchitecture::Other("i386".into()),
            soname: Some("libonnxruntime.so.1".into()),
            rpath: vec![],
            runpath: vec!["$ORIGIN".into()],
        })
        .unwrap_err();
        assert!(error.contains("architecture"), "{error}");
    }

    #[test]
    fn rejects_wrong_soname() {
        let error = validate_elf_identity(&ElfIdentity {
            architecture: expected_elf_architecture(),
            soname: Some("libonnxruntime.so.2".into()),
            rpath: vec![],
            runpath: vec![],
        })
        .unwrap_err();
        assert!(error.contains("SONAME"), "{error}");
    }

    #[test]
    fn rejects_unsafe_rpath_and_runpath() {
        for entry in [".", "../lib", "$ORIGIN/..", "/tmp/ort", ""] {
            let error = validate_elf_identity(&ElfIdentity {
                architecture: expected_elf_architecture(),
                soname: Some("libonnxruntime.so.1".into()),
                rpath: vec![],
                runpath: vec![entry.into()],
            })
            .unwrap_err();
            assert!(error.contains("RUNPATH"), "entry={entry:?}: {error}");
        }
    }

    #[test]
    fn permits_exact_origin_runpath_in_a_trusted_directory() {
        validate_elf_identity(&ElfIdentity {
            architecture: expected_elf_architecture(),
            soname: Some("libonnxruntime.so.1".into()),
            rpath: vec![],
            runpath: vec!["$ORIGIN".into()],
        })
        .unwrap();
    }

    // -- privileged path/inode policy -------------------------------------

    #[test]
    fn relative_candidate_requires_exact_lexical_canonical_form() {
        for rejected in [
            "a/./b",
            "a//b",
            "a/b/",
            "",
            "/absolute",
            "../outside",
            "a/../b",
            "./a",
        ] {
            assert!(
                !valid_relative_candidate(Path::new(rejected)),
                "accepted non-canonical candidate {rejected:?}"
            );
        }

        assert!(valid_relative_candidate(Path::new("libonnxruntime.so.1")));
        assert!(valid_relative_candidate(Path::new("providers/cuda.so")));
        assert!(!valid_relative_candidate(Path::new(OsStr::from_bytes(
            b"providers/nul-\0name.so"
        ))));
        assert!(valid_relative_candidate(Path::new(OsStr::from_bytes(
            b"providers/non-utf8-\xff.so"
        ))));
    }

    #[test]
    fn rejects_symlink_that_escapes_the_approved_root() {
        let root = scratch_dir("escape");
        let outside = root.with_extension("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("libonnxruntime.so.1"), b"bad").unwrap();
        symlink(
            outside.join("libonnxruntime.so.1"),
            root.join("libonnxruntime.so.1"),
        )
        .unwrap();

        let error = open_privileged_candidate(
            &root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("escape"), "{error}");

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn rejects_writable_parent_before_reading_candidate() {
        let root = scratch_dir("writable-parent");
        let writable = root.join("writable");
        fs::create_dir(&writable).unwrap();
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
        fs::write(writable.join("libonnxruntime.so.1"), b"bad").unwrap();

        let error = open_privileged_candidate(
            &root,
            Path::new("writable/libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("writable"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_writable_runtime_inode_before_mapping() {
        let root = scratch_dir("writable-inode");
        let runtime = root.join("libonnxruntime.so.1");
        fs::write(&runtime, b"bad").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o666)).unwrap();

        let error = open_privileged_candidate(
            &root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("writable"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_hard_linked_runtime_inode_before_mapping() {
        let root = scratch_dir("hard-linked-inode");
        let runtime = root.join("libonnxruntime.so.1");
        fs::copy(std::env::current_exe().unwrap(), &runtime).unwrap();
        fs::hard_link(&runtime, root.join("second-name")).unwrap();

        let error = open_privileged_candidate(
            &root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("multiple hard links"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_setid_runtime_inode_before_mapping() {
        let root = scratch_dir("setid-inode");
        let runtime = root.join("libonnxruntime.so.1");
        fs::copy(std::env::current_exe().unwrap(), &runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o6755)).unwrap();

        let error = open_privileged_candidate(
            &root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("set-user-ID or set-group-ID"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_symlinked_trust_root() {
        let parent = scratch_dir("symlinked-root");
        let actual_root = parent.join("actual");
        let trust_root = parent.join("trusted");
        fs::create_dir(&actual_root).unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            actual_root.join("libonnxruntime.so.1"),
        )
        .unwrap();
        symlink(&actual_root, &trust_root).unwrap();

        let error = open_privileged_candidate(
            &trust_root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&parent).unwrap().uid(),
        )
        .unwrap_err();
        assert!(
            error.contains("trusted root") && error.contains("symlink"),
            "{error}"
        );

        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn descriptor_walker_rejects_symlink_escape_even_when_target_returns_beneath_root() {
        let parent = scratch_dir("escape-return");
        let root = parent.join("root");
        let outside = parent.join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            root.join("libonnxruntime.so.1.20.1"),
        )
        .unwrap();
        symlink(
            "../outside/../root/libonnxruntime.so.1.20.1",
            root.join("libonnxruntime.so.1"),
        )
        .unwrap();
        let root_fd = File::open(&root).unwrap();

        let error = open_confined_candidate(
            &root_fd,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("escaping or absolute"), "{error}");

        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn fifo_candidate_is_rejected_without_blocking() {
        let root = scratch_dir("fifo");
        let runtime = root.join("libonnxruntime.so.1");
        let c_runtime = CString::new(runtime.as_os_str().as_bytes()).unwrap();
        let status = unsafe { libc::mkfifo(c_runtime.as_ptr(), 0o600) };
        assert_eq!(status, 0, "{}", std::io::Error::last_os_error());

        let error = open_privileged_candidate(
            &root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap_err();
        assert!(error.contains("not a regular file"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_runtime_with_linux_file_capability_before_mapping() {
        let root = scratch_dir("file-capability");
        let runtime = root.join("libonnxruntime.so.1");
        fs::copy(std::env::current_exe().unwrap(), &runtime).unwrap();
        let metadata = fs::metadata(&runtime).unwrap();

        let error = validate_privileged_file_metadata(&runtime, &metadata, metadata.uid(), true)
            .unwrap_err();
        assert!(error.contains("file capability"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_opened_inode_metadata_change_during_validation() {
        let root = scratch_dir("metadata-race");
        let runtime = root.join("libonnxruntime.so.1");
        fs::copy(std::env::current_exe().unwrap(), &runtime).unwrap();
        let before = StableFileIdentity::from(&fs::metadata(&runtime).unwrap());
        fs::hard_link(&runtime, root.join("second-name")).unwrap();
        let after = StableFileIdentity::from(&fs::metadata(&runtime).unwrap());

        let error = validate_stable_file_identity(&runtime, &before, &after).unwrap_err();
        assert!(error.contains("changed while it was validated"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_symlink_owned_by_unapproved_uid() {
        let root = scratch_dir("link-owner");
        let link = root.join("libonnxruntime.so.1");
        symlink("libonnxruntime.so.1.20.1", &link).unwrap();
        let metadata = fs::symlink_metadata(&link).unwrap();

        let error = validate_symlink_owner(&link, &metadata, metadata.uid() + 1).unwrap_err();
        assert!(
            error.contains("symbolic link") && error.contains("owned"),
            "{error}"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn descriptor_walker_opens_a_safe_soname_symlink() {
        let root = scratch_dir("descriptor-walker");
        fs::copy(
            std::env::current_exe().unwrap(),
            root.join("libonnxruntime.so.1.20.1"),
        )
        .unwrap();
        symlink("libonnxruntime.so.1.20.1", root.join("libonnxruntime.so.1")).unwrap();
        let root_fd = File::open(&root).unwrap();

        let opened = open_confined_candidate(
            &root_fd,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        )
        .unwrap();
        let resolved = fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd())).unwrap();

        assert_eq!(resolved, root.join("libonnxruntime.so.1.20.1"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn valid_elf_without_ort_soname_is_rejected() {
        let root = scratch_dir("descriptor");
        let path = root.join("libonnxruntime.so.1");
        fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
        let opened = open_privileged_candidate(
            &root,
            Path::new("libonnxruntime.so.1"),
            fs::metadata(&root).unwrap().uid(),
        );

        // The executable is valid ELF but intentionally has no ORT SONAME;
        // opening must therefore reach the ELF gate, never map the path.
        let error = opened.unwrap_err();
        assert!(error.contains("SONAME"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    /// COPR invokes this ignored test explicitly after installing Fedora's
    /// runtime-only `onnxruntime` package. The 130-byte model is from upstream
    /// ORT commit 5c1b7ccbff7e5141c1da7a9d963d660e5741c319 and is checked
    /// before a real session is created; `onnxruntime-devel` is not needed.
    #[test]
    #[ignore]
    fn live_runtime_creates_session_from_checksum_pinned_minimal_model() {
        use sha2::{Digest, Sha256};

        const MODEL_SHA256: &str =
            "71f431c4e9321ec6fbeb158d02ed240459a7dcc98673fa79a4f439ce42efaf10";
        let model_path = PathBuf::from(
            std::env::var_os("FACELOCK_ORT_SMOKE_MODEL")
                .expect("FACELOCK_ORT_SMOKE_MODEL must name the pinned test fixture"),
        );
        let bytes = fs::read(&model_path).expect("read pinned ORT smoke model");
        let digest = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(digest, MODEL_SHA256);

        ensure_runtime_loaded("cpu").expect("load the packaged ONNX Runtime");
        let mut builder = ort::session::Session::builder()
            .expect("create ORT session builder")
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
            .expect("use an optimization level supported by ORT 1.20");
        let session = builder
            .commit_from_file(&model_path)
            .expect("create a real ORT session from the pinned model");
        let _session = std::mem::ManuallyDrop::new(session);
    }

    // -- priority rule ------------------------------------------------------
    //
    // Pure over a set of available providers, so these run identically on a
    // CPU-only CI box and on a CUDA workstation.

    #[test]
    fn cuda_wins_over_rocm() {
        assert_eq!(
            select_by_priority(&[ProviderKind::Cuda, ProviderKind::Rocm]),
            ProviderKind::Cuda
        );
    }

    #[test]
    fn rocm_wins_over_openvino() {
        assert_eq!(
            select_by_priority(&[ProviderKind::Rocm, ProviderKind::OpenVino]),
            ProviderKind::Rocm
        );
    }

    #[test]
    fn openvino_alone_is_selected() {
        assert_eq!(
            select_by_priority(&[ProviderKind::OpenVino]),
            ProviderKind::OpenVino
        );
    }

    #[test]
    fn nothing_available_falls_back_to_cpu() {
        assert_eq!(select_by_priority(&[]), ProviderKind::Cpu);
    }

    #[test]
    fn everything_available_selects_cuda() {
        assert_eq!(
            select_by_priority(&[
                ProviderKind::Cuda,
                ProviderKind::Rocm,
                ProviderKind::OpenVino
            ]),
            ProviderKind::Cuda
        );
    }

    /// The priority list must not accidentally include CPU: CPU is the
    /// fallback, and listing it would make every other arm unreachable if it
    /// were ever placed first.
    #[test]
    fn auto_priority_is_gpu_only_and_ordered() {
        assert_eq!(
            ProviderKind::AUTO_PRIORITY,
            [
                ProviderKind::Cuda,
                ProviderKind::Rocm,
                ProviderKind::OpenVino
            ]
        );
        assert!(!ProviderKind::AUTO_PRIORITY.contains(&ProviderKind::Cpu));
    }

    // -- name round-trip ----------------------------------------------------

    /// Guards the two lists against drifting: anything detection can select
    /// must parse back to a kind `register_execution_provider` handles.
    #[test]
    fn every_kind_round_trips_through_parse() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::parse(kind.as_str()), Some(kind), "{kind:?}");
        }
        // Anything detection can return is one of ALL, by construction of
        // select_by_priority, but assert it over the priority list too.
        for kind in ProviderKind::AUTO_PRIORITY {
            assert!(ProviderKind::ALL.contains(&kind), "{kind:?}");
        }
    }

    #[test]
    fn unknown_provider_names_do_not_parse() {
        for name in ["", "CPU", "gpu", "tensorrt", "cpu "] {
            assert_eq!(ProviderKind::parse(name), None, "{name:?}");
        }
    }

    #[test]
    fn cpu_is_always_a_valid_selection() {
        assert_eq!(ProviderKind::parse("cpu"), Some(ProviderKind::Cpu));
        assert_eq!(select_by_priority(&[]).as_str(), "cpu");
    }

    // -- explanation --------------------------------------------------------

    #[test]
    fn cpu_only_runtime_explains_itself() {
        let detection = ProviderDetection {
            provider: ProviderKind::Cpu,
            available: vec![],
        };
        let msg = detection.explain();
        assert!(msg.contains("no GPU execution providers"), "{msg}");
        assert!(msg.contains("cpu"), "{msg}");
    }

    #[test]
    fn gpu_runtime_names_what_it_found() {
        let detection = ProviderDetection {
            provider: ProviderKind::Cuda,
            available: vec![ProviderKind::Cuda, ProviderKind::Rocm],
        };
        let msg = detection.explain();
        assert!(msg.contains("cuda, rocm"), "{msg}");
        assert!(msg.ends_with("selecting cuda"), "{msg}");
    }

    // -- live detection -----------------------------------------------------

    /// Requires a loadable libonnxruntime.so, which is a packaging artifact
    /// rather than something `cargo test` provides — hence `#[ignore]`, per the
    /// tier-2 hardware-test convention in AGENTS.md. Deliberately asserts only
    /// that the answer is *usable*, never that a specific GPU was found: the
    /// result depends entirely on which ORT build the host has installed.
    #[test]
    #[ignore]
    fn live_detection_returns_a_registerable_provider() {
        let detection = detect_execution_provider().expect("ORT must be loadable");
        // Run with --nocapture to see what this host's ORT actually reports.
        println!("{}", detection.explain());
        assert_eq!(
            ProviderKind::parse(detection.provider.as_str()),
            Some(detection.provider)
        );
        assert!(ProviderKind::ALL.contains(&detection.provider));
        if detection.available.is_empty() {
            assert_eq!(detection.provider, ProviderKind::Cpu);
        } else {
            assert_eq!(detection.provider, detection.available[0]);
        }
    }
}
