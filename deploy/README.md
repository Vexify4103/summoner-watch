# Team timer relay on a Raspberry Pi

Use a 64-bit Raspberry Pi OS installation with Docker Compose. Build on the Pi;
Docker selects the Pi's native architecture. No Riot API key is needed by the relay.

From the repository root:

```sh
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

## Public or private

By default the relay is **public**: any plugin that knows the URL can use it. The
plugin is open source, so a key built into it would not keep other programs out.
Rooms are separated by their code instead, and each room only holds timers.

For a relay only your friends use, set a key before starting it and give it to them
for the **Relay key** setting:

```sh
export SUMMONER_RELAY_KEY=$(openssl rand -hex 16)
docker compose -f deploy/compose.yaml up -d --build
```

## How rooms work

1. Everyone enables **Sync** and enters `https://your-hostname` under **Relay URL**.
2. Start a match. The status changes from **Waiting for a game** to **Connected**.
3. Teammates in the same match join the same room automatically. The room code is a
   SHA-256 hash of the ten player names, your team side and the optional **Party
   code**. The enemy team gets a different room.
4. The enemy team can see the player names, so the room code alone is not a
   secret. A premade enters the same **Party code** to keep everyone else out.
5. Spectators and replays never join a room.

Everyone in a room can start, restart, adjust and cancel timers. **Undo** reverses
only the presser's own newest start; undoing a restart restores the old deadline.
Holding **Undo / Reset** removes only the presser's own starts. Dial changes and
cancellations are shared but are not separate undo steps. Up to 256 starts can be
undone.

The PC that starts a timer calculates the cooldown with its own haste and offset
settings; the resulting timer is shared. Sound, volume, spell order and CI editing
remain local preferences.

Without a connection, timers work locally. On (re)connect, local countdowns the room
does not have are added to it (they never replace a room timer and are not undo
steps), then the room's timers are shown. Turning sync off keeps the countdowns on
that PC; later changes stay local.

## Privacy and limits

The relay keeps timers, a bounded undo history, random client IDs, enemy
champion/spell identifiers and room codes **in memory only**. Rooms expire after two
hours without requests; restarting the server clears everything. It never receives
Riot IDs, the party code itself, Riot API keys, sound files or analytics. The HTTPS
proxy/network layer sees client IP addresses; the supplied Caddy config does not
enable access logging. The relay has limits of 128 rooms and 32 client sessions per
room; restarting the plugin creates a new session.

For local development without Docker:

```sh
cargo run --release --features relay --bin summoner-watch-relay
```

It binds to `127.0.0.1:8787` by default. `SUMMONER_RELAY_BIND` can override this for
container networking; do not expose its HTTP listener directly to the internet.
