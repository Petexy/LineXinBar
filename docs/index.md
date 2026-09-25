# LineXinBar documentation

[Back to the project](../README.md)

Start with [Getting started](getting-started.md) to build and run LineXinBar.
These guides hold the detailed user and developer reference previously included
in the project README.

## Install and configure

| Guide | Contents |
| --- | --- |
| [Getting started](getting-started.md) | Requirements, permissions, build dependencies, nested and native sessions |
| [Packaging](packaging.md) | Arch, Debian/Ubuntu, Fedora and Nix/NixOS packages |
| [Configuration](configuration.md) | Full configuration reference and per-application settings |
| [Project overview](overview.md) | Design goals, Gamescope comparison, known limitations and license |

## Use the desktop

| Guide | Contents |
| --- | --- |
| [Applications, media and files](shell.md) | Launcher categories, music, videos, images, search and file operations |
| [Steam](steam.md) | Sign-in, library, installation, launching games, friends and invitations |
| [RetroArch](retroarch.md) | Optional integration, ROM folders, cores and console games |
| [Epic Games](epic.md) | Optional integration through Heroic: sign-in, library, installs, updates, cloud saves and achievements |
| [Desktop settings](settings.md) | Appearance, displays, HDR, night light, sound, networking and system settings |
| [Controls and multiple displays](controls.md) | Gamepads, keyboard shortcuts, mouse, touch and on-screen keyboard |
| [Guide overlay and context menus](guide.md) | App switching, quick settings, volume mixer and stick pointer |
| [Desktop integration](desktop-integration.md) | Screenshots, screen sharing, file dialogs and authorisation prompts |
| [Updates](updates.md) | Update sources, recovery, administrator configuration and validation limits |
| [Screenshots](screenshots/gallery.md) | Full-size views of the desktop, Settings and guide |

## Develop and maintain

- [Architecture and rendering](architecture.md): source layout, shell protocol,
  audio, glass effects and rendering behavior.
- [Localization](localization.md): language selection, translations and formatting.
- [Update providers](updates.md#custom-system-update-providers): integrating a
  distribution’s own updater.
- [Packaging and validation](packaging.md#validate-the-shared-payload):
  release checks and hardware test reporting.

Keep detailed behavior and implementation notes in the relevant guide. The root
README is the project introduction and quick start.
