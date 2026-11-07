//! Records a scripted demo (for the README / social GIF) with the real plugin exe.
//! Like fake_deck, it pretends to be Stream Deck + League, then saves one snapshot
//! per step: <out>/NN/<key>.svg, dials.json (touch strip) and meta.json (caption).
//!
//! Run: cargo build --release --bins --examples
//!      cargo run --release --example demo_frames -- target/demo

use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

fn players() -> Value {
    let p = |id: &str, team: &str, champ: &str, raw: &str, s1: &str, s2: &str, items: &[i64]| {
        json!({
            "riotId": id, "team": team, "championName": champ,
            "rawChampionName": format!("game_character_displayname_{raw}"),
            "summonerSpells": {
                "summonerSpellOne": { "displayName": "", "rawDisplayName": format!("GeneratedTip_SummonerSpell_{s1}_DisplayName") },
                "summonerSpellTwo": { "displayName": "", "rawDisplayName": format!("GeneratedTip_SummonerSpell_{s2}_DisplayName") }
            },
            "items": items.iter().map(|i| json!({ "itemID": i })).collect::<Vec<_>>()
        })
    };
    json!([
        p("Me#EUW", "ORDER", "Ahri", "Ahri", "SummonerFlash", "SummonerDot", &[]),
        p("Top#1", "CHAOS", "Wukong", "MonkeyKing", "SummonerFlash", "SummonerDot", &[3158]),
        p("Jgl#1", "CHAOS", "Lee Sin", "LeeSin", "SummonerFlash", "S5_SummonerSmiteDuel", &[]),
        p("Mid#1", "CHAOS", "Kai'Sa", "Kaisa", "SummonerFlash", "SummonerBarrier", &[]),
        p("Bot#1", "CHAOS", "Jinx", "Jinx", "SummonerHeal", "SummonerFlash", &[3171]),
        p("Sup#1", "CHAOS", "Nunu & Willump", "Nunu", "SummonerExhaust", "SummonerFlash", &[])
    ])
}

fn serve_live_client(listener: TcpListener) {
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let body = if req.contains("/activeplayername") { json!("Me#EUW").to_string() } else { players().to_string() };
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        }
    });
}

struct Deck {
    ws: WebSocket<TcpStream>,
    shown: BTreeMap<String, Value>,
    out: String,
    frame: usize,
}

impl Deck {
    fn send(&mut self, v: Value) {
        self.ws.send(Message::Text(v.to_string().into())).unwrap();
    }

    fn pump(&mut self, secs: f64) {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < until {
            if let Ok(Message::Text(t)) = self.ws.read() {
                let v: Value = serde_json::from_str(&t).unwrap();
                if matches!(v["event"].as_str(), Some("setImage" | "setFeedback")) {
                    self.shown.insert(v["context"].as_str().unwrap().to_string(), v);
                }
            }
        }
    }

    fn press(&mut self, ctx: &str, hold_ms: u64) {
        self.send(json!({ "event": "keyDown", "context": ctx }));
        std::thread::sleep(Duration::from_millis(hold_ms));
        self.send(json!({ "event": "keyUp", "context": ctx }));
    }

    /// Moves the soonest `n` timers by `ticks` dial clicks (5 s each), keeping their order.
    fn fast_forward(&mut self, n: usize, ticks: i64) {
        for d in 0..n {
            self.send(json!({ "event": "dialRotate", "context": format!("d{d}"), "payload": { "ticks": ticks } }));
        }
    }

