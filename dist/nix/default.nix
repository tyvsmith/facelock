{ lib
, rustPlatform
, pkg-config
, clang
, pam
, v4l-utils
, openssl
, sqlite
, onnxruntime
, tpm2-tss
, dbus
}:

rustPlatform.buildRustPackage {
  pname = "facelock";
  version = "0.1.0";

  src = ../../.;

  cargoLock = {
    lockFile = ../../Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
    clang
  ];

  buildInputs = [
    pam
    v4l-utils
    openssl
    sqlite
    onnxruntime
    tpm2-tss
    dbus
  ];

  # Tests require camera hardware
  doCheck = false;

  # ONNX runtime needs clang for linking
  LIBCLANG_PATH = "${clang.cc.lib}/lib";

  postInstall = ''
    # Polkit agent
    if [ -f target/release/facelock-polkit-agent ]; then
      install -Dm755 target/release/facelock-polkit-agent $out/bin/facelock-polkit-agent
    fi

    # PAM module
    mkdir -p $out/lib/security
    cp target/release/libpam_facelock.so $out/lib/security/pam_facelock.so

    # Configuration
    install -Dm644 config/facelock.toml $out/etc/facelock/config.toml

    # Quirks database
    install -dm755 $out/share/facelock/quirks.d
    install -Dm644 -t $out/share/facelock/quirks.d/ config/quirks.d/*.toml

    # systemd units
    install -Dm644 systemd/facelock-daemon.service $out/lib/systemd/system/facelock-daemon.service

    # D-Bus policy and activation service
    install -Dm644 dbus/org.facelock.Daemon.conf $out/share/dbus-1/system.d/org.facelock.Daemon.conf
    install -Dm644 dbus/org.facelock.Daemon.service $out/share/dbus-1/system-services/org.facelock.Daemon.service

    # sysusers.d and tmpfiles.d
    install -Dm644 dist/facelock.sysusers $out/lib/sysusers.d/facelock.conf
    install -Dm644 dist/facelock.tmpfiles $out/lib/tmpfiles.d/facelock.conf

    # Bundled ONNX Runtime for non-NixOS use
    install -dm755 $out/lib/facelock
    ln -s ${onnxruntime}/lib/libonnxruntime.so $out/lib/facelock/libonnxruntime.so
  '';

  meta = with lib; {
    description = "Face authentication for Linux PAM";
    homepage = "https://github.com/tyvsmith/facelock";
    license = with licenses; [ mit asl20 ];
    platforms = [ "x86_64-linux" ];
    mainProgram = "facelock";
  };
}
