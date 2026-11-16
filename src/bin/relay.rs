//! Raspberry Pi / Linux relay. TLS is terminated by a reverse proxy.
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use summoner_watch::shared::{valid_code, Exchange, Room};
use tiny_http::{Header, Method, Request, Response, Server};

type Rooms = HashMap<(String, Vec<String>), (Instant, Room)>;

fn respond(request: Request, status: u16, body: String) {
    let mut response = Response::from_string(body).with_status_code(status);
    response.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    let _ = request.respond(response);
}

fn handle(mut request: Request, rooms: &Mutex<Rooms>, access_key: &str) {
    // Empty access key = public relay.
    if request.method() == &Method::Get && request.url() == "/health" {
        return respond(request, 200, "{\"ok\":true}".into());
    }
    if request.method() != &Method::Post || request.url() != "/v1/exchange" {
        return respond(request, 404, "{\"error\":\"not found\"}".into());
    }
    let supplied = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Relay-Key"))
        .map(|h| h.value.as_str())
        .unwrap_or("");
    let authorized = access_key.is_empty()
        || supplied.len() == access_key.len()
        && supplied
            .bytes()
            .zip(access_key.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0;
    if !authorized {
        return respond(request, 401, "{\"error\":\"invalid relay key\"}".into());
    }
    let code = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
        .filter(|code| valid_code(code))
        .map(str::to_ascii_lowercase);
    let Some(code) = code else {
        return respond(request, 401, "{\"error\":\"invalid room code\"}".into());
    };
    // No chunked/unbounded uploads; keep memory and parsing work small.
    if request.body_length().is_none_or(|len| len > 16_384) {
        return respond(request, 413, "{\"error\":\"body too large\"}".into());
    }
    let mut body = Vec::new();
    if request
        .as_reader()
        .take(16_385)
        .read_to_end(&mut body)
        .is_err()
        || body.len() > 16_384
    {
        return respond(request, 400, "{\"error\":\"invalid body\"}".into());
    }
    let Ok(exchange) = serde_json::from_slice::<Exchange>(&body) else {
        return respond(request, 400, "{\"error\":\"invalid JSON\"}".into());
    };
    if !exchange.valid() {
        return respond(request, 400, "{\"error\":\"invalid exchange\"}".into());
    }
    let Ok(mut rooms) = rooms.lock() else {
        return respond(request, 503, "{}".into());
    };
    rooms.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(7200));
    let key = (code, exchange.roster.clone());
    if rooms.len() >= 128 && !rooms.contains_key(&key) {
        return respond(request, 503, "{\"error\":\"relay full\"}".into());
    }
    let (seen, room) = rooms
        .entry(key)
        .or_insert_with(|| (Instant::now(), Room::default()));
    *seen = Instant::now();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let result = room.exchange(&exchange, now);
    drop(rooms);
    match result {
        Ok(snapshot) => respond(request, 200, serde_json::to_string(&snapshot).unwrap()),
        Err(_) => respond(request, 409, "{\"error\":\"room full\"}".into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let access_key = std::env::var("SUMMONER_RELAY_KEY").unwrap_or_default();
    if !access_key.is_empty() && (access_key.len() != 32 || !access_key.bytes().all(|c| c.is_ascii_hexdigit())) {
        return Err("SUMMONER_RELAY_KEY must be empty (public relay) or a random 32-character hex key".into());
    }
    let bind = std::env::var("SUMMONER_RELAY_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let server = Arc::new(Server::http(&bind)?);
    let rooms = Arc::new(Mutex::new(Rooms::new()));
    println!("Summoner Watch relay listening on {bind}");
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let server = server.clone();
            let rooms = rooms.clone();
            let access_key = access_key.clone();
            std::thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(request, &rooms, &access_key);
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}
