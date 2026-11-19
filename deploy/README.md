# Team timer relay on a Raspberry Pi

Use a 64-bit Raspberry Pi OS installation with Docker Compose. Build on the Pi;
Docker selects the Pi's native architecture. No Riot API key is needed by the relay.

Plugins connect with a WebSocket to `wss://your-hostname/v1/ws`; changes are
pushed to teammates right away. The client accepts plain HTTP only on localhost for
testing or an SSH tunnel; certificates are verified for every remote connection.

The relay must be reachable over HTTPS. Pick one of the two ways below.

## Option A: Cloudflare Tunnel (recommended at home)

The Pi opens an outgoing connection to Cloudflare, so you need no port forwarding,
no DDNS and no public IPv4 (it also works behind DS-Lite or CGNAT). Your home IP
address stays hidden. Cloudflare provides the HTTPS certificate for free.

1. Add your domain to Cloudflare (free plan) and change its nameservers at your
   registrar to the two Cloudflare shows. Existing records (website, email) are
   copied and keep pointing where they did; check the list before switching.
2. In the Cloudflare dashboard go to **Networking > Tunnels > Create a tunnel**.
   Choose **Docker** and copy only the token (the long string after `--token`).
3. Open the tunnel, **Routes > Add route > Published application**: subdomain
   (for example `sync`), your domain, Service URL `http://relay:8787`.
4. On the Pi, from the repository root:

   ```sh
   cp deploy/.env.example deploy/.env
   chmod 600 deploy/.env
   nano deploy/.env   # paste the token after TUNNEL_TOKEN=
   docker compose -f deploy/compose.yaml up -d --build
   curl https://sync.example.com/health
   ```

The health response is `{"ok":true}`. `deploy/.env` is ignored by git; never commit
the token. If it leaks, use **Refresh token** in the tunnel settings and update
`deploy/.env`.

### Low-memory Pi

Compiling Rust needs about 1 GB of free memory. If the Pi has less, build the
relay on a PC with Docker and copy only the binary (from the repository root):

```sh
docker run --rm -v "$PWD:/src" -w /src rust:1-bookworm bash -c '
  apt-get update && apt-get install -y gcc-aarch64-linux-gnu &&
  rustup target add aarch64-unknown-linux-gnu &&
  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc   CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc CARGO_TARGET_DIR=target-docker   cargo build --release --locked --features relay --bin summoner-watch-relay     --target aarch64-unknown-linux-gnu'
scp target-docker/aarch64-unknown-linux-gnu/release/summoner-watch-relay   pi:summoner-watch/deploy/prebuilt/
```

Then set `RELAY_DOCKERFILE=deploy/Dockerfile.prebuilt` in `deploy/.env` on the Pi
and start compose as above.

## Option B: your own reverse proxy (Caddy)

Leave `COMPOSE_PROFILES` empty in `deploy/.env` (or do not create the file), then:

```sh
docker compose -f deploy/compose.yaml up -d --build
curl http://127.0.0.1:8787/health
```

The container listens only on the Pi's loopback interface. Point your hostname at
the Pi's public connection and forward ports 80 and 443 to the Pi.

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
openssl rand -hex 16   # put the result after SUMMONER_RELAY_KEY= in deploy/.env
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

## Limits

Each connection thread uses little memory; the container is capped at 128 MB.

| Limit | Value |
|---|---|
| Connections in total / per client IP | 512 / 8 |
| Rooms in total | 1024 |
| New rooms per client IP per hour | 30 |
| Messages per connection | 10 per second, bursts of 30 |
| Message size | 16 KB |
| First message (hello) | within 5 s |
| Silent connection closed after | 60 s (plugins ping every 10 s) |

A client that breaks a limit is disconnected. Limits per client IP need the real
address: the relay reads it from `SUMMONER_RELAY_IP_HEADER` (default
`X-Forwarded-For` in `compose.yaml`, `CF-Connecting-IP` in `.env.example` for the
tunnel), but only when the connection comes from a loopback or private address,
such as the tunnel container, Caddy on the same machine or the Docker bridge. Without the header every
client looks like the proxy, and the per-IP limits apply to everyone together.

## Privacy

The relay keeps timers, a bounded undo history, random client IDs, enemy
champion/spell identifiers and room codes **in memory only**. A room expires two hours
after its last player left; restarting the server clears everything. It never receives
Riot IDs, the party code itself, Riot API keys, sound files or analytics. The HTTPS
proxy/network layer sees client IP addresses; the supplied Caddy config does not
enable access logging. A room holds at most 32 connected plugins at once.

For local development without Docker:

```sh
cargo run --release --features relay --bin summoner-watch-relay
```

It binds to `127.0.0.1:8787` by default. `SUMMONER_RELAY_BIND` can override this for
container networking; do not expose its plain listener directly to the internet.
