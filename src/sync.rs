//! Team-sync client: one WebSocket to the relay, on its own thread, so relay I/O
//! never blocks the Stream Deck event/render thread.
use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use summoner_watch::shared::{valid_code, Command, Hello, Operation, Snapshot};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Room for this team in this match. The party code (optional, any text) keeps
/// out anyone who knows the player names but not the code.
pub fn room_code(match_seed: &str, party_code: &str) -> String {
    let text = format!("{match_seed}\n{}", party_code.trim());
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Config {
    pub url: String,
    pub access_key: String,
    pub code: String,
    pub roster: Vec<String>,
}

impl Config {
    pub fn valid(&self) -> bool {
        let Ok(url) = self.url.parse::<tungstenite::http::Uri>() else {
            return false;
        };
        let local = matches!(url.host(), Some("127.0.0.1" | "localhost" | "[::1]"));
        (url.scheme_str() == Some("https") || (local && url.scheme_str() == Some("http")))
            && url.authority().is_some_and(|a| !a.as_str().contains('@'))
            && url.query().is_none()
            && valid_code(&self.code)
            // The relay key is only for private relays; public relays have none.
            && (self.access_key.is_empty()
                || (self.access_key.len() == 32 && self.access_key.bytes().all(|c| c.is_ascii_hexdigit())))
            && !self.roster.is_empty()
    }

    /// `https://host/path` becomes `wss://host/path/v1/ws`.
    fn socket_url(&self) -> String {
        let base = self.url.trim_end_matches('/');
        let base = match base.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => base.replacen("http://", "ws://", 1),
        };
        format!("{base}/v1/ws")
    }
}

