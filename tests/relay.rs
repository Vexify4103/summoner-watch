#![cfg(feature = "relay")]
//! The real relay over real sockets: room isolation, ordering, idempotent retries,
//! relay key, rate limit and per-IP connection limit.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use summoner_watch::relay::{serve, Config};
use summoner_watch::shared::{Command, Hello, Key, Operation, Snapshot};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

type Socket = WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

fn relay(access_key: &str, ip_header: Option<&str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let config = Config { access_key: access_key.into(), ip_header: ip_header.map(String::from) };
    std::thread::spawn(move || serve(listener, config));
    addr
}

fn open(addr: &str, forwarded_for: Option<&str>) -> Socket {
    let mut request = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
    if let Some(ip) = forwarded_for {
        request.headers_mut().insert("X-Forwarded-For", ip.parse().unwrap());
    }
    let stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    tungstenite::client(request, tungstenite::stream::MaybeTlsStream::Plain(stream)).unwrap().0
}

fn hello(ws: &mut Socket, room: &str, who: char, key: &str) {
    let hello = Hello { room: room.into(), client: who.to_string().repeat(32), roster: vec!["Ahri".into(), "Lux".into()], key: key.into() };
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap())).unwrap();
}

fn send(ws: &mut Socket, ops: &[(u64, Command)]) {
    let ops: Vec<Operation> = ops.iter().cloned().map(|(sequence, command)| Operation { sequence, command }).collect();
    ws.send(Message::Text(serde_json::to_string(&ops).unwrap())).unwrap();
}

/// Next text message from the relay.
fn next(ws: &mut Socket) -> String {
    loop {
        match ws.read().expect("relay message") {
            Message::Text(text) => return text,
            Message::Close(_) => panic!("closed"),
            _ => {}
        }
    }
}

/// Reads snapshots until one matches.
fn until(ws: &mut Socket, ready: impl Fn(&Snapshot) -> bool) -> Snapshot {
    for _ in 0..20 {
        let snapshot: Snapshot = serde_json::from_str(&next(ws)).unwrap();
        if ready(&snapshot) {
            return snapshot;
        }
    }
    panic!("no matching snapshot");
}

fn key(champ: &str) -> Key {
    Key { champion: champ.into(), spell: "SummonerFlash".into() }
}

fn start(champ: &str) -> Command {
    Command::Start { key: key(champ), remaining_ms: 300_000, total_ms: 300_000 }
}

fn champs(s: &Snapshot) -> Vec<&str> {
    s.timers.iter().map(|t| t.key.champion.as_str()).collect()
}

#[test]
fn teammates_share_one_room_and_retries_apply_once() {
    let addr = relay("", None);
    let room = "01".repeat(32);
    let (mut a, mut b) = (open(&addr, None), open(&addr, None));
    hello(&mut a, &room, 'a', "");
    hello(&mut b, &room, 'b', "");
    until(&mut a, |s| s.timers.is_empty());
    until(&mut b, |s| s.timers.is_empty());

    send(&mut a, &[(1, start("Ahri"))]);
    let seen = until(&mut b, |s| !s.timers.is_empty());
    assert_eq!(champs(&seen), ["Ahri"], "pushed to the teammate");
    let original = seen.timers[0].end_ms;

    send(&mut b, &[(1, start("Lux"))]);
    until(&mut a, |s| s.timers.len() == 2);
    // b undoes its own start; a retries an adjustment (same sequence twice).
    send(&mut b, &[(2, Command::Undo)]);
    until(&mut a, |s| s.timers.len() == 1);
    let adjust = (2, Command::Adjust { key: key("Ahri"), delta_ms: -5000 });
    send(&mut a, std::slice::from_ref(&adjust));
    send(&mut a, &[adjust]);
    // A later operation: once it is acknowledged, both copies were handled.
    send(&mut a, &[(3, Command::Cancel { key: key("Lux") })]);
    let s = until(&mut a, |s| s.acknowledged == 3);
    assert_eq!(s.timers[0].end_ms, original - 5000, "retry applied once");
    let teammate = until(&mut b, |t| t.revision == s.revision);
    assert_eq!(teammate.timers, s.timers);

    // Another room, or another enemy roster, is separate.
    let mut c = open(&addr, None);
    hello(&mut c, &"02".repeat(32), 'c', "");
    assert!(until(&mut c, |_| true).timers.is_empty());
}

#[test]
fn private_relay_rejects_wrong_key_and_bad_hello() {
    let addr = relay(&"cd".repeat(16), None);
    let mut ws = open(&addr, None);
    hello(&mut ws, &"01".repeat(32), 'a', &"00".repeat(16));
    assert!(next(&mut ws).contains("invalid relay key"));

    let mut ws = open(&addr, None);
    hello(&mut ws, "short", 'a', &"cd".repeat(16));
    assert!(next(&mut ws).contains("invalid hello"));

    let mut ws = open(&addr, None);
    hello(&mut ws, &"01".repeat(32), 'a', &"cd".repeat(16));
    until(&mut ws, |s| s.timers.is_empty());
    // A start for a champion outside the roster is refused and closes the connection.
    send(&mut ws, &[(1, start("Zed"))]);
    assert!(next(&mut ws).contains("error"));
}

#[test]
fn flooding_closes_the_connection() {
    let addr = relay("", None);
    let mut ws = open(&addr, None);
    hello(&mut ws, &"01".repeat(32), 'a', "");
    for _ in 0..40 {
        if ws.send(Message::Text("[]".into())).is_err() {
            break;
        }
    }
    let mut saw_error = false;
    while let Ok(message) = ws.read() {
        if let Message::Text(text) = message {
            saw_error |= text.contains("too many messages");
        }
    }
    assert!(saw_error);
}

#[test]
fn connections_per_ip_are_limited_and_proxy_header_is_honoured() {
    let addr = relay("", Some("X-Forwarded-For"));
    let room = "01".repeat(32);
    // Same forwarded IP: the ninth connection is closed right away.
    let mut open_sockets: Vec<Socket> = (0..8).map(|_| open(&addr, Some("203.0.113.7"))).collect();
    for (n, ws) in open_sockets.iter_mut().enumerate() {
        hello(ws, &room, char::from(b"01234567"[n]), "");
        until(ws, |_| true);
    }
    let mut ninth = open(&addr, Some("203.0.113.7"));
    hello(&mut ninth, &room, '8', "");
    assert!(!matches!(ninth.read(), Ok(Message::Text(_))));
    // Another client IP behind the same proxy still gets in.
    let mut other = open(&addr, Some("198.51.100.1"));
    hello(&mut other, &room, '9', "");
    until(&mut other, |_| true);
}

#[test]
fn health_check_answers_plain_http() {
    let addr = relay("", None);
    let mut stream = TcpStream::connect(&addr).unwrap();
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200") && response.ends_with("{\"ok\":true}"));
}
