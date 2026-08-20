Name:           facelock
Version:        0.1.4
Release:        1%{?dist}
Summary:        Face authentication for Linux PAM
License:        MIT OR Apache-2.0
URL:            https://github.com/tyvsmith/facelock
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  clang-devel
BuildRequires:  pam-devel
BuildRequires:  libv4l-devel
BuildRequires:  wayland-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  systemd-rpm-macros
BuildRequires:  tpm2-tss-devel

Requires:       pam
Requires:       tpm2-tss
Requires:       onnxruntime
Recommends:     authselect

%description
Facelock provides Windows Hello-style face authentication for Linux
using IR anti-spoofing, ONNX inference, and PAM integration.

Features include persistent daemon with D-Bus activation for fast
authentication, oneshot mode for systems without systemd,
IR camera requirement to prevent photo spoofing, frame variance
checks, and rate limiting.

After installation, run 'sudo facelock setup' to download face
recognition models, then 'sudo facelock enroll' to register your face.

%prep
%autosetup

%build
cargo build --release --workspace
cargo build --release -p facelock-cli --features tpm

%install
# Binary
install -Dm755 target/release/facelock %{buildroot}%{_bindir}/facelock

# Polkit agent (optional — only if built)
if [ -f target/release/facelock-polkit-agent ]; then
    install -Dm755 target/release/facelock-polkit-agent %{buildroot}%{_bindir}/facelock-polkit-agent
fi

# PAM module
install -Dm755 target/release/libpam_facelock.so %{buildroot}/%{_libdir}/security/pam_facelock.so

# Configuration
install -Dm644 config/facelock.toml %{buildroot}%{_sysconfdir}/facelock/config.toml

