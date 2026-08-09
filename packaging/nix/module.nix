{ config, lib, pkgs, ... }:

let
  cfg = config.programs.linexinbar;
in
{
  options.programs.linexinbar = {
    enable = lib.mkEnableOption "the LineXinBar Wayland desktop environment";

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
    services.displayManager.sessionPackages = [ cfg.package ];
    services.graphical-desktop.enable = true;
    services.dbus.enable = true;
    security.polkit.enable = true;
    hardware.graphics.enable = lib.mkDefault true;
  };
}
