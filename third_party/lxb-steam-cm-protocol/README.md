# steam-cm-protocol

Rust implementation of the Steam client protocol for building native Steam applications.

`steam-cm-protocol` speaks directly to Steam CM servers over WebSocket and exposes higher-level
Rust APIs for client-style Steam features — authentication, friends, chat, and your game library —
without depending on the Steam client or Web API key. It was built for, and is used by,
[Steamie](https://github.com/LargeModGames/steamie), but it is a standalone crate you can use on its own.

## Features

- Steam authentication: QR, username/password (with Steam Guard), and refresh-token reuse.
- CM server discovery and WebSocket transport.
- Friends/persona state and real-time friend presence events.
- 1-on-1 friend chat: send, receive, typing indicators, and history.
- Owned/recently-played library loading through protocol messages.
- Per-user achievements and PICS app metadata (type, install dir, launch options).
- Service method calls for authenticated unified Steam services.

## Status

This crate is usable and evolving toward a stable public API. It is `0.x`: expect breaking changes
between minor releases while protocol coverage grows. Talking to Steam requires a real Steam account,
and you are responsible for using it in line with the Steam Subscriber Agreement.

## Requirements

- Rust 1.85+ (the crate uses edition 2024).
- A [Tokio](https://tokio.rs) runtime — the API is async.
- A real Steam account to authenticate against.

## Install

```bash
cargo add steam-cm-protocol
```

`steam-cm-protocol` pulls in `tokio`, so add it too if you don't already depend on it:

```bash
cargo add tokio --features full
```

## Quickstart

Log in with a QR code, then load your owned games and echo any incoming chat message. Every type
used here is re-exported from the crate root.

```rust,no_run
use tokio::sync::mpsc;
use steam_cm_protocol::{AuthEvent, AuthMethod, Error, FriendsEvent, RunCommand, SteamClient};

#[tokio::main]
async fn main() -> steam_cm_protocol::Result<()> {
    let mut client = SteamClient::new();

    // 1. Authenticate. QR is shown here; see "Authentication" for credentials / refresh tokens.
    let mut auth = client.begin_auth(AuthMethod::Qr).await?;
    let session = loop {
        match auth.recv().await {
            Some(AuthEvent::QrChallenge(url)) => {
                // Render `url` as a QR code and scan it in the Steam Mobile app.
                println!("Scan to sign in: {url}");
            }
            Some(AuthEvent::GuardRequired(kind)) => println!("Steam Guard required: {kind:?}"),
            Some(AuthEvent::Success(session)) => break session,
            Some(AuthEvent::Failure(error)) => return Err(error),
            None => return Err(Error::Closed),
        }
    };
    println!("Logged in as {} ({})", session.account_name, session.steamid);
    // Persist `session.refresh_token` to log in again later without re-scanning
    // (see AuthMethod::RefreshToken below).

    // 2. Drive the session over two channels: you send `RunCommand`s in and
    //    receive `FriendsEvent`s back.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    // Handle events — and react with new commands — on a background task.
    let commands = cmd_tx.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                FriendsEvent::OwnedGames(games) => {
                    for game in games {
                        println!("{} — {} min played", game.name, game.playtime_forever);
                    }
                }
                FriendsEvent::IncomingMessage(msg) => {
                    println!("{}: {}", msg.steamid, msg.message);
                    let _ = commands.send(RunCommand::SendMessage {
                        steamid: msg.steamid,
                        message: "got it".to_owned(),
                    });
                }
                _ => {}
            }
        }
    });

    // Ask for the owned library; results arrive as the events handled above.
    let _ = cmd_tx.send(RunCommand::GetLibrary);

    // `run` owns the connection loop and returns when Steam disconnects.
    client.run(cmd_rx, event_tx).await
}
```

## Session model

A `SteamClient` has three stages:

1. **`SteamClient::new()`** — create the client.
2. **`begin_auth(method)`** — connect and start an auth flow. Returns a receiver of `AuthEvent`s;
   wait for `AuthEvent::Success(LoggedOn)`.
3. **`run(commands, events)`** — take over the connection. You drive the session by sending
   `RunCommand`s on the `commands` channel and reacting to `FriendsEvent`s on the `events` channel.
   `run` sets your status online, requests friend data as it arrives, and resolves until the
   connection closes.

### Commands you send (`RunCommand`)

| Command | Effect | Result event |
| --- | --- | --- |
| `SetPersonaState(PersonaState)` | Set your online status | — |
| `RequestFriendData(Vec<u64>)` | Request persona data for SteamIDs | `PersonaStates` |
| `GetLibrary` | Load owned + recently-played games | `RecentlyPlayedGames`, then `OwnedGames` |
| `GetPlayerAchievements(u32)` | Achievements for an appid | `PlayerAchievements` |
| `SendMessage { steamid, message }` | Send a 1-on-1 chat message | `MessageSent` |
| `SendTyping { steamid }` | Send a typing indicator (best-effort) | — |
| `GetRecentMessages { steamid }` | Fetch recent chat history | `RecentMessages` |

### Events you receive (`FriendsEvent`)

| Event | Meaning |
| --- | --- |
| `FriendsList(Vec<Friend>)` | Your friends list (pushed after login) |
| `PersonaStates(Vec<Persona>)` | Presence/profile updates |
| `RecentlyPlayedGames(Vec<ProtocolGame>)` | Recently-played subset of the library |
| `OwnedGames(Vec<ProtocolGame>)` | Full owned-game catalog |
| `PlayerAchievements { appid, achievements }` | Achievements for one app |
| `IncomingMessage(ChatMessage)` | A 1-on-1 message arrived |
| `MessageSent(ChatMessage)` | Your send confirmed (with server timestamp) |
| `TypingNotification { steamid }` | A friend is typing |
| `RecentMessages { steamid, messages }` | History for one conversation |

## Authentication

`begin_auth` takes one of three `AuthMethod`s and reports progress as `AuthEvent`s
(`QrChallenge`, `GuardRequired`, `Success`, `Failure`).

- **`AuthMethod::Qr`** — emits `QrChallenge(url)`; render it as a QR code and scan it in the Steam
  Mobile app.
- **`AuthMethod::Credentials { account, password }`** — may emit `GuardRequired(kind)`. For
  `EmailCode` / `DeviceCode`, collect the code and call `submit_guard_code`; for
  `DeviceConfirmation`, approve the prompt in the Steam Mobile app (no code needed):

  ```rust,ignore
  let mut auth = client
      .begin_auth(AuthMethod::Credentials {
          account: "username".to_owned(),
          password: "password".to_owned(),
      })
      .await?;

  while let Some(event) = auth.recv().await {
      match event {
          AuthEvent::GuardRequired(GuardKind::EmailCode | GuardKind::DeviceCode) => {
              client.submit_guard_code("ABCDE")?; // a code you collected from the user
          }
          AuthEvent::Success(session) => { /* logged in; break and call run() */ }
          AuthEvent::Failure(error) => return Err(error),
          _ => {}
      }
  }
  ```

- **`AuthMethod::RefreshToken(token)`** — reuse the `refresh_token` from a previous
  `LoggedOn` to sign in non-interactively. Pair it with `set_account_name_hint` if you have the
  account name. Store refresh tokens securely; they grant access to the account.

## Development

Run the same checks as CI before opening a PR:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo package --locked
```

If you are developing `steam-cm-protocol` alongside Steamie, point Steamie at your local checkout with a
path dependency so changes to both land together:

```toml
steam-cm-protocol = { path = "../steam-cm-protocol" }
```

Release steps are documented in
[how_to_release.md](https://github.com/LargeModGames/steam-cm-protocol/blob/main/how_to_release.md).

## License

This repository is licensed under `MIT`.
