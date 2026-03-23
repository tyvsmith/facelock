Name:           facelock
Version:        0.1.0
Release:        1%{?dist}
Summary:        Face authentication for Linux PAM
License:        MIT OR Apache-2.0
URL:            https://github.com/tyvsmith/facelock
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  clang-devel
BuildRequires:  pam-devel
BuildRequires:  libv4l-devel
BuildRequires:  systemd-rpm-macros
BuildRequires:  tpm2-tss-devel

Requires:       pam
Requires:       tpm2-tss
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

# sysusers.d
install -Dm644 dist/facelock.sysusers %{buildroot}%{_sysusersdir}/facelock.conf

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

# Bundled CPU ONNX Runtime (if present — added by release CI)
if [ -f onnxruntime/lib/libonnxruntime.so ]; then
    install -Dm755 onnxruntime/lib/libonnxruntime.so %{buildroot}%{_libdir}/facelock/libonnxruntime.so
fi

# Licenses
install -Dm644 LICENSE-MIT %{buildroot}%{_datadir}/licenses/%{name}/LICENSE-MIT
install -Dm644 LICENSE-APACHE %{buildroot}%{_datadir}/licenses/%{name}/LICENSE-APACHE

%check
# Tests require hardware (camera); skip in package build

%pre
%sysusers_create_compat dist/facelock.sysusers

%post
%tmpfiles_create_compat dist/facelock.tmpfiles
%systemd_post facelock-daemon.service

echo ""
echo "facelock installed. Two steps remaining:"
echo "  1. sudo facelock setup       (download face recognition models)"
echo "  2. sudo facelock enroll      (register your face)"

%preun
%systemd_preun facelock-daemon.service

%postun
%systemd_postun_with_restart facelock-daemon.service

%files
%license LICENSE-MIT LICENSE-APACHE
%doc config/facelock.toml
%{_bindir}/facelock
%{_bindir}/facelock-polkit-agent
%{_libdir}/security/pam_facelock.so
%{_libdir}/facelock/
%config(noreplace) %{_sysconfdir}/facelock/config.toml
%{_datadir}/facelock/quirks.d/
%{_unitdir}/facelock-daemon.service
%{_sysusersdir}/facelock.conf
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
