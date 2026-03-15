{ lib
, rustPlatform
, pkg-config
, clang
, pam
, v4l-utils
, openssl
, sqlite
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
  ];

  # Tests require camera hardware
  doCheck = false;

  # ONNX runtime needs clang for linking
  LIBCLANG_PATH = "${clang.cc.lib}/lib";

  postInstall = ''
    # PAM module
    mkdir -p $out/lib/security
    cp target/release/libpam_facelock.so $out/lib/security/pam_facelock.so

    # Configuration
    install -Dm644 config/facelock.toml $out/etc/facelock/config.toml

    # systemd units
    install -Dm644 systemd/facelock-daemon.service $out/lib/systemd/system/facelock-daemon.service

    # sysusers.d and tmpfiles.d
    install -Dm644 dist/facelock.sysusers $out/lib/sysusers.d/facelock.conf
    install -Dm644 dist/facelock.tmpfiles $out/lib/tmpfiles.d/facelock.conf
  '';

  meta = with lib; {
    description = "Face authentication for Linux PAM";
    homepage = "https://github.com/tyvsmith/facelock";
    license = with licenses; [ mit asl20 ];
    platforms = [ "x86_64-linux" ];
    mainProgram = "facelock";
  };
}