enum Job {
    Configure(u64, Config),
    Command(u64, Command, Instant),
}
pub enum Event {
    Snapshot(u64, Snapshot, Instant),
    Status(u64, &'static str),
}

pub struct Client {
    tx: SyncSender<Job>,
    pub rx: Receiver<Event>,
    pub generation: u64,
    config: Config,
    /// Set while the relay connection is up (on each snapshot), cleared on a
    /// disconnect. Edits are only sent while it is set.
    pub connected_at: Option<Instant>,
}

impl Client {
    pub fn spawn() -> Self {
        let (tx, jobs) = mpsc::sync_channel(64);
        let (events, rx) = mpsc::sync_channel(8);
        std::thread::spawn(move || worker(jobs, events));
        Self { tx, rx, generation: 0, config: Config::default(), connected_at: None }
    }
    pub fn configure(&mut self, config: Config) {
        if config == self.config {
            return;
        }
        let next = self.generation + 1;
        if self.tx.try_send(Job::Configure(next, config.clone())).is_ok() {
            self.generation = next;
            self.config = config;
            self.connected_at = None;
        }
    }
    pub fn submit(&self, command: Command) -> bool {
        self.connected_at.is_some() && self.tx.try_send(Job::Command(self.generation, command, Instant::now())).is_ok()
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

const PING_EVERY: Duration = Duration::from_secs(10);
/// No message (snapshot or pong) for this long = connection lost.
const SILENCE: Duration = Duration::from_secs(25);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
const DISCONNECTED: &str = "Disconnected — timers stay local until the relay is back";

fn connect(config: &Config, client: &str) -> Option<Socket> {
    let url = config.socket_url();
    let uri: tungstenite::http::Uri = url.parse().ok()?;
    let tls = uri.scheme_str() == Some("wss");
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
    stream.set_nodelay(true).ok()?;
    let ws_config = WebSocketConfig { max_message_size: Some(64 * 1024), ..Default::default() };
    // Certificates are verified against the built-in web PKI roots.
    let (mut ws, _) = tungstenite::client_tls_with_config(url.as_str(), stream, Some(ws_config), None).ok()?;
    let hello = Hello {
        room: config.code.clone(),
        client: client.into(),
        roster: config.roster.clone(),
        key: config.access_key.clone(),
    };
    ws.send(Message::Text(serde_json::to_string(&hello).ok()?)).ok()?;
    // Short reads from now on, so the worker can also send and ping.
    let tcp = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => return None,
    };
    tcp.set_read_timeout(Some(Duration::from_millis(100))).ok()?;
    Some(ws)
}

fn worker(jobs: Receiver<Job>, events: SyncSender<Event>) {
    let client = format!("{:032x}", rand::random::<u128>());
    let mut generation = 0;
    let mut config = Config::default();
    let mut sequence = 0u64;
    let mut pending: Vec<(Operation, Instant)> = Vec::new();
    let mut ws: Option<Socket> = None;
    let (mut retry_at, mut backoff) = (Instant::now(), Duration::from_secs(1));
    // Highest sequence sent on the current connection; unacknowledged ones are
    // sent again after a reconnect (the relay applies each sequence once).
    let mut sent = 0u64;
    let (mut last_rx, mut last_ping, mut ping_at) = (Instant::now(), Instant::now(), None::<Instant>);
    let mut rtt = Duration::ZERO;
    let mut told_disconnected = false;
    loop {
        // While connected the socket read below paces the loop.
        let wait = if ws.is_some() { Duration::ZERO } else { Duration::from_millis(100) };
        let first = if wait.is_zero() {
            jobs.try_recv().map_err(|e| matches!(e, mpsc::TryRecvError::Disconnected))
        } else {
            jobs.recv_timeout(wait).map_err(|e| matches!(e, mpsc::RecvTimeoutError::Disconnected))
        };
        if matches!(first, Err(true)) {
            return;
        }
        for job in first.ok().into_iter().chain(jobs.try_iter()) {
            match job {
                Job::Configure(next, new) => {
                    generation = next;
                    config = new;
                    pending.clear();
                    if let Some(mut old) = ws.take() {
                        let _ = old.close(None);
                        let _ = old.flush();
                    }
                    (retry_at, backoff, told_disconnected) = (Instant::now(), Duration::from_secs(1), false);
                }
                // Far above what a person can press within one round trip; the
                // relay acknowledges and removes them within milliseconds.
                Job::Command(version, command, at) if version == generation && pending.len() < 256 => {
                    sequence += 1;
                    pending.push((Operation { sequence, command }, at));
                }
                Job::Command(..) => {}
            }
        }
        if !config.valid() {
            continue;
        }
        // Never replay old clicks after a long outage; the local app already
        // kept them as local timers and merges them on reconnect.
        pending.retain(|(_, at)| at.elapsed() < Duration::from_secs(5));

        let Some(socket) = ws.as_mut() else {
            if Instant::now() < retry_at {
                continue;
            }
            match connect(&config, &client) {
                Some(socket) => {
                    ws = Some(socket);
                    (sent, last_rx, last_ping, ping_at) = (0, Instant::now(), Instant::now(), None);
                    backoff = Duration::from_secs(1);
                }
                None => {
                    if !told_disconnected && events.send(Event::Status(generation, DISCONNECTED)).is_err() {
                        return;
                    }
                    told_disconnected = true;
                    retry_at = Instant::now() + backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
            continue;
        };

        let mut lost = false;
        let unsent: Vec<Operation> = pending
            .iter()
            .filter(|(op, _)| op.sequence > sent)
            .filter_map(|(op, at)| {
                let mut op = op.clone();
                // A start waited in the queue: send the time actually left.
                if let Command::Start { remaining_ms, .. } | Command::Seed { remaining_ms, .. } = &mut op.command {
                    *remaining_ms = remaining_ms.saturating_sub(at.elapsed().as_millis() as u64);
                    if *remaining_ms == 0 {
                        return None;
                    }
                }
                Some(op)
            })
            // The relay takes at most 32 per message; the rest go next round.
            .take(32)
            .collect();
        if let Some(last) = unsent.last() {
            sent = last.sequence;
            lost |= socket.send(Message::Text(serde_json::to_string(&unsent).unwrap_or_default())).is_err();
        }
        if last_ping.elapsed() >= PING_EVERY {
            last_ping = Instant::now();
            ping_at = Some(last_ping);
            lost |= socket.send(Message::Ping(Vec::new())).is_err();
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                last_rx = Instant::now();
                match serde_json::from_str::<Snapshot>(&text) {
                    Ok(snapshot) => {
                        pending.retain(|(op, _)| op.sequence > snapshot.acknowledged);
                        // The relay sent it about half a round trip ago.
                        let anchor = Instant::now().checked_sub(rtt / 2).unwrap_or_else(Instant::now);
                        if events.send(Event::Snapshot(generation, snapshot, anchor)).is_err() {
                            return;
                        }
                        told_disconnected = false;
                    }
                    Err(_) => {
                        // {"error": ...}: wrong relay key, relay full, rate limit.
                        // Never log the message, URL, room code or credentials.
                        let status = if text.contains("invalid relay key") {
                            "Relay key rejected"
                        } else {
                            "Relay refused the connection, retrying"
                        };
                        if events.send(Event::Status(generation, status)).is_err() {
                            return;
                        }
                        told_disconnected = true;
                        ws = None;
                        retry_at = Instant::now() + MAX_BACKOFF;
                        continue;
                    }
                }
            }
            Ok(Message::Pong(_)) => {
                last_rx = Instant::now();
                if let Some(at) = ping_at.take() {
                    rtt = at.elapsed();
                }
            }
            Ok(Message::Close(_)) => lost = true,
            Ok(_) => last_rx = Instant::now(),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => lost = true,
        }
        if lost || last_rx.elapsed() > SILENCE {
            ws = None;
            if events.send(Event::Status(generation, DISCONNECTED)).is_err() {
                return;
            }
            told_disconnected = true;
            retry_at = Instant::now() + backoff;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_https_or_loopback_and_strong_room_codes() {
        let mut c = Config {
            url: "https://timers.example.com".into(),
            access_key: "cd".repeat(16),
            code: room_code(&"ab".repeat(32), ""),
            roster: vec!["Ahri".into()],
        };
        assert!(c.valid());
        assert_eq!(c.socket_url(), "wss://timers.example.com/v1/ws");
        c.access_key.clear();
        assert!(c.valid(), "relay key is optional");
        c.access_key = "short".into();
        assert!(!c.valid());
        c.access_key.clear();
        c.url = "http://timers.example.com".into();
        assert!(!c.valid());
        c.url = "http://127.0.0.1:8787/".into();
        assert!(c.valid());
        assert_eq!(c.socket_url(), "ws://127.0.0.1:8787/v1/ws");
        c.url = "https://user:password@example.com".into();
        assert!(!c.valid());
        c.url = "https://example.com?token=x".into();
        assert!(!c.valid());
        c.url = "https://example.com".into();
        c.code = "1234".into();
        assert!(!c.valid());
    }

    #[test]
    fn party_code_changes_the_room() {
        let seed = "ab".repeat(32);
        assert_eq!(room_code(&seed, " duo "), room_code(&seed, "duo"));
        assert_ne!(room_code(&seed, ""), room_code(&seed, "duo"));
        assert!(valid_code(&room_code(&seed, "")));
    }
}
