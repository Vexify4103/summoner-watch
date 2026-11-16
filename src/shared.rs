//! Small, platform-independent protocol and authoritative room state.
//! Deadlines use the relay's clock; clients translate time remaining to Instant.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const HISTORY_LIMIT: usize = 256;
/// Longest timer the relay accepts (20 minutes).
const MAX_MS: u64 = 1_200_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    /// Language-independent champion id, e.g. "MonkeyKing".
    pub champion: String,
    pub spell: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedTimer {
    pub key: Key,
    pub end_ms: u64,
    pub total_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Command {
    /// A key press. Restarting a running timer replaces it; Undo brings it back.
    Start {
        key: Key,
        remaining_ms: u64,
        total_ms: u64,
    },
    /// A timer started while offline. Never replaces a room timer and is not
    /// part of anyone's undo history.
    Seed {
        key: Key,
        remaining_ms: u64,
        total_ms: u64,
    },
    Adjust {
        key: Key,
        delta_ms: i64,
    },
    Cancel {
        key: Key,
    },
    /// Undoes the sender's own newest start. Teammates' starts are never undone.
    Undo,
    /// Undoes all of the sender's own starts.
    Reset,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub sequence: u64,
    pub command: Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Exchange {
    pub client: String,
    /// Sorted champion ids of the enemy team. Never includes Riot IDs or API credentials.
    pub roster: Vec<String>,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub now_ms: u64,
    pub revision: u64,
    pub acknowledged: u64,
    pub timers: Vec<SharedTimer>,
}

struct Started {
    client: String,
    timer: SharedTimer,
    /// The countdown this start replaced (a restart), restored by Undo.
    previous: Option<SharedTimer>,
}

#[derive(Default)]
pub struct Room {
    timers: BTreeMap<Key, SharedTimer>,
    history: VecDeque<Started>,
    clients: BTreeMap<String, u64>,
    revision: u64,
}

pub fn valid_code(code: &str) -> bool {
    code.len() == 64 && code.bytes().all(|c| c.is_ascii_hexdigit())
}

impl Exchange {
    pub fn valid(&self) -> bool {
        let short = |s: &str| !s.is_empty() && s.len() <= 100 && !s.chars().any(char::is_control);
        let key_ok = |key: &Key| self.roster.contains(&key.champion) && short(&key.spell);
        let timer_ok = |key: &Key, remaining: u64, total: u64| {
            key_ok(key) && remaining > 0 && remaining <= total && total <= MAX_MS
        };
        self.client.len() == 32
            && self.client.bytes().all(|c| c.is_ascii_hexdigit())
            && !self.roster.is_empty()
            && self.roster.len() <= 5
            && self.roster.iter().all(|s| short(s))
            && self.roster.windows(2).all(|pair| pair[0] < pair[1])
            && self.operations.len() <= 32
            && self.operations.windows(2).all(|p| p[0].sequence < p[1].sequence)
            && self.operations.iter().all(|op| {
                op.sequence > 0
                    && match &op.command {
                        Command::Start { key, remaining_ms, total_ms }
                        | Command::Seed { key, remaining_ms, total_ms } => timer_ok(key, *remaining_ms, *total_ms),
                        Command::Adjust { key, delta_ms } => key_ok(key) && delta_ms.unsigned_abs() <= MAX_MS,
                        Command::Cancel { key } => key_ok(key),
                        Command::Undo | Command::Reset => true,
                    }
            })
    }
}

impl Room {
    pub fn exchange(&mut self, request: &Exchange, now_ms: u64) -> Result<Snapshot, &'static str> {
        if !request.valid() {
            return Err("invalid request");
        }
        if self.clients.len() >= 32 && !self.clients.contains_key(&request.client) {
            return Err("room is full");
        }
        let mut acknowledged = *self.clients.get(&request.client).unwrap_or(&0);
        for op in &request.operations {
            // Retried requests must never repeat an undo or adjustment.
            if op.sequence <= acknowledged {
                continue;
            }
            self.apply(&op.command, &request.client, now_ms);
            acknowledged = op.sequence;
            self.revision += 1;
        }
        self.clients.insert(request.client.clone(), acknowledged);
        self.timers.retain(|_, t| t.end_ms > now_ms);
        Ok(Snapshot {
            now_ms,
            revision: self.revision,
            acknowledged,
            timers: self.timers.values().cloned().collect(),
        })
    }

    fn apply(&mut self, command: &Command, client: &str, now: u64) {
        let make = |key: &Key, remaining: u64, total: u64| SharedTimer {
            key: key.clone(),
            end_ms: now.saturating_add(remaining),
            total_ms: total,
        };
        match command {
            Command::Start { key, remaining_ms, total_ms } => {
                if self.timers.len() >= 15 && !self.timers.contains_key(key) {
                    return;
                }
                let timer = make(key, *remaining_ms, *total_ms);
                let previous = self.timers.insert(key.clone(), timer.clone()).filter(|t| t.end_ms > now);
                self.history.push_back(Started { client: client.into(), timer, previous });
                if self.history.len() > HISTORY_LIMIT {
                    self.history.pop_front();
                }
            }
            Command::Seed { key, remaining_ms, total_ms } => {
                let running = self.timers.get(key).is_some_and(|t| t.end_ms > now);
                if !running && self.timers.len() < 15 {
                    self.timers.insert(key.clone(), make(key, *remaining_ms, *total_ms));
                }
            }
            Command::Adjust { key, delta_ms } => {
                if let Some(t) = self.timers.get_mut(key).filter(|t| t.end_ms > now) {
                    t.end_ms = t.end_ms.saturating_add_signed(*delta_ms).clamp(now + 1000, now + MAX_MS);
                    t.total_ms = t.total_ms.max(t.end_ms - now);
                    let t = t.clone();
                    // Keep the newest start in step, so Undo knows it is still running.
                    if let Some(entry) = self.history.iter_mut().rev().find(|e| &e.timer.key == key) {
                        entry.timer = t;
                    }
                }
            }
            Command::Cancel { key } => {
                self.timers.remove(key);
                // A cancelled timer is no longer a candidate for undo.
                self.history.retain(|entry| &entry.timer.key != key);
            }
            Command::Undo => {
                self.undo(client, now);
            }
            Command::Reset => while self.undo(client, now) {},
        }
    }

    /// Undoes the client's newest start that is still running. Returns false when
    /// there is none.
    fn undo(&mut self, client: &str, now: u64) -> bool {
        let Some(i) = self.history.iter().rposition(|e| e.client == client && e.timer.end_ms > now) else {
            return false;
        };
        let entry = self.history.remove(i).expect("index from rposition");
        let key = entry.timer.key.clone();
        match self.history.iter_mut().skip(i).find(|e| e.timer.key == key) {
            // A teammate restarted it later: their countdown stays, but their
            // own undo must no longer bring back this start.
            Some(newer) => newer.previous = entry.previous,
            None => match entry.previous.filter(|t| t.end_ms > now) {
                Some(previous) => {
                    self.timers.insert(key, previous);
                }
                None => {
                    self.timers.remove(&key);
                }
            },
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(champion: &str) -> Key {
        Key { champion: champion.into(), spell: "SummonerFlash".into() }
    }
    fn request(client: char, ops: Vec<(u64, Command)>) -> Exchange {
        Exchange {
            client: client.to_string().repeat(32),
            roster: vec!["Ahri".into(), "Lux".into()],
            operations: ops.into_iter().map(|(sequence, command)| Operation { sequence, command }).collect(),
        }
    }
    fn start(champ: &str) -> Command {
        Command::Start { key: key(champ), remaining_ms: 300_000, total_ms: 300_000 }
    }
    fn seed(champ: &str, remaining_ms: u64) -> Command {
        Command::Seed { key: key(champ), remaining_ms, total_ms: 300_000 }
    }
    fn champs(s: &Snapshot) -> Vec<&str> {
        s.timers.iter().map(|t| t.key.champion.as_str()).collect()
    }

    #[test]
    fn undo_only_reverts_own_starts_and_retry_is_idempotent() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri"))]), 1000).unwrap();
        room.exchange(&request('b', vec![(1, start("Lux"))]), 2000).unwrap();
        let undo = request('a', vec![(2, Command::Undo)]);
        assert_eq!(champs(&room.exchange(&undo, 3000).unwrap()), ["Lux"]);
        assert_eq!(champs(&room.exchange(&undo, 4000).unwrap()), ["Lux"]); // lost-response retry
        // a has nothing left to undo; b's timer stays.
        assert_eq!(champs(&room.exchange(&request('a', vec![(3, Command::Undo)]), 5000).unwrap()), ["Lux"]);
        assert!(room.exchange(&request('b', vec![(2, Command::Undo)]), 6000).unwrap().timers.is_empty());
    }

    #[test]
    fn undo_restart_restores_original_deadline() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri"))]), 1000).unwrap();
        room.exchange(&request('a', vec![(2, start("Ahri"))]), 5000).unwrap();
        let s = room.exchange(&request('a', vec![(3, Command::Undo)]), 8000).unwrap();
        assert_eq!(s.timers[0].end_ms, 301_000);
    }

    #[test]
    fn undoing_a_start_a_teammate_restarted_keeps_their_countdown() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri"))]), 1000).unwrap();
        room.exchange(&request('b', vec![(1, start("Ahri"))]), 5000).unwrap();
        let s = room.exchange(&request('a', vec![(2, Command::Undo)]), 6000).unwrap();
        assert_eq!(s.timers[0].end_ms, 305_000, "b's restart stays");
        // b's undo must not bring back a's undone start.
        assert!(room.exchange(&request('b', vec![(2, Command::Undo)]), 7000).unwrap().timers.is_empty());
    }

    #[test]
    fn reset_clears_only_own_starts() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri")), (2, start("Lux"))]), 1000).unwrap();
        room.exchange(&request('b', vec![(1, start("Lux"))]), 2000).unwrap();
        let s = room.exchange(&request('a', vec![(3, Command::Reset)]), 3000).unwrap();
        assert_eq!(champs(&s), ["Lux"]);
        assert_eq!(s.timers[0].end_ms, 302_000, "b's start stays");
    }

    #[test]
    fn seed_never_replaces_and_is_not_undoable() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri"))]), 1000).unwrap();
        let s = room.exchange(&request('b', vec![(1, seed("Ahri", 10_000)), (2, seed("Lux", 10_000))]), 2000).unwrap();
        assert_eq!(s.timers[0].end_ms, 301_000, "seed must not replace a room timer");
        assert_eq!(s.timers[1].end_ms, 12_000);
        let s = room.exchange(&request('b', vec![(3, Command::Undo)]), 3000).unwrap();
        assert_eq!(s.timers.len(), 2);
    }

    #[test]
    fn expired_and_cancelled_starts_are_skipped() {
        let mut room = Room::default();
        room.exchange(
            &request('a', vec![(1, start("Ahri")), (2, start("Lux")), (3, Command::Cancel { key: key("Lux") }), (4, Command::Undo)]),
            1,
        )
        .unwrap();
        assert!(room.timers.is_empty());
        room.exchange(&request('a', vec![(5, start("Ahri"))]), 2).unwrap();
        // Long after it expired, Undo has nothing to do.
        assert!(room.exchange(&request('a', vec![(6, Command::Undo)]), 400_000).unwrap().timers.is_empty());
    }

    #[test]
    fn malformed_operations_rejected_atomically() {
        let mut room = Room::default();
        let invalid = request('a', vec![(1, start("Ahri")), (2, start("Zed"))]);
        assert!(room.exchange(&invalid, 0).is_err());
        assert!(room.timers.is_empty());
        assert!(valid_code(&"ab".repeat(32)));
        assert!(!valid_code(&"ab".repeat(16)));
    }
}
