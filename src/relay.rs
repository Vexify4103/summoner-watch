//! WebSocket relay for team sync. TLS is terminated by a reverse proxy.
//!
//! One thread per connection. A change in a room marks every member "dirty";
//! each connection thread then sends its member the room's current snapshot, so a
//! slow client never queues up old states.
use crate::shared::{valid_code, Exchange, Hello, Operation, Room};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

pub struct Config {
    /// Empty = public relay. Otherwise clients must send this key.
    pub access_key: String,
    /// Header with the real client IP, set by the reverse proxy (for example
    /// "X-Forwarded-For" or "CF-Connecting-IP"). Trusted only from a loopback or
    /// private peer, so clients on the internet cannot fake it.
    pub ip_header: Option<String>,
}

const MAX_CONNECTIONS: usize = 512;
const MAX_CONNECTIONS_PER_IP: usize = 8;
const MAX_ROOMS: usize = 1024;
const NEW_ROOMS_PER_IP_PER_HOUR: usize = 30;
const MAX_MESSAGE: usize = 16 * 1024;
/// Messages per second per connection, and the burst allowed.
const RATE: f64 = 10.0;
const BURST: f64 = 30.0;
const HELLO_WAIT: Duration = Duration::from_secs(5);
/// Clients ping every 10 s; a silent connection is closed after this.
const IDLE: Duration = Duration::from_secs(60);
const ROOM_IDLE: Duration = Duration::from_secs(2 * 3600);

struct Member {
    client: String,
    dirty: Arc<AtomicBool>,
}

struct Entry {
    room: Room,
    seen: Instant,
    members: Vec<Member>,
}

type RoomKey = (String, Vec<String>);

#[derive(Default)]
struct State {
    rooms: HashMap<RoomKey, Entry>,
    connections: usize,
    per_ip: HashMap<IpAddr, usize>,
    created: HashMap<IpAddr, Vec<Instant>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Accepts connections until the listener fails.
pub fn serve(listener: TcpListener, config: Config) {
    let state = Arc::new(Mutex::new(State::default()));
    let config = Arc::new(config);
    for stream in listener.incoming().flatten() {
        let (state, config) = (state.clone(), config.clone());
        let _ = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || connection(stream, &state, &config));
    }
}

/// Counts an open connection; frees its slot when dropped.
struct Slot<'a> {
    state: &'a Mutex<State>,
    ip: Option<IpAddr>,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut s = self.state.lock().unwrap();
        s.connections -= 1;
        if let Some(ip) = self.ip {
            if let Some(n) = s.per_ip.get_mut(&ip) {
                *n -= 1;
                if *n == 0 {
                    s.per_ip.remove(&ip);
                }
            }
        }
    }
}