    fn snapshot(&mut self, caption: &str, pressed: &str, hold_ms: u64) {
        let dir = format!("{}/{:02}", self.out, self.frame);
        std::fs::create_dir_all(&dir).unwrap();
        let mut dials = serde_json::Map::new();
        for (ctx, v) in &self.shown {
            if v["event"] == "setImage" {
                let url = v["payload"]["image"].as_str().unwrap();
                let svg = base64::engine::general_purpose::STANDARD
                    .decode(url.trim_start_matches("data:image/svg+xml;base64,"))
                    .unwrap();
                std::fs::write(format!("{dir}/{ctx}.svg"), svg).unwrap();
            } else {
                dials.insert(ctx.clone(), v["payload"].clone());
            }
        }
        std::fs::write(format!("{dir}/dials.json"), Value::Object(dials).to_string()).unwrap();
        std::fs::write(
            format!("{dir}/meta.json"),
            json!({ "caption": caption, "pressed": pressed, "hold_ms": hold_ms }).to_string(),
        )
        .unwrap();
        self.frame += 1;
    }
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: demo_frames <out_dir>");
    std::fs::create_dir_all(&out).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = http.local_addr().unwrap().port();
    serve_live_client(http);

    let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = ws_listener.local_addr().unwrap().port();
    let mut child = Command::new("target/release/summoner-watch.exe")
        .args(["-port", &port.to_string(), "-pluginUUID", "DEMO", "-registerEvent", "registerPlugin", "-info", "{}"])
        .env("LOL_LIVE_URL", format!("http://127.0.0.1:{http_port}/liveclientdata"))
        .spawn()
        .expect("start plugin exe");
    let (stream, _) = ws_listener.accept().unwrap();
    let ws = tungstenite::accept(stream).unwrap();
    ws.get_ref().set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut deck = Deck { ws, shown: BTreeMap::new(), out, frame: 0 };

    deck.pump(0.3);
    deck.send(json!({ "event": "didReceiveGlobalSettings", "payload": { "settings": { "offset": "3" } } }));
    deck.send(json!({ "event": "applicationDidLaunch", "payload": { "application": "League of Legends.exe" } }));
    // Stream Deck + layout: 4 keys per row, enemies 1-5, then Insight and Undo.
    let keys = [("k0", 0, 0, "enemy"), ("k1", 1, 0, "enemy"), ("k2", 2, 0, "enemy"), ("k3", 3, 0, "enemy"),
                ("k4", 0, 1, "enemy"), ("insight", 1, 1, "insight"), ("reset", 2, 1, "reset")];
    for (ctx, col, row, action) in keys {
        deck.send(json!({ "event": "willAppear", "action": format!("com.vexify4103.summonerwatch.{action}"), "context": ctx,
            "payload": { "settings": {}, "controller": "Keypad", "coordinates": { "column": col, "row": row } } }));
    }
    for d in 0..4 {
        deck.send(json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.dial", "context": format!("d{d}"),
            "payload": { "settings": {}, "controller": "Encoder", "coordinates": { "column": d, "row": 0 } } }));
    }
    deck.pump(6.0); // game data + icons

    deck.snapshot("Gegner werden automatisch erkannt", "", 2200);

    deck.press("k0", 60); // Wukong: tap = Flash (waits 0.3 s for a possible double-tap)
    deck.pump(0.6);
    deck.snapshot("Wukong flasht → Taste antippen", "k0", 1800);

    deck.press("k4", 60); // Nunu: Flash is on F, still a tap
    deck.pump(0.5);
    deck.snapshot("Nunu flasht → antippen (Flash ist immer Tap)", "k4", 1800);

    deck.press("k2", 60); // Kai'Sa
    deck.pump(0.5);
    deck.snapshot("Kai'Sa flasht → antippen", "k2", 1600);

    for _ in 0..4 {
        deck.fast_forward(3, -12); // one minute
        deck.pump(0.5);
        deck.snapshot("⏩ Zeitraffer", "", 650);
    }
    deck.fast_forward(1, -12); // Wukong's Flash runs out
    deck.pump(1.6);
    deck.snapshot("Flash ist zurück → Taste wird grün + Sound 🔔", "", 3200);

    let _ = deck.ws.close(None);
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    println!("frames: {}", deck.frame);
}
