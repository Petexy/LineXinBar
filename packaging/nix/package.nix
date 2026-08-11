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
  src ? ../..,
}:

let
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
    wireplumber
    pulseaudio
    alsa-utils
    ddcutil
    systemd
  ];
in
rustPlatform.buildRustPackage {
  pname = "linexinbar";
  version = lib.removeSuffix "\n" (builtins.readFile ../VERSION);
  src = cleanSrc;

  cargoLock.lockFile = "${cleanSrc}/Cargo.lock";
  cargoBuildFlags = [ "--workspace" "--bins" ];
  cargoTestFlags = [ "--workspace" "--lib" "--bins" ];
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
  # and installs both workspace binaries selected by cargoBuildFlags.
  postInstall = ''
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

    mkdir -p "$out/share/icons/Bibata-Modern-Classic"
    cp -a --no-preserve=ownership share/icons/Bibata-Modern-Classic/. \
      "$out/share/icons/Bibata-Modern-Classic/"

    install -Dm0644 LICENSE \
      "$out/share/licenses/linexinbar/GPL-3.0-only.txt"
    install -Dm0644 font/Roboto/LICENSE.txt \
      "$out/share/licenses/linexinbar/Roboto-Apache-2.0.txt"
    install -Dm0644 README.md "$out/share/doc/linexinbar/README.md"
    install -Dm0644 docs/configuration.md \
      "$out/share/doc/linexinbar/configuration.md"
    install -Dm0644 examples/config.toml \
      "$out/share/doc/linexinbar/config.example.toml"

    patchShebangs "$out/bin/lxb-session"
    substituteInPlace "$out/share/wayland-sessions/lxb.desktop" \
      --replace-fail "Exec=lxb-session" "Exec=$out/bin/lxb-session" \
      --replace-fail "TryExec=lxb-session" "TryExec=$out/bin/lxb-session"
  '';

  postFixup = ''
    for program in lxb lxb-desktop lxb-portal; do
      addDriverRunpath "$out/bin/$program"
      patchelf --add-rpath "${lib.makeLibraryPath runtimeLibraries}" \
        "$out/bin/$program"
      wrapProgram "$out/bin/$program" \
        --prefix PATH : "${lib.makeBinPath runtimePrograms}"
    done
    wrapProgram "$out/bin/lxb-session" \
      --prefix PATH : "$out/bin:${lib.makeBinPath runtimePrograms}"
  '';

  passthru.providedSessions = [ "lxb" ];

  meta = {
    description = "Multi-display Wayland desktop with an XMB-style shell";
    homepage = "https://github.com/petexy/project-linexinbar";
    license = with lib.licenses; [ gpl3Only asl20 ];
    mainProgram = "lxb";
    platforms = lib.platforms.linux;
  };
}
