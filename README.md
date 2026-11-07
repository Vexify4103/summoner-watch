# Summoner Watch

Enemy summoner spell timers for League of Legends on your Stream Deck.

You press a key when an enemy uses a summoner spell. The key counts down, plays a short
sound when the spell is back, and stays green for 20 seconds so you can glance at it.
Champions, spell icons, cooldowns, boots and levels fill in automatically.

- Works on Stream Deck + (keys, dials and touch strip) and on Stream Deck XL / MK.2 (keys only).
- Native Rust exe: ~1.5 MB, a few MB of RAM, idle while League is closed.
- Windows only (League's Live Client API and sound playback). Stream Deck app 6.9 or newer.

**Fair play:** Summoner Watch never detects when an enemy uses a spell. Timers start only
when you press a key. It reads the same data the in-game scoreboard shows, through Riot's
official [Live Client Data API](https://developer.riotgames.com/docs/lol#game-client-api).

## Install

Download the latest `.streamDeckPlugin` file from
[Releases](https://github.com/Vexify4103/summoner-watch/releases) and double-click it.
Stream Deck installs the plugin. Your antivirus may ask about `summoner-watch.exe` the first
time, because the exe is not code-signed.

## Set up the page

1. In Stream Deck, create a new profile, for example "League".
2. In the profile settings, link the profile to the application `League of Legends.exe`.
   Stream Deck then switches to this profile while the game is in focus, and back to your
   main profile when it is not.
3. Drag the actions from the "Summoner Watch" category.

### Stream Deck +

| Position | Action | Use |
|---|---|---|
| Keys 1-4 (top row), key 5 (bottom left) | Enemy | Enemies 1-5 in scoreboard order (top, jungle, mid, bot, support) |
| Key 6 | Cosmic Insight Edit | Tap it, then tap an enemy, to toggle Cosmic Insight for that enemy |
| Key 7 | Undo / Reset | Tap: undo the last started timer. Hold: clear all timers |
| Key 8 | Stream Deck "Switch Profile" | Back to your main profile (built-in action) |
| Dials 1-4 | Timer | Dial 1 shows the soonest timer, dial 2 the next, and so on |

### Stream Deck XL, MK.2 and others without dials

The plugin reads each device's width, so automatic order still works. On an XL (8 columns)
the first 5 keys of the top row are enemies 1-5.

| Row | Keys |
|---|---|
| Top | Enemy x5, Cosmic Insight Edit, Undo / Reset, Switch Profile |
| Second | Timer x4 (or up to 8): soonest timer, 2nd soonest, ... |

A **Timer** key shows the champion, spell, countdown and a bar. **Tap = 5 s less** (for a late
press), **hold = cancel**. Timer keys are optional; the enemy keys already show their countdowns.

Enemy keys pick their enemy from their position. To choose one manually, open the key's
settings and set "Enemy".

## In game

- **Tap an enemy key:** start the timer for their more important spell (left icon).
- **Hold the key (0.5 s):** start the timer for their other spell (right icon).
- "More important" comes from the **Spell order** setting, not from D/F slots. Default:
  Flash > Teleport > Ignite > Cleanse > Exhaust > Barrier > Heal > Ghost > Smite.
  So Flash is always a tap, on D or F.
- **Smite** is ignored by default. The jungler's key shows only their other spell; tap and
  hold both start it.
- **Double-tap the enemy top laner:** start their quest Teleport (390 s). Only shown when the top
  laner has no Teleport of their own; a small TP icon sits left of the champion. Top laner =
  scoreboard position TOP, or the first enemy in custom games. Taps on that one key wait 0.3 s
  to tell a tap from a double-tap.
- **Unleashed Teleport** (Teleport after 10:00): cooldown follows the enemy's level,
  330 s at level 1 down to 240 s at level 18, read live from the scoreboard.
- **Tap a running spell again:** restart that timer.
- **Rotate a dial:** move that timer 5 s per click (fix a late press).
- **Push or tap a dial:** cancel that timer.

Badges on the key:
- **CI** (purple): the enemy has Cosmic Insight (+18 summoner spell haste).
- **B** (blue): the enemy owns Ionian Boots of Lucidity (+10 summoner haste).
- **B+** (red): the enemy owns Crimson Lucidity, the upgraded boots (+20 summoner haste).
- Both boots badges are detected automatically from the scoreboard.

## Cooldown math

`timer = base cooldown x 100 / (100 + summoner haste) - start early`

- Base cooldowns and icons come from Data Dragon (current patch). Offline fallback values are built in.
- "Start early" defaults to 3 s to cover your reaction time.
- Example: Flash with Cosmic Insight and Lucidity boots: 300 x 100 / 128 = 234 s, minus 3 s = 3:51.

Haste values are settings, because Riot changes them in balance patches.

## Settings (key settings panel, shared by all keys)

| Setting | Default | Meaning |
|---|---|---|
| Start early (s) | 3 | Subtracted from every timer |
| Insight haste | 18 | Summoner haste from Cosmic Insight |
| Lucidity haste | 10 | Summoner haste from Ionian Boots of Lucidity |
| Crimson Lucidity haste | 20 | Summoner haste from Crimson Lucidity |
| Quest TP cooldown (s) | 390 | Base cooldown of the top lane quest Teleport, haste applies |
| Spell order | Flash, Teleport, Ignite, Cleanse, Exhaust, Barrier, Heal, Ghost, Smite | Importance, most important first. Separate with `,` or `>`. `tp` works as Teleport. Unlisted spells rank last |
| Ignore spells | Smite | Never tracked. `none` = track everything |
| Volume | 60 | Built-in sound, 0 = silent |
| Custom .wav | empty | Path to your own sound file |
| Riot API key / Server | empty | Optional, see below |

## Optional: auto-detect Cosmic Insight

The local game API does not show minor runes. With your own Riot API key, the plugin asks
the Riot spectator API once per game which enemies took Cosmic Insight, and sets the CI badges.

- Get a key at [developer.riotgames.com](https://developer.riotgames.com). Development keys
  expire every 24 hours; for daily use, apply for a free Personal API Key.
- Without a key, use the Cosmic Insight Edit key manually.

## Privacy

- The plugin talks to `127.0.0.1:2999` (League's local game API), to
  `ddragon.leagueoflegends.com` (icons and cooldowns), and, only if you enter a key, to
  `*.api.riotgames.com`.
- Your Riot API key is stored in Stream Deck's settings on your PC. It is sent only to Riot.
- No analytics, no other servers.

## Build from source

Needs [Rust](https://rustup.rs) and Windows.

```
powershell -ExecutionPolicy Bypass -File install.ps1   # build + install into your Stream Deck
powershell -ExecutionPolicy Bypass -File package.ps1   # build dist\*.streamDeckPlugin to share
cargo test --release                                    # unit tests
cargo build --release --bins --examples
cargo run --release --example fake_deck -- target/preview   # end-to-end test without Stream Deck or League
```

`fake_deck` runs the real exe against a fake Stream Deck and a fake game, then writes the
key images as SVG files to `target/preview`.

The exe is built as a console-subsystem app on purpose. Stream Deck starts plugins without a
window, and a GUI-subsystem build was flagged by Kaspersky.

## Store images

`marketing/` holds the Elgato Marketplace thumbnail and gallery images (1920x960). They are
HTML pages using the real key images from `fake_deck`. To re-render one with Edge:

```
msedge --headless=new --hide-scrollbars --window-size=1920,960 --virtual-time-budget=8000 --screenshot=marketing	humbnail.png file:///<path>/marketing/thumbnail.html
```

The demo video (`marketing/demo.gif` / `demo.mp4`, German captions) is recorded from the real
plugin and rendered on a Stream Deck + mockup (needs Edge and ffmpeg):

```
cargo build --release --bins --examples
cargo run --release --example demo_frames -- target/demo
python -I tools/render_demo.py target/demo marketing/demo
```

## License

[MIT](LICENSE)

## Legal

Summoner Watch isn't endorsed by Riot Games and doesn't reflect the views or opinions of
Riot Games or anyone officially involved in producing or managing Riot Games properties.
Riot Games, and all associated properties are trademarks or registered trademarks of
Riot Games, Inc.

Stream Deck is a trademark of Corsair Memory, Inc. This project is not affiliated with Elgato or Corsair.
