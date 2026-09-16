{ config, lib, pkgs, ... }:

let
  cfg = config.programs.linexinbar;
in
{
  options.programs.linexinbar = {
    enable = lib.mkEnableOption "the LineXinBar Wayland desktop environment";

    updatesPolicy = lib.mkOption {
      type = lib.types.nullOr (pkgs.formats.json {}).type;
      default = null;
      example = {
        nixos = { kind = "flake"; directory = "/etc/nixos"; configuration = "desktop"; };
      };
      description = ''
        Administrator-selected update sources, written to /etc/linexinbar/updates.json.
        See docs/updates.md. Without this policy, NixOS updates require manual
        configuration. The selected flake and lock file must be writable by root
        and protected against replacement by other users.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { src = ../..; };
      defaultText = lib.literalExpression
        "pkgs.callPackage ./packaging/nix/package.nix { src = ./.; }";
      description = "LineXinBar package to install and register as a session.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.etc = lib.optionalAttrs (cfg.updatesPolicy != null) {
      "linexinbar/updates.json".text = builtins.toJSON cfg.updatesPolicy;
    };
    services.displayManager.sessionPackages = [ cfg.package ];
    # The two device nodes the shell opens itself: the second-generation Steam
    # Controller's hidraw, which no kernel driver claims, and `/dev/uinput`,
    # which is how a guarded pad and a stand-in gamepad are handed back to the
    # rest of the machine. Neither is granted to anybody by default, and
    # without them the controller handling half works and says nothing.
    #
    # Enabling this grants both to the active session's user, which means every
    # process that user runs — `/dev/uinput` is input injection, so this is a
    # wider trust boundary than the rest of the session's. See the Permissions
    # section of the README.
    services.udev.packages = [ cfg.package ];
    services.graphical-desktop.enable = true;
    services.dbus.enable = true;
    services.fwupd.enable = true;
    security.polkit.enable = true;
    hardware.graphics.enable = lib.mkDefault true;
  };
}