# Quirks database
install -dm755 %{buildroot}%{_datadir}/facelock/quirks.d
install -Dm644 -t %{buildroot}%{_datadir}/facelock/quirks.d/ config/quirks.d/*.toml

# systemd units
install -Dm644 systemd/facelock-daemon.service %{buildroot}%{_unitdir}/facelock-daemon.service

# tmpfiles.d
install -Dm644 dist/facelock.tmpfiles %{buildroot}%{_tmpfilesdir}/facelock.conf

# D-Bus policy and activation
install -Dm644 dbus/org.facelock.Daemon.conf %{buildroot}%{_datadir}/dbus-1/system.d/org.facelock.Daemon.conf
install -Dm644 dbus/org.facelock.Daemon.service %{buildroot}%{_datadir}/dbus-1/system-services/org.facelock.Daemon.service

# authselect profile
install -dm755 %{buildroot}%{_datadir}/authselect/vendor/facelock
install -Dm644 dist/authselect/facelock/system-auth %{buildroot}%{_datadir}/authselect/vendor/facelock/system-auth
install -Dm644 dist/authselect/facelock/password-auth %{buildroot}%{_datadir}/authselect/vendor/facelock/password-auth
install -Dm644 dist/authselect/facelock/postlogin %{buildroot}%{_datadir}/authselect/vendor/facelock/postlogin
install -Dm644 dist/authselect/facelock/README %{buildroot}%{_datadir}/authselect/vendor/facelock/README

# Omarchy helper scripts (inert if walker/omarchy isn't installed)
install -Dm755 dist/omarchy/omarchy-setup-security-face %{buildroot}%{_bindir}/omarchy-setup-security-face
install -Dm755 dist/omarchy/omarchy-remove-security-face %{buildroot}%{_bindir}/omarchy-remove-security-face

# Bundled CPU ONNX Runtime (if present — added by release CI).
# Always create %{_libdir}/facelock/ so the %files entry resolves even when ORT
# is not bundled (the Packit/COPR from-source build, which depends on system
# onnxruntime instead). The CI RPM additionally installs the bundled .so here.
install -dm755 %{buildroot}%{_libdir}/facelock
if [ -f onnxruntime/lib/libonnxruntime.so ]; then
    install -Dm755 onnxruntime/lib/libonnxruntime.so %{buildroot}%{_libdir}/facelock/libonnxruntime.so
fi

# Licenses
install -Dm644 LICENSE-MIT %{buildroot}%{_datadir}/licenses/%{name}/LICENSE-MIT
install -Dm644 LICENSE-APACHE %{buildroot}%{_datadir}/licenses/%{name}/LICENSE-APACHE

%check
# Tests require hardware (camera); skip in package build

%post
%tmpfiles_create %{_tmpfilesdir}/facelock.conf
# ADR 010 retired the facelock group: nothing is group-owned any more, so
# remove a group an older install created. Best-effort.
if getent group facelock >/dev/null 2>&1; then
    groupdel facelock 2>/dev/null || true
fi
# A legacy copy in /etc/dbus-1/system.d is read after /usr/share and would
# re-deny Authenticate (last matching rule wins); refresh it if present.
if [ -f /etc/dbus-1/system.d/org.facelock.Daemon.conf ] && \
   grep -q 'org.facelock.Daemon' /etc/dbus-1/system.d/org.facelock.Daemon.conf; then
    install -Dm644 %{_datadir}/dbus-1/system.d/org.facelock.Daemon.conf \
        /etc/dbus-1/system.d/org.facelock.Daemon.conf 2>/dev/null || true
fi
# Bus policy may have changed (ADR 010); ask the bus to re-read it.
dbus-send --system --type=method_call --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ReloadConfig 2>/dev/null || true
%systemd_post facelock-daemon.service

echo ""
echo "facelock installed. Two steps remaining:"
echo "  1. sudo facelock setup       (download face recognition models)"
echo "  2. sudo facelock enroll      (register your face)"

%preun
%systemd_preun facelock-daemon.service

# Only on full uninstall ($1 == 0), not upgrade.
# Remove PAM lines that `facelock setup` may have added — otherwise stale
# references to pam_facelock.so survive removal. facelock writes
# `auth sufficient pam_facelock.so`, so with the module gone PAM logs a dlopen
# failure, treats the module as failed and — being `sufficient` — falls through
# to pam_unix: the cost is log noise and an auth stack that reads wrong, not a
# lockout. At `required` (a hand-edit facelock never makes) the same stale line
# would lock the service out, which is why the cleanup still matters.
if [ $1 -eq 0 ]; then
    # Every PAM service `facelock setup` can write to: the services offered by
    # the setup multi-select (PAM_CANDIDATES in
    # crates/facelock-cli/src/commands/setup.rs) plus the ones it gates behind
    # a confirmation (SENSITIVE_SERVICES). `--service` accepts an arbitrary
    # name, so this list can never be exhaustive; it covers everything facelock
    # itself offers or gates. Missing files are skipped, so naming a service
    # this host does not have is inert. A drift test in setup.rs
    # (`packaging_uninstall_covers_every_pam_candidate`) fails if a new
    # candidate is added without being listed here.
    FACELOCK_PAM_SERVICES="sudo polkit-1 hyprlock swaylock kscreenlocker_greet gdm-password sddm lightdm omarchy-lock-face system-auth login sshd common-auth password-auth system-login system-auth-ac password-auth-ac"

    for service in $FACELOCK_PAM_SERVICES; do
        PAM_FILE="/etc/pam.d/$service"
        if [ -f "$PAM_FILE" ] && grep -q 'pam_facelock\.so' "$PAM_FILE"; then
            sed -i '/pam_facelock\.so/d' "$PAM_FILE"
        fi
    done
    # Remove PAM safety backups created by `facelock setup`
    for service in $FACELOCK_PAM_SERVICES; do
        backup="/etc/pam.d/$service.facelock-backup"
        [ -f "$backup" ] && rm -f "$backup" && echo "Removed $backup"
    done
    # Kill facelock polkit agent if running (lets the DE's agent take over)
    pkill -f facelock-polkit-agent 2>/dev/null || true
fi

%postun
%systemd_postun_with_restart facelock-daemon.service
if [ $1 -eq 0 ]; then
    echo ""
    echo "facelock uninstalled. User data preserved at:"
    echo "  /etc/facelock/      (config.toml, encryption.key.sealed, setup markers)"
    echo "  /var/lib/facelock/  (face database, ONNX models)"
    echo "  /var/log/facelock/  (audit logs and snapshots)"
    echo ""
    echo "Retained state cleanup is intentionally not automated."
    echo "Cleanup must stay within the fixed roots above, leave configured external paths untouched, and refuse links or mount crossings."
    echo "Filesystem deletion does not securely erase SSDs, snapshots, or backups."
fi

%files
%license LICENSE-MIT LICENSE-APACHE
%doc config/facelock.toml
%{_bindir}/facelock
%{_bindir}/facelock-polkit-agent
%{_bindir}/omarchy-setup-security-face
%{_bindir}/omarchy-remove-security-face
%{_libdir}/security/pam_facelock.so
%{_libdir}/facelock/
%config(noreplace) %{_sysconfdir}/facelock/config.toml
%{_datadir}/facelock/quirks.d/
%{_unitdir}/facelock-daemon.service
%{_tmpfilesdir}/facelock.conf
%{_datadir}/dbus-1/system.d/org.facelock.Daemon.conf
%{_datadir}/dbus-1/system-services/org.facelock.Daemon.service
%{_datadir}/authselect/vendor/facelock/

%changelog
* Mon Mar 10 2026 Facelock Contributors <facelock@example.com> - 0.1.0-1
- Initial package
- Unified binary for CLI, daemon, and oneshot auth
- PAM module with daemon and oneshot modes
- IR camera anti-spoofing with frame variance checks
- ONNX inference with SCRFD + ArcFace models
- D-Bus activation support
