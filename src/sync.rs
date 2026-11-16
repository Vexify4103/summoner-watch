//! All relay I/O stays off the Stream Deck event/render thread.
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use summoner_watch::shared::{Command, Exchange, Operation, Snapshot};

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
            && self.code.len() == 32
            && self.access_key.len() == 32
            && self.access_key.bytes().all(|c| c.is_ascii_hexdigit())
            && self.code.bytes().all(|c| c.is_ascii_hexdigit())
            && !self.roster.is_empty()
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
    /// Only allow edits after a recent successful exchange.
    pub connected_at: Option<Instant>,
}

impl Client {
    pub fn spawn() -> Self {
        let (tx, jobs) = mpsc::sync_channel(64);
        let (events, rx) = mpsc::sync_channel(8);
        std::thread::spawn(move || worker(jobs, events));
        Self {
            tx,
            rx,
            generation: 0,
            config: Config::default(),
            connected_at: None,
        }
    }
    pub fn configure(&mut self, config: Config) {
        if config == self.config {
            return;
        }
        let next = self.generation + 1;
        if self
            .tx
            .try_send(Job::Configure(next, config.clone()))
            .is_ok()
        {
            self.generation = next;
            self.config = config;
            self.connected_at = None;
        }
    }
    pub fn submit(&self, command: Command) -> bool {
        self.connected_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
            && self
                .tx
                .try_send(Job::Command(self.generation, command, Instant::now()))
                .is_ok()
    }
}

fn worker(jobs: Receiver<Job>, events: SyncSender<Event>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(2)))
        .max_redirects(0)
        .build()
        .into();
    let client = format!("{:032x}", rand::random::<u128>());
    let mut generation = 0;
    let mut config = Config::default();
    let mut sequence = 0u64;
    let mut pending: Vec<(Operation, Instant)> = Vec::new();
    let mut status = "";
    loop {
        let job = match jobs.recv_timeout(Duration::from_millis(250)) {
            Ok(job) => Some(job),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        for job in job.into_iter().chain(jobs.try_iter()) {
            match job {
                Job::Configure(next, new) => {
                    generation = next;
                    config = new;
                    pending.clear();
                    status = "";
                }
                Job::Command(version, command, at)
                    if version == generation && pending.len() < 32 =>
                {
                    sequence += 1;
                    pending.push((Operation { sequence, command }, at));
                }
                Job::Command(..) => {}
            }
        }
        if !config.valid() {
            continue;
        }
        // Never replay old clicks after an outage. An ambiguous request may have
        // committed; a subsequent snapshot resolves it without repeating it.
        pending.retain(|(_, at)| at.elapsed() < Duration::from_secs(5));
        let operations = pending
            .iter()
            .filter_map(|(op, at)| {
                let mut op = op.clone();
                if let Command::Start { remaining_ms, .. } = &mut op.command {
                    *remaining_ms = remaining_ms.saturating_sub(at.elapsed().as_millis() as u64);
                    if *remaining_ms == 0 {
                        return None;
                    }
                }
                Some(op)
            })
            .collect();
        let request = Exchange {
            client: client.clone(),
            roster: config.roster.clone(),
            operations,
        };
        let before = Instant::now();
        let result = agent
            .post(format!("{}/v1/exchange", config.url.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {}", config.code))
            .header("X-Relay-Key", &config.access_key)
            .send_json(&request)
            .and_then(|mut r| {
                r.body_mut()
                    .with_config()
                    .limit(32_768)
                    .read_json::<Snapshot>()
            });
        match result {
            Ok(snapshot) => {
                pending.retain(|(op, _)| op.sequence > snapshot.acknowledged);
                // Server's response clock lies near the round-trip midpoint.
                let anchor = before + before.elapsed() / 2;
                if events
                    .send(Event::Snapshot(generation, snapshot, anchor))
                    .is_err()
                {
                    return;
                }
                status = "Connected";
            }
            Err(_) => {
                // Never include the URL, body, room code, or credentials in logs.
                if status != "Disconnected" {
                    if events
                        .send(Event::Status(
                            generation,
                            "Disconnected — check relay URL, relay key and connection",
                        ))
                        .is_err()
                    {
                        return;
                    }
                    status = "Disconnected";
                }
            }
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
            code: "ab".repeat(16),
            roster: vec!["Ahri".into()],
        };
        assert!(c.valid());
        c.url = "http://timers.example.com".into();
        assert!(!c.valid());
        c.url = "http://127.0.0.1:8787".into();
        assert!(c.valid());
        c.url = "https://user:password@example.com".into();
        assert!(!c.valid());
        c.url = "https://example.com?token=x".into();
        assert!(!c.valid());
        c.url = "https://example.com".into();
        c.code = "1234".into();
        assert!(!c.valid());
    }
}
