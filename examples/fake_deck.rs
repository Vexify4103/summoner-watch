//! Local end-to-end test without Stream Deck or League running.
//! Pretends to be the Stream Deck app (WebSocket) and the League Live Client
//! (HTTP), drives the real plugin exe, and writes what the keys would show.
//!
//! Run: cargo build --release && cargo run --release --example fake_deck -- <out_dir>

use base64::Engine;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};
use tungstenite::Message;

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

fn main() {
    let out_dir = std::env::args().nth(1).expect("usage: fake_deck <out_dir>");
    std::fs::create_dir_all(&out_dir).unwrap();

    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = http.local_addr().unwrap().port();
    serve_live_client(http);

    let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = ws_listener.local_addr().unwrap().port();
    let mut child = Command::new("target/release/summoner-watch.exe")
        .args(["-port", &port.to_string(), "-pluginUUID", "TEST", "-registerEvent", "registerPlugin", "-info", r#"{"devices":[{"id":"xl","size":{"columns":8,"rows":4}}]}"#])
        .env("LOL_LIVE_URL", format!("http://127.0.0.1:{http_port}/liveclientdata"))
        .spawn()
        .expect("start plugin exe");

    let (stream, _) = ws_listener.accept().unwrap();
    let mut ws = tungstenite::accept(stream).unwrap();
    ws.get_ref().set_read_timeout(Some(Duration::from_millis(100))).unwrap();

    let mut images: std::collections::BTreeMap<String, Value> = Default::default();
    let mut log = Vec::new();
    let mut pump = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, secs: f64, log: &mut Vec<String>| {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < until {
            if let Ok(Message::Text(t)) = ws.read() {
                let v: Value = serde_json::from_str(&t).unwrap();
                let ev = v["event"].as_str().unwrap_or("").to_string();
                match ev.as_str() {
                    "setImage" | "setFeedback" => {
                        images.insert(v["context"].as_str().unwrap().to_string(), v.clone());
                    }
                    "logMessage" => log.push(v["payload"]["message"].as_str().unwrap_or("").to_string()),
                    _ => log.push(format!("{ev}: {}", v)),
                }
            }
        }
    };
    let send = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value| ws.send(Message::Text(v.to_string().into())).unwrap();

    pump(&mut ws, 0.5, &mut log);
    send(&mut ws, json!({ "event": "didReceiveGlobalSettings", "payload": { "settings": { "offset": "3", "volume": 40, "sound": "ding" } } }));
    // "Test sound" button in the settings panel.
    send(&mut ws, json!({ "event": "sendToPlugin", "context": "pi", "action": "com.vexify4103.summonerwatch.enemy", "payload": { "event": "testSound" } }));
    send(&mut ws, json!({ "event": "applicationDidLaunch", "payload": { "application": "League of Legends.exe" } }));
    let keys = [("k0", 0, 0), ("k1", 1, 0), ("k2", 2, 0), ("k3", 3, 0), ("k4", 0, 1)];
    for (ctx, col, row) in keys {
        send(&mut ws, json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.enemy", "context": ctx, "payload": { "settings": {}, "coordinates": { "column": col, "row": row } } }));
    }
    send(&mut ws, json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.insight", "context": "insight", "payload": { "settings": {}, "coordinates": { "column": 1, "row": 1 } } }));
    send(&mut ws, json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.reset", "context": "reset", "payload": { "settings": {}, "coordinates": { "column": 2, "row": 1 } } }));
    for d in 0..2 {
        send(&mut ws, json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.dial", "context": format!("dial{d}"), "payload": { "settings": {}, "controller": "Encoder", "coordinates": { "column": d, "row": 0 } } }));
    }
    // Stream Deck XL: enemy key at column 4 (= enemy 5) and a Timer action on a normal key.
    send(&mut ws, json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.enemy", "device": "xl", "context": "xl_enemy5", "payload": { "settings": {}, "controller": "Keypad", "coordinates": { "column": 4, "row": 0 } } }));
    for t in 0..4 {
        send(&mut ws, json!({ "event": "willAppear", "action": "com.vexify4103.summonerwatch.dial", "device": "xl", "context": format!("xl_timer{}", t + 1), "payload": { "settings": {}, "controller": "Keypad", "coordinates": { "column": t, "row": 1 } } }));
    }
    pump(&mut ws, 6.0, &mut log); // worker polls + Data Dragon download

    // Wukong (Lucidity boots): tap = Flash. Lee Sin: Smite is ignored, so any press = Flash. Kai'Sa: toggle Insight then Flash.
    let press = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, ctx: &str, hold_ms: u64| {
        ws.send(Message::Text(json!({ "event": "keyDown", "context": ctx }).to_string().into())).unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
        ws.send(Message::Text(json!({ "event": "keyUp", "context": ctx }).to_string().into())).unwrap();
    };
    press(&mut ws, "k0", 50);
    press(&mut ws, "k1", 600);
    press(&mut ws, "insight", 50);
    press(&mut ws, "k2", 50);
    press(&mut ws, "k2", 50);
    press(&mut ws, "k4", 50);
    pump(&mut ws, 1.5, &mut log);
    // Double-tap Jinx = her other spell (Heal).
    press(&mut ws, "k3", 40);
    std::thread::sleep(Duration::from_millis(80));
    press(&mut ws, "k3", 40);
    // Hold Wukong (top, no TP) = quest Teleport. Keep reading during the hold,
    // like Stream Deck does, so the plugin is not blocked sending images.
    send(&mut ws, json!({ "event": "keyDown", "context": "k0" }));
    pump(&mut ws, 0.6, &mut log);
    send(&mut ws, json!({ "event": "keyUp", "context": "k0" }));
    pump(&mut ws, 1.0, &mut log);
    // Jinx's timer (k4 is the 5th key = enemy 5? no: row 1 col 0 = index 4 = Nunu). Shrink the soonest one to test expiry + sound.
    send(&mut ws, json!({ "event": "dialRotate", "context": "dial0", "payload": { "ticks": -100 } }));
    pump(&mut ws, 3.0, &mut log);

    for (ctx, v) in &images {
        if v["event"] == "setImage" {
            let url = v["payload"]["image"].as_str().unwrap();
            let svg = base64::engine::general_purpose::STANDARD.decode(url.trim_start_matches("data:image/svg+xml;base64,")).unwrap();
            std::fs::write(format!("{out_dir}/{ctx}.svg"), svg).unwrap();
        } else {
            let mut fb = v["payload"].clone();
            for k in ["champ", "spell"] {
                let s = fb[k].as_str().unwrap_or("");
                fb[k] = json!(format!("<{} chars>", s.len()));
            }
            println!("{ctx} feedback: {fb}");
        }
    }
    for l in &log {
        println!("log: {l}");
    }
    let _ = ws.close(None);
    std::thread::sleep(Duration::from_millis(500));
    println!("plugin exited by itself: {}", child.try_wait().unwrap().is_some());
    let _ = child.kill();
}
