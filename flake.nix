{
  description = "LineXinBar Wayland desktop environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          linexinbar = pkgs.callPackage ./packaging/nix/package.nix { src = ./.; };
          # The compositor on its own, for anything that needs a Wayland
          # session on the hardware but not this project's shell — a display
          # manager, most of all. See `component` in package.nix.
          lxb-compositor = pkgs.callPackage ./packaging/nix/package.nix {
            src = ./.;
            component = "compositor";
          };
        in
        {
          inherit linexinbar lxb-compositor;
          default = linexinbar;
        });

      nixosModules = {
        linexinbar = import ./packaging/nix/module.nix;
        default = import ./packaging/nix/module.nix;
      };
    };
}
