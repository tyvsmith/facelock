{ config, lib, pkgs, ... }:

let
  cfg = config.services.facelock;
  settingsFormat = pkgs.formats.toml { };
  configFile = settingsFormat.generate "config.toml" cfg.config;
  facelockPackage = cfg.package;
in
{
  options.services.facelock = {
    enable = lib.mkEnableOption "Facelock face authentication";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./default.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./default.nix { }";
      description = "The Facelock package to use.";
    };

    config = lib.mkOption {
      type = settingsFormat.type;
      default = { };
      description = ''
        Configuration for Facelock. These options map directly to
        /etc/facelock/config.toml keys. See the default config for
        available options.
      '';
      example = lib.literalExpression ''
        {
          device.path = "/dev/video2";
          recognition.threshold = 0.80;
          recognition.timeout_secs = 5;
          daemon.mode = "daemon";
          security.require_ir = true;
        }
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # Install the package
    environment.systemPackages = [ facelockPackage ];

    # Polkit authority, for the facelock-polkit-agent binary the package ships
    security.polkit.enable = true;

    # PAM module
    security.pam.services = {
      sudo.rules.auth.facelock = {
        order = 100;
        control = "sufficient";
        modulePath = "${facelockPackage}/lib/security/pam_facelock.so";
      };
    };

    # Configuration file
    environment.etc."facelock/config.toml".source = configFile;
    environment.etc."dbus-1/system.d/org.facelock.Daemon.conf".source =
      "${facelockPackage}/share/dbus-1/system.d/org.facelock.Daemon.conf";
    environment.etc."dbus-1/system-services/org.facelock.Daemon.service".text = ''
      [D-BUS Service]
      Name=org.facelock.Daemon
      Exec=${facelockPackage}/bin/facelock daemon
      User=root
      SystemdService=facelock-daemon.service
    '';

    # systemd units
    systemd.services.facelock-daemon = {
      description = "Facelock Face Authentication Daemon";
      after = [ "local-fs.target" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "dbus";
        BusName = "org.facelock.Daemon";
        ExecStart = "${facelockPackage}/bin/facelock daemon";
        StandardOutput = "journal";
        StandardError = "journal";
        Restart = "on-failure";
        RestartSec = 3;
        LimitNOFILE = 1024;
        UMask = "0027";
        ProtectSystem = "strict";
        InaccessiblePaths = [ "/home" "/root" ];
        ReadWritePaths = [ "/var/lib/facelock" "/var/log/facelock" ];
        PrivateTmp = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictNamespaces = true;
        LockPersonality = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
      };
    };

    # tmpfiles rules
    # Must match dist/facelock.tmpfiles.
    systemd.tmpfiles.rules = [
      # Nothing is group-owned any more (ADR 010).
      "d /run/facelock 0755 root root -"
      # 0711 root:root = traversable by everyone, listable by root only
      # (ADR 010). Parent must come before its children.
      "d /var/lib/facelock 0711 root root -"
      # Public, SHA256-verified downloads.
      "d /var/lib/facelock/models 0755 root root -"
      # Markers only: a user can open its own 0600 marker by name but cannot
      # enumerate who else is enrolled.
      "d /var/lib/facelock/enrolled 0711 root root -"
      # PAM rollback state contains complete service files: root-only.
      "d /var/lib/facelock/pam-backups 0700 root root -"
      # Encrypted biometric templates: root-only. `z` never creates.
      "z /var/lib/facelock/facelock.db 0600 root root -"
      "z /var/lib/facelock/facelock.db-wal 0600 root root -"
      "z /var/lib/facelock/facelock.db-shm 0600 root root -"
      # Per-user auth history and raw face snapshots: root-only.
      "d /var/log/facelock 0700 root root -"
      "d /var/log/facelock/snapshots 0700 root root -"
      "z /var/log/facelock/audit.jsonl 0600 root root -"
    ];
  };
}
