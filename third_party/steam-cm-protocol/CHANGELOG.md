# Changelog


## [v0.4.1] - 2026-06-09

### Changed

- Renamed the crate from `vapour-protocol` to `steam-cm-protocol` to reflect that it speaks the Steam Connection Manager (CM) protocol, and to decouple it from the consuming application's brand. Code is otherwise equivalent to `vapour-protocol` 0.4.0.


## [v0.4.0] - 2026-06-09

### Added

- Release automation for GitHub releases and crates.io publishing.
- CI checks for formatting, clippy, tests, and package validation.
- Crate governance and contribution documents, including code of conduct, contributing guide, security policy, pull request template, and release instructions.
- `Cargo.lock` for reproducible CI and release validation.

### Changed

- Prepared the crate metadata, README, and public exports for the first public crates.io release.
- Documented authentication, friends, chat, library, achievements, and PICS metadata usage in the README.

## [v0.3.0] - 2026-06-08

### Added

- One-on-one friend messaging through the `FriendMessages` service.
- Confirmed send responses that include server timestamp and ordinal metadata.
- Live incoming friend-message handling through chat mode subscription and EMsg push decoding.
- PICS launch metadata, including install directory, configuration, and launch options surfaced from appinfo.

### Fixed

- Friend-message receive flow now enables chat mode `2` and decodes live EMsg `146` pushes.

## [v0.2.5] - 2026-06-07

### Added

- `ClientGetUserStats` EMsgs and userstats protobuf support.
- Authenticated per-game playtime fetching over the CM envelope.
- Native per-user achievement loading through `ClientGetUserStats`.
- Achievement schema parser fixture coverage.

### Fixed

- Corrected service-method EMsg values used by user stats and achievement requests.
- Hardened achievement schema parsing.

## [v0.2.0] - 2026-06-07

### Added

- Friends-over-protocol support, including presence, persona state, and live persona updates.
- Library and achievements access through CM service method calls.
- Asynchronous `GetOwnedGames` request and response handling.
- Keyless CM library loading through PICS appinfo.
- Library filtering by app type.
- Public library exports for `SteamClient`, auth events, friends/events models, protocol games, library entries, achievements, and PICS metadata.

### Fixed

- Corrected persona state flag decoding.
- Added `game_fields_present` tracking to persona data.
- Hardened CM library loading when appinfo data is missing or incomplete.

## [v0.1.0] - 2026-05-28

### Added

- Initial Steam CM protocol implementation.
- Authentication helpers for QR and credential flows.
- CM WebSocket transport, connection lifecycle, heartbeat handling, and server list loading.
- Protobuf build pipeline and generated Steam message foundations.
- Core message envelope, EMsg, EResult, service method, token, and error handling modules.

# What is this?

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
