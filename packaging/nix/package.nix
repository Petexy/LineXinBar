{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  patchelf,
  addDriverRunpath,
  wayland,
  libinput,
  seatd,
  systemd,
  libdrm,
  libgbm,
  libglvnd,
  libxkbcommon,
  xorg,
  vulkan-loader,
  dbus,
  xwayland,
  wireplumber,
  pulseaudio,
  alsa-lib,
  alsa-utils,
  ddcutil,
  pipewire,
  src ? ../..,
  # "all" builds the compositor, the shell and the portal as one package.
  # "compositor" builds the compositor alone, for a consumer that needs a
  # Wayland session on the hardware and nothing that assumes this project's
  # shell — a display manager, most of all.
  #
  # There is deliberately no shell-only component here as there is for the
  # distro packages. Those split to keep a dependency graph and a file list
  # apart on an installed system; Nix has neither problem, and a desktop
  # package that had to find `lxb` in another store path would be strictly
  # worse than one that carries it.
  component ? "all",
}:

assert lib.assertOneOf "component" component [ "all" "compositor" ];

let
  compositorOnly = component == "compositor";
  sourceRoot = toString src;
  cleanSrc = lib.cleanSourceWith {
    inherit src;
    filter = path: type:
      let
        relative = lib.removePrefix "${sourceRoot}/" (toString path);
      in
      !(relative == ".git"
        || lib.hasPrefix ".git/" relative
        || relative == "target"
        || lib.hasPrefix "target/" relative
        || relative == "packaging/out"
        || lib.hasPrefix "packaging/out/" relative
        || relative == "result"
        || lib.hasPrefix "result-" relative);
  };
  runtimeLibraries = [
    alsa-lib
    wayland
    libinput
    seatd
    systemd
    libdrm
    libgbm
    libglvnd
    vulkan-loader
    libxkbcommon
    xorg.libX11
    xorg.libxcb
    xorg.libXcursor
    xorg.libXi
  ];
  runtimePrograms = [
    dbus
    xwayland
    systemd
  ] ++ lib.optionals (!compositorOnly) [
    wireplumber
    pulseaudio
    alsa-utils
    ddcutil
  ];
  binaries = if compositorOnly then [ "lxb" ] else [ "lxb" "lxb-desktop" "lxb-portal" ];
  crates = if compositorOnly then [ "-p" "lxb-compositor" ] else [ "--workspace" ];
in
rustPlatform.buildRustPackage {
  pname = if compositorOnly then "lxb-compositor" else "lxb-desktop";
  version = lib.removeSuffix "\n" (builtins.readFile ../../VERSION);
  src = cleanSrc;

  cargoLock.lockFile = "${cleanSrc}/Cargo.lock";
  cargoBuildFlags = crates ++ [ "--bins" ];
  cargoTestFlags = crates ++ [ "--lib" "--bins" ];
  checkType = "debug";

  strictDeps = true;
  nativeBuildInputs = [
    pkg-config
    makeWrapper
    patchelf
    addDriverRunpath
  ];
  buildInputs = [
    alsa-lib
    wayland
    libinput
    seatd
    systemd
    libdrm
    libgbm
    libglvnd
    libxkbcommon
    xorg.libX11
    xorg.libxcb
    xorg.libXcursor
    xorg.libXi
    vulkan-loader
    pipewire
  ];

  # cargoInstallHook knows buildRustPackage's target-triple output directory
  # and installs the binaries selected by cargoBuildFlags. Everything below is
  # what those binaries need beside them. The cursor theme and the
  # configuration reference go with the compositor, which is what loads the one
  # and reads the other.
  postInstall = ''
    mkdir -p "$out/share/icons/Bibata-Modern-Classic"
    cp -a --no-preserve=ownership share/icons/Bibata-Modern-Classic/. \
      "$out/share/icons/Bibata-Modern-Classic/"

    install -Dm0644 LICENSE \
      "$out/share/licenses/$pname/GPL-3.0-only.txt"
    install -Dm0644 font/Roboto/LICENSE.txt \
      "$out/share/licenses/$pname/Roboto-Apache-2.0.txt"
    install -Dm0644 docs/configuration.md \
      "$out/share/doc/$pname/configuration.md"
    install -Dm0644 examples/config.toml \
      "$out/share/doc/$pname/config.example.toml"
  '' + lib.optionalString (!compositorOnly) ''
    install -Dm0755 packaging/files/lxb-session "$out/bin/lxb-session"
    install -Dm0644 packaging/files/lxb.desktop \
      "$out/share/wayland-sessions/lxb.desktop"

    install -Dm0644 share/xdg-desktop-portal/portals/lxb.portal \
      "$out/share/xdg-desktop-portal/portals/lxb.portal"
    install -Dm0644 share/xdg-desktop-portal/linexinbar-portals.conf \
      "$out/share/xdg-desktop-portal/linexinbar-portals.conf"
    install -Dm0644 \
      share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service \
      "$out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service"

    install -Dm0644 README.md "$out/share/doc/$pname/README.md"

    patchShebangs "$out/bin/lxb-session"
    substituteInPlace "$out/share/wayland-sessions/lxb.desktop" \
      --replace-fail "Exec=lxb-session" "Exec=$out/bin/lxb-session" \
      --replace-fail "TryExec=lxb-session" "TryExec=$out/bin/lxb-session"
  '';

  postFixup = ''
    for program in ${lib.escapeShellArgs binaries}; do
      addDriverRunpath "$out/bin/$program"
      patchelf --add-rpath "${lib.makeLibraryPath runtimeLibraries}" \
        "$out/bin/$program"
      wrapProgram "$out/bin/$program" \
        --prefix PATH : "${lib.makeBinPath runtimePrograms}"
    done
  '' + lib.optionalString (!compositorOnly) ''
    wrapProgram "$out/bin/lxb-session" \
      --prefix PATH : "$out/bin:${lib.makeBinPath runtimePrograms}"
  '';

  passthru.providedSessions = lib.optionals (!compositorOnly) [ "lxb" ];

  meta = {
    description =
      if compositorOnly
      then "Wayland compositor for LineXinBar, usable on its own"
      else "Multi-display Wayland desktop with an XMB-style shell";
    homepage = "https://github.com/Petexy/LineXinBar";
    license = with lib.licenses; [ gpl3Only asl20 mit ];
    mainProgram = "lxb";
    platforms = lib.platforms.linux;
  };
}
