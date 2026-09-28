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
  ffmpeg,
  fwupd,
  polkit,
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
    fwupd
    polkit
  ];
  # `lxb-retroarch` and `lxb-heroic` are deliberately not in this list: they
  # link nothing that
  # needs a driver runpath or a wrapper, and they look for `flatpak` on the PATH
  # of the session that started them rather than on one baked in here — a
  # helper wrapped with this package's own PATH would be one that could not see
  # the flatpak the user installed.
  binaries = if compositorOnly then [ "lxb" ] else [ "lxb" "lxb-desktop" "lxb-portal" "lxb-updates" ];
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

  # The tests that bring up a wayland-server Display load libwayland at
  # runtime rather than by link: smithay's winit backend asks winit for
  # `wayland-dlopen`, and feature unification makes every user of wayland-sys
  # in the graph dlopen too. A normal machine has libwayland in its system
  # library path; the build sandbox has none, so hand the test binaries one.
  # The portal's caller test also starts a private `dbus-daemon`, which the
  # sandbox has nowhere on its PATH without this.
  preCheck = ''
    export LD_LIBRARY_PATH="${lib.makeLibraryPath [ wayland ]}"
    export PATH="$PATH:${lib.makeBinPath [ dbus ]}"
  '';

  strictDeps = true;
  nativeBuildInputs = [
    pkg-config
    makeWrapper
    patchelf
    addDriverRunpath
    # FFmpeg's Rust bindings are generated at build time, and bindgen needs to
    # be told where libclang and the C headers are. The hook is what does that.
    rustPlatform.bindgenHook
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
    # A wallpaper of the user's own: their picture decoded, or their film
    # played, under Settings > Appearance > Theme > Wallpaper.
    ffmpeg
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
    install -Dm0644 font/RobotoMono/OFL.txt \
      "$out/share/licenses/$pname/RobotoMono-OFL-1.1.txt"
    install -Dm0644 font/NotoSansDevanagariUI/OFL.txt \
      "$out/share/licenses/$pname/NotoSansDevanagariUI-OFL-1.1.txt"
    install -Dm0644 font/NotoSansCJKsc/OFL.txt \
      "$out/share/licenses/$pname/NotoSansCJKsc-OFL-1.1.txt"
    install -Dm0644 docs/configuration.md \
      "$out/share/doc/$pname/configuration.md"
    install -Dm0644 examples/config.toml \
      "$out/share/doc/$pname/config.example.toml"
  '' + lib.optionalString (!compositorOnly) ''
    install -Dm0755 packaging/files/lxb-session "$out/bin/lxb-session"
    install -Dm0644 packaging/files/lxb.desktop \
      "$out/share/wayland-sessions/lxb.desktop"

    # The two device nodes the shell opens itself. NixOS reads rules out of
    # `services.udev.packages`, which is what `lib/udev/rules.d` in an output
    # is for; the module beside this file names this package there.
    install -Dm0644 packaging/files/70-linexinbar-input.rules \
      "$out/lib/udev/rules.d/70-linexinbar-input.rules"

    install -Dm0644 share/xdg-desktop-portal/portals/lxb.portal \
      "$out/share/xdg-desktop-portal/portals/lxb.portal"
    install -Dm0644 share/xdg-desktop-portal/linexinbar-portals.conf \
      "$out/share/xdg-desktop-portal/linexinbar-portals.conf"
    install -Dm0644 \
      share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service \
      "$out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service"

    # Being the machine's file manager. The shell takes
    # `org.freedesktop.FileManager1` while it runs, which is what a browser's
    # Show in folder calls; these two cover `xdg-open` on a folder and every
    # application that falls back to launching whatever opens one. The list is
    # read only while XDG_CURRENT_DESKTOP lowercases to `linexinbar`, so it
    # takes nothing away from any other desktop on the machine.
    install -Dm0644 share/applications/linexinbar-files.desktop \
      "$out/share/applications/linexinbar-files.desktop"
    install -Dm0644 share/applications/linexinbar-mimeapps.list \
      "$out/share/applications/linexinbar-mimeapps.list"

    # The entry standing for the shell's own Extract, which is what opens an
    # archive in this session. Not a program — the shell answers the press
    # itself — but the machine has to have something to name as the handler for
    # the choice to be recordable, and this is also what `xdg-open` on a `.zip`
    # reaches. See crates/lxb-desktop/src/archive.rs.
    install -Dm0644 share/applications/linexinbar-extract.desktop \
      "$out/share/applications/linexinbar-extract.desktop"

    install -Dm0644 README.md "$out/share/doc/$pname/README.md"
    install -Dm0644 packaging/files/org.linexinbar.updates.policy.in \
      "$out/share/polkit-1/actions/org.linexinbar.updates.policy"
    substituteInPlace "$out/share/polkit-1/actions/org.linexinbar.updates.policy" \
      --replace-fail '@HELPER@' "$out/bin/lxb-updates"
    install -Dm0644 packaging/files/org.linexinbar.locale.policy.in \
      "$out/share/polkit-1/actions/org.linexinbar.locale.policy"
    substituteInPlace "$out/share/polkit-1/actions/org.linexinbar.locale.policy" \
      --replace-fail '@HELPER@' "$out/bin/lxb-desktop"
    install -Dm0644 packaging/files/org.linexinbar.drives.policy.in \
      "$out/share/polkit-1/actions/org.linexinbar.drives.policy"
    substituteInPlace "$out/share/polkit-1/actions/org.linexinbar.drives.policy" \
      --replace-fail '@HELPER@' "$out/bin/lxb-desktop"
    install -Dm0644 docs/updates.md "$out/share/doc/$pname/updates.md"

    # The RetroArch integration's marks. Its binary is installed by
    # cargoInstallHook with the rest of the workspace's; these are what the
    # shell reads out of the data directory to draw that column's rows, and
    # without them every one of them falls back to the shell's own pad.
    #
    # Nix carries the integration inside this one derivation rather than in a
    # package of its own, which is the same choice the compositor's own note
    # explains: the distro packages split to keep a dependency graph and a file
    # list apart on an installed system, and Nix has neither problem. The shell
    # finds the helper the way it finds an installed one, on PATH.
    #
    # The whole directory rather than named files: there is a mark per console
    # and consoles.rs gains machines. Naming them here left this derivation
    # installing two of forty-six, one of which had already left the tree.
    install -Dm0644 crates/lxb-retroarch/glyphs/*.svg \
      -t "$out/share/lxb/glyphs"
    # And the Epic Games integration's one mark, on the same terms.
    install -Dm0644 crates/lxb-heroic/glyphs/epic.svg \
      -t "$out/share/lxb/glyphs"

    patchShebangs "$out/bin/lxb-session"
    # TryExec first: `Exec=lxb-session` is a substring of `TryExec=lxb-session`,
    # so replacing Exec first would rewrite the TryExec line too and the second
    # --replace-fail would then find nothing left to match.
    substituteInPlace "$out/share/wayland-sessions/lxb.desktop" \
      --replace-fail "TryExec=lxb-session" "TryExec=$out/bin/lxb-session" \
      --replace-fail "Exec=lxb-session" "Exec=$out/bin/lxb-session"
    # And the same for the folder handler, which is started by whatever opens a
    # folder rather than by this package: a bare name would only be found if
    # the shell happened to be on that program's PATH.
    substituteInPlace "$out/share/applications/linexinbar-files.desktop" \
      --replace-fail "Exec=lxb-desktop " "Exec=$out/bin/lxb-desktop " \
      --replace-fail "TryExec=lxb-desktop" "TryExec=$out/bin/lxb-desktop"
    # And for the archive handler, which is started the same way and by the
    # same kind of caller.
    substituteInPlace "$out/share/applications/linexinbar-extract.desktop" \
      --replace-fail "Exec=lxb-desktop " "Exec=$out/bin/lxb-desktop " \
      --replace-fail "TryExec=lxb-desktop" "TryExec=$out/bin/lxb-desktop"
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
      else "Multi-display Wayland desktop with a console-style shell";
    homepage = "https://github.com/Petexy/LineXinBar";
    license = with lib.licenses; [ gpl3Only asl20 ofl mit ];
    mainProgram = "lxb";
    platforms = lib.platforms.linux;
  };
}
