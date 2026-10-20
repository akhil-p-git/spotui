# spotui

A terminal Spotify client in Rust: search, play, like songs, and edit playlists from the keyboard.

```text
┌ Search (/) ──────────────────────────────────────────────────────────────────┐
│daft punk                                                                     │
└──────────────────────────────────────────────────────────────────────────────┘
┌ Results ─────────────────────────────────────────────────────────────────────┐
│    End of Line  Daft Punk  TRON: Legacy - The Complete Edition (Original Moti│
│>   One More Time  Daft Punk  Discovery  5:20                                 │
│    Around the World  Daft Punk  Homework  7:09                               │
│  ♥ Instant Crush (feat. Julian Casablancas)  Daft Punk, Julian Casablancas  R│
│  ♥ Get Lucky (feat. Pharrell Williams and Nile Rodgers)  Daft Punk, Pharrell │
│    Harder, Better, Faster, Stronger  Daft Punk  Discovery  3:46              │
└──────────────────────────────────────────────────────────────────────────────┘
┌ Now playing ─────────────────────────────────────────────────────────────────┐
│Nothing playing. Press d to pick a device.                                    │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
10 results
```

spotui is a remote control. It sends commands to Spotify's Web API, and a separate Spotify Connect device on your account plays the audio. This README sets up [spotifyd](https://github.com/Spotifyd/spotifyd) as that device, so no Spotify desktop app has to stay open.

## What you need

