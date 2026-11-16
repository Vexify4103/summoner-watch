//! Small, platform-independent protocol and authoritative room state.
//! Deadlines use the relay's clock; clients translate time remaining to Instant.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const HISTORY_LIMIT: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    pub champion: String,
    pub spell: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedTimer {
    pub key: Key,
    pub end_ms: u64,
    pub total_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Command {
    Start {
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
    Undo,
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
    /// Sorted champion roster. Never includes Riot IDs or API credentials.
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
    key: Key,
    previous: Option<SharedTimer>,
}

#[derive(Default)]
pub struct Room {
    timers: BTreeMap<Key, SharedTimer>,
    history: VecDeque<Started>,
    clients: BTreeMap<String, u64>,
    revision: u64,
}

impl Exchange {
    pub fn valid(&self) -> bool {
        let short = |s: &str| !s.is_empty() && s.len() <= 100 && !s.chars().any(char::is_control);
        let key_ok = |key: &Key| self.roster.contains(&key.champion) && short(&key.spell);
        self.client.len() == 32
            && self.client.bytes().all(|c| c.is_ascii_hexdigit())
            && !self.roster.is_empty()
            && self.roster.len() <= 5
            && self.roster.iter().all(|s| short(s))
            && self.roster.windows(2).all(|pair| pair[0] < pair[1])
            && self.operations.len() <= 32
            && self
                .operations
                .windows(2)
                .all(|p| p[0].sequence < p[1].sequence)
            && self.operations.iter().all(|op| {
                op.sequence > 0
                    && match &op.command {
                        Command::Start {
                            key,
                            remaining_ms,
                            total_ms,
                        } => {
                            key_ok(key)
                                && *remaining_ms > 0
                                && *remaining_ms <= *total_ms
                                && *total_ms <= 1_200_000
                        }
                        Command::Adjust { key, delta_ms } => {
                            key_ok(key) && delta_ms.unsigned_abs() <= 1_200_000
                        }
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
            // Retried HTTP requests must never repeat an undo or adjustment.
            if op.sequence <= acknowledged {
                continue;
            }
            self.apply(&op.command, now_ms);
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

    fn apply(&mut self, command: &Command, now: u64) {
        match command {
            Command::Start {
                key,
                remaining_ms,
                total_ms,
            } => {
                if self.timers.len() >= 15 && !self.timers.contains_key(key) {
                    return;
                }
                let previous = self.timers.insert(
                    key.clone(),
                    SharedTimer {
                        key: key.clone(),
                        end_ms: now.saturating_add(*remaining_ms),
                        total_ms: *total_ms,
                    },
                );
                self.history.push_back(Started {
                    key: key.clone(),
                    previous,
                });
                if self.history.len() > HISTORY_LIMIT {
                    self.history.pop_front();
                }
            }
            Command::Adjust { key, delta_ms } => {
                if let Some(t) = self.timers.get_mut(key).filter(|t| t.end_ms > now) {
                    t.end_ms = t
                        .end_ms
                        .saturating_add_signed(*delta_ms)
                        .clamp(now + 1000, now + 1_200_000);
                    t.total_ms = t.total_ms.max(t.end_ms - now);
                }
            }
            Command::Cancel { key } => {
                self.timers.remove(key);
                // A cancelled timer is no longer a candidate for undo-start.
                self.history.retain(|entry| &entry.key != key);
            }
            Command::Undo => {
                while let Some(entry) = self.history.pop_back() {
                    if !self.timers.get(&entry.key).is_some_and(|t| t.end_ms > now) {
                        continue;
                    }
                    self.timers.remove(&entry.key);
                    if let Some(previous) = entry.previous.filter(|t| t.end_ms > now) {
                        self.timers.insert(entry.key, previous);
                    }
                    break;
                }
            }
            Command::Reset => {
                self.timers.clear();
                self.history.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(champion: &str) -> Key {
        Key {
            champion: champion.into(),
            spell: "SummonerFlash".into(),
        }
    }
    fn request(client: char, ops: Vec<(u64, Command)>) -> Exchange {
        Exchange {
            client: client.to_string().repeat(32),
            roster: vec!["Ahri".into(), "Lux".into()],
            operations: ops
                .into_iter()
                .map(|(sequence, command)| Operation { sequence, command })
                .collect(),
        }
    }
    fn start(champ: &str) -> Command {
        Command::Start {
            key: key(champ),
            remaining_ms: 300_000,
            total_ms: 300_000,
        }
    }
    #[test]
    fn teammates_share_order_and_retry_is_idempotent() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri"))]), 1000)
            .unwrap();
        room.exchange(&request('b', vec![(1, start("Lux"))]), 2000)
            .unwrap();
        let undo = request('a', vec![(2, Command::Undo)]);
        assert_eq!(
            room.exchange(&undo, 3000).unwrap().timers[0].key,
            key("Ahri")
        );
        assert_eq!(room.exchange(&undo, 4000).unwrap().timers.len(), 1);
        assert!(room
            .exchange(&request('b', vec![(2, Command::Undo)]), 5000)
            .unwrap()
            .timers
            .is_empty());
    }
    #[test]
    fn undo_restart_restores_original_deadline() {
        let mut room = Room::default();
        room.exchange(&request('a', vec![(1, start("Ahri"))]), 1000)
            .unwrap();
        room.exchange(&request('a', vec![(2, start("Ahri"))]), 5000)
            .unwrap();
        assert_eq!(
            room.exchange(&request('a', vec![(3, Command::Undo)]), 8000)
                .unwrap()
                .timers[0]
                .end_ms,
            301_000
        );
    }
    #[test]
    fn expired_and_cancelled_starts_are_skipped() {
        let mut room = Room::default();
        room.exchange(
            &request(
                'a',
                vec![
                    (1, start("Ahri")),
                    (2, start("Lux")),
                    (3, Command::Cancel { key: key("Lux") }),
                    (4, Command::Undo),
                ],
            ),
            1,
        )
        .unwrap();
        assert!(room.timers.is_empty());
        room.exchange(
            &request(
                'a',
                vec![(5, start("Ahri")), (6, Command::Reset), (7, Command::Undo)],
            ),
            2,
        )
        .unwrap();
        assert!(room.timers.is_empty());
    }
    #[test]
    fn malformed_operations_rejected_atomically() {
        let mut room = Room::default();
        let invalid = request('a', vec![(1, start("Ahri")), (2, start("Zed"))]);
        assert!(room.exchange(&invalid, 0).is_err());
        assert!(room.timers.is_empty());
    }
}
