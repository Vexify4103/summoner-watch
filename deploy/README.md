# Team timer relay on a Raspberry Pi

Use a 64-bit Raspberry Pi OS installation with Docker Compose. Build on the Pi;
Docker selects the Pi's native architecture. No Riot API key is needed by the relay.

From the repository root:

```sh
export SUMMONER_RELAY_KEY=$(openssl rand -hex 16)
# Save this key privately; use the same value when recreating the container.
docker compose -f deploy/compose.yaml up -d --build
curl http://127.0.0.1:8787/health
```

The health response is `{"ok":true}`. The container listens only on the Pi's
loopback interface. Put an HTTPS reverse proxy in front of it before connecting
teammates over the internet. The client accepts plain HTTP only on localhost for
testing or an SSH tunnel; certificates are verified for every remote connection.

Point your hostname at the Pi's public connection and forward ports 80 and 443 to
the Pi. If your connection is behind CGNAT, use a tunnel/reverse proxy offering a
public HTTPS endpoint instead of router port forwarding.

`Caddyfile.example` is a complete configuration for a **host-installed Caddy**.
Replace `timers.example.com` with your hostname, merge with any existing Caddy
configuration, validate it with `caddy validate --config /etc/caddy/Caddyfile`, and
reload Caddy. Do not replace an existing configuration blindly. Caddy handles the
TLS certificate. Keep port 8787 private. If Caddy also runs in Docker, connect it
to the relay's Docker network and proxy to `relay:8787` instead.

In the plugin's settings:

1. Enter `https://your-hostname` under **Relay URL**.
2. Enter the server's `SUMMONER_RELAY_KEY` under **Relay key**. Share it only with
   people you allow to use the relay; it prevents unauthenticated room creation.
3. Click **New private room** and share that 32-character code privately with
   your teammates. Everyone enters the same URL and code and enables **Sync**.
4. Start the same match. The status changes from **Waiting for a game** to
   **Connected** once the plugin reads the enemy roster and reaches the relay.
5. Use a **new room code for each match**. The roster separates different enemy
   teams, but does not uniquely identify a rematch with identical champions.

Anyone with the relay key and room code can start, restart, adjust, cancel, undo and reset its
timers. Undo reverses the latest **accepted timer start in the room**, regardless
of which teammate pressed it. Undoing a restart restores the old deadline. Dial
changes and cancellations are shared but are not separate undo steps. Cancelled
and expired timer starts are skipped. Holding Undo / Reset clears the room's
timers and undo history. Up to 256 starts can be undone.

The initiating PC calculates the cooldown using its haste and offset settings;
the resulting timer is shared. Sound, volume, spell order and CI editing remain
local preferences. Coordinate haste settings and manual CI choices with teammates.

Joining a room replaces local timers with the room's current state. Existing
local timers are never uploaded automatically. Disabling sync keeps the currently
displayed countdowns on that PC, and later changes stay local. During an outage,
existing countdowns continue, but new shared edits are refused until a successful
connection. Already submitted requests are retried idempotently for up to 5 seconds;
old clicks are discarded after that. A request whose response was lost may already
have committed; the next snapshot shows the authoritative result.

The relay keeps timers, a bounded undo history, random client IDs, champion/spell
identifiers and room codes **in memory only**. Rooms expire after two hours without
requests; restarting the server clears everything. It never receives Riot IDs,
Riot API keys, sound files or analytics. The HTTPS proxy/network layer sees client
IP addresses; the supplied Caddy config does not enable access logging. Do not
enable header/body logging containing room codes or relay keys. The code is the room's access
credential, not an account password. The relay has limits of 128 rooms and 32
client sessions per room; restarting the plugin creates a new session.

For local development without Docker:

```sh
export SUMMONER_RELAY_KEY=$(openssl rand -hex 16)
cargo run --release --features relay --bin summoner-watch-relay
```

It binds to `127.0.0.1:8787` by default. `SUMMONER_RELAY_BIND` can override this for
container networking; do not expose its HTTP listener directly to the internet.