fn trusted_proxy(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

fn reject(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(None);
    *response.status_mut() = status;
    response
}

// The handshake callback's error type is fixed by tungstenite.
#[allow(clippy::result_large_err)]
fn connection(stream: TcpStream, state: &Mutex<State>, config: &Config) {
    let Ok(peer) = stream.peer_addr() else { return };
    // Bounded handshake: slow or silent clients are dropped.
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    {
        let mut s = state.lock().unwrap();
        if s.connections >= MAX_CONNECTIONS {
            return;
        }
        s.connections += 1;
    }
    let mut slot = Slot { state, ip: None };

    // Plain-HTTP health check for Docker and monitoring.
    let mut head = [0u8; 12];
    if stream.peek(&mut head).is_ok_and(|n| head[..n].starts_with(b"GET /health ")) {
        // Read the request first: closing with unread data resets the connection.
        let _ = (&stream).read(&mut [0u8; 2048]);
        let body = "{\"ok\":true}";
        let _ = (&stream).write_all(
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                .as_bytes(),
        );
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return;
    }

    let mut forwarded = None;
    let callback = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        if request.uri().path() != "/v1/ws" {
            return Err(reject(StatusCode::NOT_FOUND));
        }
        if let Some(header) = config.ip_header.as_deref().filter(|_| trusted_proxy(peer.ip())) {
            // The proxy appends the address it saw last.
            forwarded = request
                .headers()
                .get(header)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit(',').next())
                .and_then(|v| v.trim().parse::<IpAddr>().ok());
        }
        Ok(response)
    };
    let ws_config = WebSocketConfig { max_message_size: Some(MAX_MESSAGE), max_frame_size: Some(MAX_MESSAGE), ..Default::default() };
    let Ok(mut ws) = tungstenite::accept_hdr_with_config(stream, callback, Some(ws_config)) else { return };
    let ip = forwarded.unwrap_or(peer.ip());
    {
        let mut s = state.lock().unwrap();
        let n = s.per_ip.entry(ip).or_insert(0);
        if *n >= MAX_CONNECTIONS_PER_IP {
            drop(s);
            let _ = ws.close(None);
            return;
        }
        *n += 1;
        slot.ip = Some(ip);
    }
    ws.get_ref().set_read_timeout(Some(Duration::from_millis(100))).ok();

    let dirty = Arc::new(AtomicBool::new(false));
    let mut joined: Option<(RoomKey, String)> = None;
    let opened = Instant::now();
    let mut last_seen = Instant::now();
    let (mut tokens, mut refill) = (BURST, Instant::now());
    loop {
        let message = match ws.read() {
            Ok(Message::Text(text)) => Some(text),
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                last_seen = Instant::now();
                None
            }
            Ok(Message::Frame(_)) => None,
            Ok(_) => break, // Close or binary
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
            Err(_) => break,
        };
        if let Some(text) = message {
            last_seen = Instant::now();
            tokens = (tokens + refill.elapsed().as_secs_f64() * RATE).min(BURST);
            refill = Instant::now();
            if tokens < 1.0 {
                let _ = ws.send(Message::Text(r#"{"error":"too many messages"}"#.into()));
                break;
            }
            tokens -= 1.0;
            let result = match &joined {
                None => serde_json::from_str::<Hello>(&text)
                    .map_err(|_| "invalid hello")
                    .and_then(|hello| join(state, config, ip, &hello, &dirty).map(|key| joined = Some((key, hello.client)))),
                Some((key, client)) => serde_json::from_str::<Vec<Operation>>(&text)
                    .map_err(|_| "invalid message")
                    .and_then(|ops| apply(state, key, client, ops)),
            };
            if let Err(error) = result {
                let _ = ws.send(Message::Text(serde_json::json!({ "error": error }).to_string()));
                break;
            }
        }
        if joined.is_none() && opened.elapsed() > HELLO_WAIT || last_seen.elapsed() > IDLE {
            break;
        }
        if let Some((key, client)) = joined.as_ref().filter(|_| dirty.swap(false, Ordering::AcqRel)) {
            let snapshot = {
                let s = state.lock().unwrap();
                s.rooms.get(key).map(|e| e.room.snapshot_for(client, now_ms()))
            };
            let Some(snapshot) = snapshot else { break };
            if ws.send(Message::Text(serde_json::to_string(&snapshot).unwrap())).is_err() {
                break;
            }
        }
    }
    if let Some((key, _)) = joined {
        let mut s = state.lock().unwrap();
        if let Some(entry) = s.rooms.get_mut(&key) {
            entry.members.retain(|m| !Arc::ptr_eq(&m.dirty, &dirty));
            entry.seen = Instant::now();
        }
    }
    // Close handshake: read until the client answers, so unread data does not
    // reset the connection before the client sees an error message.
    let _ = ws.close(None);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match ws.read() {
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
}

fn same_key(supplied: &str, expected: &str) -> bool {
    supplied.len() == expected.len()
        && supplied.bytes().zip(expected.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

fn join(state: &Mutex<State>, config: &Config, ip: IpAddr, hello: &Hello, dirty: &Arc<AtomicBool>) -> Result<RoomKey, &'static str> {
    if !config.access_key.is_empty() && !same_key(&hello.key, &config.access_key) {
        return Err("invalid relay key");
    }
    let empty = Exchange { client: hello.client.clone(), roster: hello.roster.clone(), operations: Vec::new() };
    if !valid_code(&hello.room) || !empty.valid() {
        return Err("invalid hello");
    }
    let key = (hello.room.to_ascii_lowercase(), hello.roster.clone());
    let mut s = state.lock().unwrap();
    s.rooms.retain(|_, e| !e.members.is_empty() || e.seen.elapsed() < ROOM_IDLE);
    if !s.rooms.contains_key(&key) {
        if s.rooms.len() >= MAX_ROOMS {
            return Err("relay full");
        }
        let created = s.created.entry(ip).or_default();
        created.retain(|t| t.elapsed() < Duration::from_secs(3600));
        if created.len() >= NEW_ROOMS_PER_IP_PER_HOUR {
            return Err("too many new rooms");
        }
        created.push(Instant::now());
        s.created.retain(|_, list| !list.is_empty());
        s.rooms.insert(key.clone(), Entry { room: Room::default(), seen: Instant::now(), members: Vec::new() });
    }
    let entry = s.rooms.get_mut(&key).expect("inserted above");
    if entry.members.len() >= 32 && !entry.members.iter().any(|m| m.client == hello.client) {
        return Err("room is full");
    }
    // Free the slots of sessions that left (each plugin start is a new session).
    let live: Vec<String> = entry.members.iter().map(|m| m.client.clone()).collect();
    entry.room.retain_clients(|c| c == hello.client || live.iter().any(|l| l == c));
    entry.room.exchange(&empty, now_ms())?;
    entry.members.push(Member { client: hello.client.clone(), dirty: dirty.clone() });
    entry.seen = Instant::now();
    dirty.store(true, Ordering::Release);
    Ok(key)
}

fn apply(state: &Mutex<State>, key: &RoomKey, client: &str, operations: Vec<Operation>) -> Result<(), &'static str> {
    let mut s = state.lock().unwrap();
    let entry = s.rooms.get_mut(key).ok_or("room expired")?;
    let exchange = Exchange { client: client.into(), roster: key.1.clone(), operations };
    entry.room.exchange(&exchange, now_ms())?;
    entry.seen = Instant::now();
    for member in &entry.members {
        member.dirty.store(true, Ordering::Release);
    }
    Ok(())
}