- Linux. It was built and tested on Arch Linux only.
- Spotify Premium. [Spotify's play endpoint](https://developer.spotify.com/documentation/web-api/reference/start-a-users-playback) works only for Premium accounts, and since February 2026 the owner of a Development Mode app [needs Premium too](https://developer.spotify.com/documentation/web-api/tutorials/february-2026-migration-guide). Such an app is also limited to 5 users. If you create the app and play music on the same account, one Premium subscription covers both.
- Git, and Rust installed with [rustup](https://rustup.rs). It was built and tested with Rust 1.96.0; older versions are untested.
- `spotifyd` for playback, or any other Spotify Connect device.

## Install

```bash
git clone https://github.com/akhil-p-git/spotui.git
cd spotui
cargo build --release
ln -s "$PWD/target/release/spotui" ~/.local/bin/spotui   # optional; ~/.local/bin must be on your PATH
```

The binary is `target/release/spotui`. With the symlink, a later `cargo build --release` updates the `spotui` command too.

## Set up

### 1. Create a Spotify app

1. Open the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard), log in with your Spotify account, and create an app. Fill in the name and description it asks for.
2. When it asks which APIs you plan to use, choose **Web API**.
3. Add the redirect URI `http://127.0.0.1:8888/callback`. Use `127.0.0.1` and not `localhost`: Spotify [rejects `localhost` and allows plain HTTP only for loopback IP addresses](https://developer.spotify.com/documentation/web-api/concepts/redirect_uri).
4. Save the app, then copy its Client ID. spotui uses PKCE, so it never needs the client secret. The Client ID only identifies your app and is not a secret; the login tokens in `tokens.json` (step 3) are.

### 2. Write the config file

Create the folder and the file `~/.config/spotui/config.json` (or `$XDG_CONFIG_HOME/spotui/config.json`):

```bash
mkdir -p ~/.config/spotui
```

```json
{
  "client_id": "your_client_id",
  "port": 8888
}
```

`port` is optional and defaults to 8888. It must match the port in the redirect URI.

### 3. Log in

```bash
spotui login
```

spotui prints a URL and tries to open it with `xdg-open`. Approve the app in the browser, and the terminal prints `Logged in. Tokens saved.` The tokens are stored in `tokens.json` next to the config file, readable only by you (mode `0600`).

The login asks for permission to control playback, read and edit your library, and read and edit your playlists. If a new version needs more permissions, spotui says `Run spotui login again to grant the new permissions.`

### 4. Set up a player

Install `spotifyd`. On Arch Linux that is `sudo pacman -S spotifyd`; for other systems see the [spotifyd repository](https://github.com/Spotifyd/spotifyd). Then create the folder and the file `~/.config/spotifyd/spotifyd.conf`:

```bash
mkdir -p ~/.config/spotifyd
```

```ini
[global]
backend = "pulseaudio"
device_name = "spotui"
device_type = "computer"
bitrate = 320
```

`backend = "pulseaudio"` also works on PipeWire through its PulseAudio compatibility. Other backends are listed by `spotifyd --help`. `device_name` is the name the device shows in Spotify and in spotui.

`spotifyd` needs its own Spotify login, separate from spotui's. Run `spotifyd authenticate`, which starts a browser login whose redirect uses port 8000, and approve it. It saves the login under `~/.cache/spotifyd/oauth/`. Then start spotifyd as a user service and check it:

```bash
spotifyd authenticate
systemctl --user enable --now spotifyd
systemctl --user is-active spotifyd       # prints "active"
spotui devices                            # should list "spotui"
```

Finally play a first song:

1. Run `spotui`, press `d`, and select the `spotui` device. This points playback at spotifyd but does not start any audio.
2. Press `/`, type a song name, and press `Enter`.
3. Press `Enter` again on a result. The sound comes out of your computer's default audio output.

## Use

`spotui` with no arguments opens the terminal UI. It starts on a Library menu with Search, Liked Songs and Playlists. **Right** goes into an entry and **Left** goes back.

| Key | Action |
|---|---|
| `Up` `Down` or `k` `j` | Move the selection. Long lists load the next page as you near the end. |
| `Right` or `Enter` | Open the highlighted menu entry or playlist. |
| `Left`, `Backspace` or `Esc` | Go back. `Esc` on the Library menu quits. |
| `/` or `s` | Search. Type a query and press `Enter`; `Esc` cancels. |
| `Enter` on a track | Play it. In Liked Songs this plays the next 50 loaded tracks. In a playlist it plays the playlist from that track. |
| `P` | Play the highlighted or open playlist from its start. |
| `a` | Add the track to the playback queue. |
| `l` | Like or unlike the track. A red ♥ marks liked tracks. |
| `A` | Add the track to a playlist you own or collaborate on, or to a new private playlist. |
| `x` | In a playlist, remove the track. Press `x` again to confirm; any other key cancels. |
| `Space` | Pause or resume. |
| `n` `p` | Next or previous track. |
| `+` `-` | Volume up or down by 5. |
| `d` | Choose the playback device. |
| `q` or `Ctrl-C` | Quit. |

The same player controls work without the UI:

```bash
spotui devices                 # list devices; * marks the active one
spotui transfer spotui         # move playback to a device by name, without starting it
spotui search daft punk        # up to 10 tracks, each with its spotify:track: URI
spotui play spotify:track:0DiWol3AO6WpXZgp0goxAV
spotui now                     # what is playing
spotui pause                   # also: spotui next, spotui prev
spotui volume 40               # 0 to 100
spotui queue <uri>             # add a track or episode to the queue
spotui whoami                  # show the logged-in account
```

`spotui play` with no argument resumes playback.

### Limits

- Search returns at most 10 tracks. Spotify caps `limit` at 10 for Development Mode apps.
- Spotify lists the tracks of a playlist only if you own it or collaborate on it. For any other playlist spotui shows an explanation instead. Pressing `P` on one asks Spotify to play it; whether Spotify allows that for playlists you only follow has not been tested.
- The track count in the playlist list can lag behind a change you just made. Opening the playlist shows the real contents.
- spotui cannot rename, reorder or delete playlists.

### Audio quality

Without a `bitrate` line, spotifyd streams at 160 kbps; its source falls back to `Bitrate160`. The highest it offers is 320 kbps. Spotify's Lossless tier is not available through spotifyd: its streaming library, librespot, closed [the request as not planned](https://github.com/librespot-org/librespot/issues/1580). Lossless works only in Spotify's own apps and on supported hardware.

## Troubleshooting

| You see | Cause and fix |
|---|---|
| `Error: reading …/spotui/config.json` and `No such file or directory` | There is no config file. Create it as described in step 2. |
| `no saved login at …/tokens.json; run spotui login` | You have not logged in yet. Run `spotui login`. |
| `Run spotui login again to grant the new permissions.` | Your saved login predates a permission the feature needs. Run `spotui login`. |
| `No device is playing. Start one, then run spotui transfer <device name>.` | No device is active. Start spotifyd, then run `spotui transfer spotui`, or press `d` in the UI. |
| `port 8888 is busy; set another port in config.json` | Another program uses the login port. Change `port` in `config.json` and add the matching redirect URI in the dashboard. |
| `spotui devices` lists no `spotui` device | Run `systemctl --user status spotifyd` and `journalctl --user -u spotifyd -n 30` to see why spotifyd stopped. |

## Development

```bash
cargo test                  # HTTP calls are checked against a local mock server
cargo clippy --all-targets
cargo fmt --check
```

| File | Contents |
|---|---|
| `src/auth.rs` | PKCE login, the loopback callback, token storage and refresh |
| `src/api.rs` | The Spotify Web API calls and their response types |
| `src/app.rs` | UI state and key handling, with no terminal or network access |
| `src/tui.rs` | Drawing with ratatui and the event loop that runs API calls in the background |
| `src/main.rs` | The command-line entry point |
