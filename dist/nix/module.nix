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

    # Create facelock group
    users.groups.facelock = { };

    # systemd units
    systemd.services.facelock-daemon = {
      description = "Facelock Face Authentication Daemon";
      after = [ "local-fs.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${facelockPackage}/bin/facelock daemon";
        StandardOutput = "journal";
        StandardError = "journal";
        Restart = "on-failure";
        RestartSec = 3;
        LimitNOFILE = 1024;
      };
    };

    # tmpfiles rules
    systemd.tmpfiles.rules = [
      "d /run/facelock 0755 root facelock -"
      "d /var/lib/facelock 0750 root facelock -"
      "d /var/lib/facelock/models 0755 root root -"
      "d /var/log/facelock 0750 root facelock -"
      "d /var/log/facelock/snapshots 0750 root facelock -"
    ];
  };
}
