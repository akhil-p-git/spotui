#!/bin/bash
# Runs inside Terminal: first-run setup, then starts spotifyd (the player) and spotui.
res="$(cd "$(dirname "$0")" && pwd)"
export PATH="$res/bin:$PATH"

conf="$HOME/.config/spotui"
cache="$HOME/Library/Caches/spotui/spotifyd"
log="$HOME/Library/Logs/spotui-spotifyd.log"
sd_conf="$conf/spotifyd.conf"
mkdir -p "$conf" "$cache" "$(dirname "$log")"

pause_and_exit() {
  read -r -p "Press Enter to close. " _
  exit 1
}

if [ ! -f "$conf/config.json" ]; then
  cat <<'TXT'
First run: spotui needs a Spotify app of your own (free, takes two minutes).

  1. Open https://developer.spotify.com/dashboard and create an app.
  2. Choose "Web API" when it asks which APIs you plan to use.
  3. Add this redirect URI exactly: http://127.0.0.1:8888/callback
  4. Save, then copy the app's Client ID.

TXT
  read -r -p "Client ID: " client_id
  if [[ ! "$client_id" =~ ^[A-Za-z0-9]+$ ]]; then
    echo "That does not look like a Client ID (letters and digits only)."
    pause_and_exit
  fi
  printf '{\n  "client_id": "%s",\n  "port": 8888\n}\n' "$client_id" > "$conf/config.json"
fi

if [ ! -f "$conf/tokens.json" ]; then
  echo "Logging spotui in to Spotify. Approve it in your browser."
  spotui login || pause_and_exit
fi

cat > "$sd_conf" <<CONF
[global]
device_name = "spotui"
device_type = "computer"
bitrate = 320
cache_path = "$cache"
CONF

sd_args=(--config-path "$sd_conf" --cache-path "$cache")

if [ ! -f "$cache/oauth/credentials.json" ]; then
  echo "Logging the player (spotifyd) in to Spotify. Approve it in your browser."
  spotifyd "${sd_args[@]}" authenticate || pause_and_exit
fi

if pgrep -x spotifyd > /dev/null; then
  echo "spotifyd is already running; using it."
else
  spotifyd "${sd_args[@]}" --no-daemon > "$log" 2>&1 &
  sd_pid=$!
  trap 'kill "$sd_pid" 2>/dev/null' EXIT
fi

echo "Starting spotui. Press D and choose the \"spotui\" device before playing."
sleep 2
spotui
