{
  config,
  pkgs,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  systemd.user.services.gpg-import-keys = lib.mkIf (!hS.isDarwin) {
    Unit = {
      Description = "Import gpg keys";
      After = ["gpg-agent.socket"];
    };
    Service = {
      Type = "oneshot";
      ExecStart = toString (
        pkgs.writeScript "gpg-import-keys" ''
          #! ${pkgs.runtimeShell} -el
          ${pkgs.gnupg}/bin/gpg --import ${hS.home}/.ssh/pgp_github.key ${hS.home}/.ssh/pgp_github.pub
        ''
      );
    };
    Install = {
      WantedBy = ["default.target"];
    };
  };
}
