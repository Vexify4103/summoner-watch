// Console-subsystem exe on purpose: Stream Deck starts plugins hidden, and the
// windows_subsystem flag made Kaspersky lock the exe as suspicious.

mod game;
mod render;
mod settings;
mod sound;
mod sync;

use game::{Boots, Enemy, Update, QUEST_TP};
use render::SpellState;
use serde_json::{json, Value};
use settings::Settings;
use std::collections::{HashMap, VecDeque};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::Message;
use summoner_watch::shared::{Command, Key, Snapshot, HISTORY_LIMIT};

const LONG_PRESS: Duration = Duration::from_millis(450);
/// Second tap within this time = the other spell (enemy keys).
const DOUBLE_TAP: Duration = Duration::from_millis(300);
/// How long a spell that just came back stays highlighted.
const FRESH_FOR: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Enemy,
    Dial,
    Insight,
    Reset,
}

struct Instance {
    kind: Kind,
    /// Enemy index (0-4) for keys, timer rank (0-3) for dials.
    slot: usize,
    /// Cheap summary of what is shown; the image is only rebuilt when it changes.
    shown: String,
    down: Option<Instant>,
    /// Time of a tap waiting to see if a second tap follows (enemy keys).
    pending_tap: Option<Instant>,
    /// Timer action placed on a dial (true) or on a normal key (false).
    encoder: bool,
    /// Key columns of the device this action is on.
    cols: usize,
}

#[derive(Clone, Copy)]
struct Timer {
    end: Instant,
    total: f64,
}

struct Started {
    slot: usize,
    idx: usize,
    at: Instant,
    previous: Option<Timer>,
}

struct App {
    settings: Arc<Mutex<Settings>>,
    league_running: Arc<AtomicBool>,
    instances: HashMap<String, Instance>,
    /// Device id -> key columns, from the registration info and deviceDidConnect.
    device_cols: HashMap<String, usize>,
    enemies: Vec<Enemy>,
    /// Per enemy: [spell 0, spell 1, quest Teleport].
    timers: [[Option<Timer>; 3]; 5],
    fresh: [[Option<Instant>; 3]; 5],
    manual_insight: [Option<bool>; 5],
    edit_mode: bool,
    history: VecDeque<Started>,
    sync: sync::Client,
    sync_status: String,
    /// Team-sync room seed of the current match (None when spectating).
    match_seed: Option<String>,
    /// True once the relay sent a snapshot on this connection: changes then go
    /// through the relay. Otherwise they stay local and are merged on reconnect.
    sync_live: bool,
    inspectors: Vec<String>,
    /// Timer order kept while a dial or Timer key is in use, so a timer that
    /// passes its neighbour does not jump to another dial mid-turn:
    /// (keep until, (enemy, spell) in display order).
    frozen: Option<(Instant, Vec<(usize, usize)>)>,
    out: Vec<Value>,
}

/// How long the timer order stays fixed after the last dial turn or Timer key press.
const ORDER_HOLD: Duration = Duration::from_millis(2000);

/// Sorts running timers by time left. With a `frozen` order, timers keep
/// those positions; timers started since then follow, sorted.
fn order_timers(mut list: Vec<(usize, usize, f64, f64)>, frozen: Option<&[(usize, usize)]>) -> Vec<(usize, usize, f64, f64)> {
    list.sort_by(|a, b| a.2.total_cmp(&b.2));
    if let Some(order) = frozen {
        let pos = |t: &(usize, usize, f64, f64)| order.iter().position(|&o| o == (t.0, t.1)).unwrap_or(usize::MAX);
        list.sort_by_key(pos); // stable: new timers keep their time order at the end
    }
    list
}

/// Enemy index or timer rank for an action. `cols` = key columns of that device
/// (Stream Deck + = 4, Stream Deck XL = 8, MK.2 = 5).
fn slot_for(kind: Kind, settings: &Value, coords: &Value, cols: usize) -> usize {
    let col = coords["column"].as_u64().unwrap_or(0) as usize;
    let row = coords["row"].as_u64().unwrap_or(0) as usize;
    let explicit = settings["slot"]
        .as_str()
        .and_then(|s| s.parse::<usize>().ok())
        .or_else(|| settings["slot"].as_u64().map(|n| n as usize))
        .filter(|n| *n >= 1);
    match (kind, explicit) {
        (Kind::Enemy, Some(n)) => (n - 1).min(4),
        (Kind::Dial, Some(n)) => (n - 1).min(7),
        (Kind::Enemy, None) => (row * cols.max(1) + col) % 5,
        (Kind::Dial, None) => col.min(7),
        _ => 0,
    }
}

impl App {
    fn log(&mut self, msg: impl Into<String>) {
        self.out.push(json!({ "event": "logMessage", "payload": { "message": msg.into() } }));
    }

    fn insight(&self, slot: usize) -> bool {
        self.manual_insight[slot]
            .or_else(|| self.enemies.get(slot).and_then(|e| e.insight_api))
            .unwrap_or(false)
    }

    fn clear_timers(&mut self) {
        self.instances.values_mut().for_each(|i| i.pending_tap = None);
        self.timers = [[None; 3]; 5];
        self.fresh = [[None; 3]; 5];
        self.history.clear();
        self.frozen = None;
    }

    fn shared_key(&self, slot: usize, idx: usize) -> Key {
        let e = &self.enemies[slot];
        let id = &e.spell(idx).id;
        // Teleport's upgrade must keep the same shared timer identity.
        let spell = if idx == QUEST_TP { "QuestTeleport" } else if id.contains("Teleport") { "SummonerTeleport" } else { id };
        Key { champion: e.champ_id.clone(), spell: spell.into() }
    }

    fn refresh_sync(&mut self) {
        let s = self.settings.lock().unwrap().clone();
        let mut roster: Vec<_> = self.enemies.iter().map(|e| e.champ_id.clone()).collect();
        roster.sort();
        roster.dedup();
        let code = self.match_seed.as_deref().map(|seed| sync::room_code(seed, &s.party_code));
        let config = match code {
            Some(code) if s.sync_enabled => sync::Config { url: s.sync_url, access_key: s.sync_key, code, roster },
            _ => sync::Config::default(),
        };
        let status = if !s.sync_enabled {
            "Off"
        } else if self.enemies.is_empty() {
            "Waiting for a game"
        } else if self.match_seed.is_none() {
            "Only works while you play in the match"
        } else if !config.valid() {
            "Enter an HTTPS relay URL"
        } else {
            "Connecting"
        };
        let before = self.sync.generation;
        self.sync.configure(config);
        if before != self.sync.generation { self.sync_live = false; }
        if before != self.sync.generation || status != "Connecting" {
            self.set_sync_status(status);
        }
    }

    fn set_sync_status(&mut self, status: &str) {
        if self.sync_status == status { return; }
        self.sync_status = status.into();
        for ctx in &self.inspectors {
            self.out.push(json!({ "event": "sendToPropertyInspector", "context": ctx, "payload": { "event": "syncStatus", "status": status } }));
        }
    }

    /// Sends a change to the relay. False = not connected: apply it locally, it
    /// is merged into the room when the connection is back.
    fn share(&mut self, command: Command) -> bool {
        if !self.sync_live { return false; }
        if self.sync.submit(command) { return true; }
        self.sync_live = false;
        false
    }

    /// Shows the room's timers. With `merge` (first snapshot after connecting),
    /// local countdowns the room does not have are kept and sent to the room.
    fn apply_snapshot(&mut self, snapshot: Snapshot, anchor: Instant, merge: bool) {
        let now = Instant::now();
        let mut next = [[None; 3]; 5];
        for (slot, enemy) in self.enemies.iter().enumerate().take(5) {
            for (idx, next_timer) in next[slot].iter_mut().enumerate() {
                if idx == QUEST_TP && enemy.quest_tp.is_none() { continue; }
                let key = self.shared_key(slot, idx);
                if let Some(t) = snapshot.timers.iter().find(|t| t.key == key) {
                    let left = Duration::from_millis(t.end_ms.saturating_sub(snapshot.now_ms)).saturating_sub(now.saturating_duration_since(anchor));
                    if !left.is_zero() && left <= Duration::from_secs(1200) && t.total_ms <= 1_200_000 {
                        *next_timer = Some(Timer { end: now + left, total: t.total_ms as f64 / 1000.0 });
                        self.fresh[slot][idx] = None;
                    }
                }
            }
        }
        if merge {
            // The relay owns undo from now on.
            self.history.clear();
            for (slot, row) in next.iter_mut().enumerate().take(self.enemies.len()) {
                for (idx, cell) in row.iter_mut().enumerate() {
                    let Some(t) = self.timers[slot][idx].filter(|t| t.end > now && cell.is_none()) else { continue };
                    let remaining_ms = t.end.duration_since(now).as_millis() as u64;
                    if remaining_ms > 0 && remaining_ms <= 1_200_000 {
                        let total_ms = ((t.total * 1000.0) as u64).clamp(remaining_ms, 1_200_000);
                        *cell = Some(t);
                        self.sync.submit(Command::Seed { key: self.shared_key(slot, idx), remaining_ms, total_ms });
                    }
                }
            }
        }
        let mut ring = false;
        for (slot, pair) in self.timers.iter().enumerate() {
            for (idx, old) in pair.iter().enumerate() {
                if let Some(old) = old.filter(|_| next[slot][idx].is_none()) {
                    if old.end <= now {
                        self.fresh[slot][idx] = Some(now);
                        ring = true;
                    } else { self.fresh[slot][idx] = None; }
                }
            }
        }
        self.timers = next;
        if ring { sound::play(&self.settings.lock().unwrap()); }
    }

    fn flush_pending(&mut self) {
        let mut pending: Vec<_> = self.instances.iter_mut().filter_map(|(ctx, i)| i.pending_tap.take().map(|at| (ctx.clone(), i.slot, at))).collect();
        pending.sort_by_key(|p| p.2);
        for (ctx, slot, at) in pending { self.press_enemy(&ctx, slot, false, at); }
    }

    /// Undoes the newest timer start that is still running. Undoing a restart
    /// brings back the countdown it replaced. With team sync only your own starts.
    fn undo(&mut self) {
        self.flush_pending();
        if self.share(Command::Undo) { return; }
        while let Some(entry) = self.history.pop_back() {
            if !self.timers[entry.slot][entry.idx].is_some_and(|t| t.end > Instant::now()) { continue; }
            self.timers[entry.slot][entry.idx] = entry.previous.filter(|t| t.end > Instant::now());
            self.fresh[entry.slot][entry.idx] = None;
            self.frozen = None;
            break;
        }
    }

    /// Running timers in display order: (enemy, spell, remaining, total).
    /// Sorted by time left, except while the order is frozen (see `frozen`).
    fn running(&self) -> Vec<(usize, usize, f64, f64)> {
        let now = Instant::now();
        let mut list = Vec::new();
        for (s, pair) in self.timers.iter().enumerate() {
            for (i, t) in pair.iter().enumerate() {
                if let Some(t) = t {
                    list.push((s, i, t.end.saturating_duration_since(now).as_secs_f64(), t.total));
                }
            }
        }
        let frozen = self.frozen.as_ref().filter(|(until, _)| now < *until).map(|(_, o)| o.as_slice());
        order_timers(list, frozen)
    }

    /// Keeps the current timer order for ORDER_HOLD from now.
    fn hold_order(&mut self, order: &[(usize, usize, f64, f64)]) {
        self.frozen = Some((Instant::now() + ORDER_HOLD, order.iter().map(|t| (t.0, t.1)).collect()));
    }

    /// Display order of an enemy's two spells: [single tap, double-tap] as spell indexes.
    fn order(&self, slot: usize) -> [usize; 2] {
        let Some(e) = self.enemies.get(slot) else { return [0, 1] };
        let s = self.settings.lock().unwrap();
        let rank = |i: usize| s.spell_rank(&e.spells[i].id, &e.spells[i].name);
        if rank(1) < rank(0) { [1, 0] } else { [0, 1] }
    }

    /// Which of an enemy's spells are ignored (by spell index).
    fn hidden(&self, slot: usize) -> [bool; 2] {
        let Some(e) = self.enemies.get(slot) else { return [false; 2] };
        let s = self.settings.lock().unwrap();
        let h = |i: usize| s.is_ignored(&e.spells[i].id, &e.spells[i].name);
        [h(0), h(1)]
    }

    fn start(&mut self, ctx: &str, slot: usize, idx: usize) {
        self.start_at(ctx, slot, idx, Instant::now());
    }

    /// Starts a timer as if the key was pressed at `at`, so waiting to tell
    /// a tap from a double-tap does not cost time on the timer.
    fn start_at(&mut self, ctx: &str, slot: usize, idx: usize, at: Instant) {
        let Some(e) = self.enemies.get(slot) else {
            self.out.push(json!({ "event": "showAlert", "context": ctx }));
            return;
        };
        let s = self.settings.lock().unwrap().clone();
        let mut haste = 0.0;
        if self.insight(slot) {
            haste += s.insight_haste;
        }
        haste += match e.boots {
            Boots::Lucidity => s.lucidity_haste,
            Boots::Crimson => s.crimson_haste,
            Boots::None => 0.0,
        };
        let spell = e.spell(idx);
        let base = if idx == QUEST_TP {
            s.quest_tp_cd
        } else if spell.is_unleashed_tp() {
            game::unleashed_tp_cooldown(e.level)
        } else {
            spell.cooldown
        };
        let secs = (base * 100.0 / (100.0 + haste) - s.offset).max(1.0);
        let total_ms = (secs * 1000.0) as u64;
        let remaining_ms = total_ms.saturating_sub(at.elapsed().as_millis() as u64).max(1);
        if self.share(Command::Start { key: self.shared_key(slot, idx), remaining_ms, total_ms }) { return; }
        let timer = Timer { end: at + Duration::from_secs_f64(secs), total: secs };
        let pos = self.history.partition_point(|e| e.at <= at);
        // A single tap can resolve after a later immediate start on another
        // copy of the same enemy key. Insert it chronologically without
        // replacing that newer start, and repair the newer entry's undo state.
        let newer = self.history.iter_mut().skip(pos).find(|e| e.slot == slot && e.idx == idx);
        let (previous, replace) = match newer {
            Some(next) => (next.previous.replace(timer), false),
            None => (self.timers[slot][idx], true),
        };
        let entry = Started { slot, idx, at, previous };
        self.history.insert(pos, entry);
        if self.history.len() > HISTORY_LIMIT { self.history.pop_front(); }
        if replace {
            self.timers[slot][idx] = Some(timer);
            self.fresh[slot][idx] = None;
        }
    }

    /// Moves the Nth soonest timer by `delta` seconds (never below 1s left).
    fn adjust_rank(&mut self, rank: usize, delta: f64) {
        let order = self.running();
        self.hold_order(&order);
        if let Some(&(s, i, ..)) = order.get(rank) {
            if self.share(Command::Adjust { key: self.shared_key(s, i), delta_ms: (delta * 1000.0) as i64 }) { return; }
            if let Some(t) = self.timers[s][i].as_mut() {
                let now = Instant::now();
                let left = (t.end.saturating_duration_since(now).as_secs_f64() + delta).max(1.0);
                t.end = now + Duration::from_secs_f64(left);
                t.total = t.total.max(left);
            }
        }
    }

    fn cancel_rank(&mut self, rank: usize) {
        let order = self.running();
        self.hold_order(&order);
        if let Some(&(s, i, ..)) = order.get(rank) {
            if self.share(Command::Cancel { key: self.shared_key(s, i) }) { return; }
            self.timers[s][i] = None;
            self.history.retain(|e| e.slot != s || e.idx != i);
        }
    }

    /// Tracked spells of an enemy, most important first.
    fn visible(&self, slot: usize) -> Vec<usize> {
        let hidden = self.hidden(slot);
        self.order(slot).into_iter().filter(|i| !hidden[*i]).collect()
    }

    /// Single tap = more important spell, double-tap (`second`) = the other one.
    fn press_enemy(&mut self, ctx: &str, slot: usize, second: bool, at: Instant) {
        let visible = self.visible(slot);
        match visible.len() {
            // Only one tracked spell (e.g. jungler with Smite): any press starts it.
            1 => self.start_at(ctx, slot, visible[0], at),
            2 => self.start_at(ctx, slot, visible[second as usize], at),
            _ => self.out.push(json!({ "event": "showAlert", "context": ctx })),
        }
    }

    /// Enemy key released.
    /// Tap = more important spell, double-tap = other spell, long press = quest
    /// Teleport (enemy top laner without Teleport; on other enemies it starts
    /// the other spell).
    fn release_enemy(&mut self, ctx: &str, slot: usize, long: bool) {
        let now = Instant::now();
        let Some(pending) = self.instances.get_mut(ctx).map(|i| i.pending_tap.take()) else { return };
        if long {
            if let Some(first) = pending {
                self.press_enemy(ctx, slot, false, first);
            }
            if self.enemies.get(slot).is_some_and(|e| e.quest_tp.is_some()) {
                self.start(ctx, slot, QUEST_TP);
            } else {
                self.press_enemy(ctx, slot, true, now);
            }
        } else if let Some(first) = pending.filter(|t| now.duration_since(*t) < DOUBLE_TAP) {
            self.press_enemy(ctx, slot, true, first);
        } else if self.visible(slot).len() == 2 {
            // A tap whose window passed but was not handled yet (tick runs every 250 ms).
            if let Some(first) = pending {
                self.press_enemy(ctx, slot, false, first);
            }
            // Wait briefly to tell a tap from a double-tap.
            if let Some(i) = self.instances.get_mut(ctx) {
                i.pending_tap = Some(now);
            }
        } else {
            self.press_enemy(ctx, slot, false, now);
        }
    }

    fn handle(&mut self, ev: &Value) {
        let event = ev["event"].as_str().unwrap_or("");
        let ctx = ev["context"].as_str().unwrap_or("").to_string();
        let p = &ev["payload"];
        match event {
            "willAppear" => {
                let action = ev["action"].as_str().unwrap_or("");
                let kind = if action.ends_with(".enemy") {
                    Kind::Enemy
                } else if action.ends_with(".dial") {
                    Kind::Dial
                } else if action.ends_with(".insight") {
                    Kind::Insight
                } else {
                    Kind::Reset
                };
                let cols = ev["device"].as_str().and_then(|d| self.device_cols.get(d)).copied().unwrap_or(4);
                let slot = slot_for(kind, &p["settings"], &p["coordinates"], cols);
                let encoder = p["controller"].as_str() == Some("Encoder");
                self.instances.insert(
                    ctx,
                    Instance { kind, slot, shown: String::new(), down: None, pending_tap: None, encoder, cols },
                );
            }
            "deviceDidConnect" => {
                if let (Some(id), Some(c)) = (ev["device"].as_str(), ev["deviceInfo"]["size"]["columns"].as_u64()) {
                    self.device_cols.insert(id.to_string(), c as usize);
                }
            }
            "willDisappear" => {
                self.instances.remove(&ctx);
            }
            "didReceiveSettings" => {
                if let Some(i) = self.instances.get_mut(&ctx) {
                    i.slot = slot_for(i.kind, &p["settings"], &p["coordinates"], i.cols);
                    i.shown.clear();
                }
            }
            "didReceiveGlobalSettings" => {
                *self.settings.lock().unwrap() = Settings::from_json(&p["settings"]);
                self.refresh_sync();
            }
            "propertyInspectorDidAppear" => {
                if !self.inspectors.contains(&ctx) { self.inspectors.push(ctx.clone()); }
                self.out.push(json!({ "event": "sendToPropertyInspector", "context": ctx, "payload": { "event": "syncStatus", "status": self.sync_status } }));
            }
            "propertyInspectorDidDisappear" => self.inspectors.retain(|c| c != &ctx),
            "sendToPlugin" if p["event"].as_str() == Some("getSyncStatus") => {
                self.out.push(json!({ "event": "sendToPropertyInspector", "context": ctx, "payload": { "event": "syncStatus", "status": self.sync_status } }));
            }
            // "Test sound" button in the settings panel.
            "sendToPlugin" if p["event"].as_str() == Some("testSound") => {
                sound::play(&self.settings.lock().unwrap());
            }
            "applicationDidLaunch" => self.league_running.store(true, Ordering::Relaxed),
            "applicationDidTerminate" => self.league_running.store(false, Ordering::Relaxed),
            "keyDown" => {
                if let Some(i) = self.instances.get_mut(&ctx) {
                    i.down = Some(Instant::now());
                }
            }
            "keyUp" => {
                let Some(i) = self.instances.get_mut(&ctx) else { return };
                let long = i.down.take().is_some_and(|t| t.elapsed() >= LONG_PRESS);
                let (kind, slot) = (i.kind, i.slot);
                match kind {
                    Kind::Enemy if self.edit_mode => {
                        self.manual_insight[slot] = Some(!self.insight(slot));
                        self.edit_mode = false;
                    }
                    Kind::Enemy => self.release_enemy(&ctx, slot, long),
                    Kind::Insight => self.edit_mode = !self.edit_mode,
                    // With team sync: clears your own starts, teammates' stay.
                    Kind::Reset if long => {
                        self.instances.values_mut().for_each(|i| i.pending_tap = None);
                        if !self.share(Command::Reset) { self.clear_timers(); }
                    }
                    Kind::Reset => self.undo(),
                    // Timer on a normal key (no dial): tap = 5s less, hold = cancel.
                    Kind::Dial if long => self.cancel_rank(slot),
                    Kind::Dial => self.adjust_rank(slot, -5.0),
                }
            }
            "dialRotate" => {
                let Some(rank) = self.instances.get(&ctx).map(|i| i.slot) else { return };
                let ticks = p["ticks"].as_i64().unwrap_or(0);
                self.adjust_rank(rank, ticks as f64 * 5.0);
            }
            "dialDown" | "touchTap" => {
                let Some(rank) = self.instances.get(&ctx).map(|i| i.slot) else { return };
                self.cancel_rank(rank);
            }
            _ => {}
        }
    }

    fn on_update(&mut self, u: Update) {
        match u {
            Update::Game(list, seed) => {
                let same_game = list.len() == self.enemies.len()
                    && list.iter().zip(&self.enemies).all(|(a, b)| a.champ_id == b.champ_id);
                self.match_seed = seed;
                if !same_game {
                    self.clear_timers();
                    self.manual_insight = [None; 5];
                    self.edit_mode = false;
                    self.log(format!(
                        "Game detected: {}",
                        list.iter().map(|e| e.champ.as_str()).collect::<Vec<_>>().join(", ")
                    ));
                }
                self.enemies = list;
                self.refresh_sync();
            }
            Update::NoGame => {
                self.instances.values_mut().for_each(|i| i.pending_tap = None);
                self.enemies.clear();
                self.match_seed = None;
                self.clear_timers();
                self.manual_insight = [None; 5];
                self.edit_mode = false;
                self.refresh_sync();
            }
            Update::Log(m) => self.log(m),
        }
    }

    fn tick(&mut self) {
        while let Ok(event) = self.sync.rx.try_recv() {
            match event {
                sync::Event::Snapshot(generation, snapshot, at) if generation == self.sync.generation => {
                    self.sync.connected_at = Some(Instant::now());
                    self.set_sync_status("Connected");
                    let merge = !self.sync_live;
                    self.sync_live = true;
                    self.apply_snapshot(snapshot, at, merge);
                }
                sync::Event::Status(generation, status) if generation == self.sync.generation => {
                    self.sync.connected_at = None;
                    self.sync_live = false;
                    self.set_sync_status(status);
                }
                _ => {}
            }
        }
        let now = Instant::now();
        // Single taps whose double-tap window has passed.
        let mut due: Vec<(String, usize, Instant)> = self
            .instances
            .iter_mut()
            .filter_map(|(ctx, i)| {
                let first = i.pending_tap.filter(|t| now.duration_since(*t) >= DOUBLE_TAP)?;
                i.pending_tap = None;
                Some((ctx.clone(), i.slot, first))
            })
            .collect();
        due.sort_by_key(|p| p.2);
        for (ctx, slot, first) in due {
            self.press_enemy(&ctx, slot, false, first);
        }
        let mut ring = false;
        for s in 0..5 {
            for i in 0..3 {
                if self.timers[s][i].is_some_and(|t| t.end <= now) {
                    self.timers[s][i] = None;
                    self.fresh[s][i] = Some(now);
                    ring = true;
                }
                if self.fresh[s][i].is_some_and(|f| now.duration_since(f) >= FRESH_FOR) {
                    self.fresh[s][i] = None;
                }
            }
        }
        if ring {
            sound::play(&self.settings.lock().unwrap());
        }
        self.render();
    }

    fn spell_states(&self, slot: usize) -> [SpellState; 3] {
        let now = Instant::now();
        let one = |i: usize| match (self.timers[slot][i], self.fresh[slot][i]) {
            (Some(t), _) => SpellState::Running { remaining: t.end.saturating_duration_since(now).as_secs_f64(), total: t.total },
            (None, Some(_)) => SpellState::Fresh,
            _ => SpellState::Ready,
        };
        [one(0), one(1), one(2)]
    }

    /// Short text describing what a key shows. Cheap; compared before building images.
    fn summary(&self, inst: &Instance, running: &[(usize, usize, f64, f64)]) -> String {
        match inst.kind {
            Kind::Enemy => {
                let e = self.enemies.get(inst.slot);
                let shown: Vec<String> = self
                    .spell_states(inst.slot)
                    .iter()
                    .map(|s| match s {
                        SpellState::Running { remaining, .. } => render::fmt_time(*remaining),
                        other => format!("{other:?}"),
                    })
                    .collect();
                format!(
                    "{:?}|{:?}|{:?}|{}|{}|{}|{}",
                    self.order(inst.slot),
                    self.hidden(inst.slot),
                    e.map(|e| (&e.champ, e.champ_icon.len(), e.boots, e.spells[0].icon.len(), e.spells[1].icon.len(), e.quest_tp.is_some())),
                    shown.join(","),
                    self.insight(inst.slot),
                    self.edit_mode,
                    inst.slot
                )
            }
            Kind::Dial => format!(
                "{:?}",
                running.get(inst.slot).and_then(|&(s, i, rem, tot)| self
                    .enemies
                    .get(s)
                    .map(|e| (&e.champ, i, render::fmt_time(rem), (rem / tot * 100.0).round() as i64)))
            ),
            Kind::Insight => format!("{}", self.edit_mode),
            Kind::Reset => "reset".into(),
        }
    }

    fn message(&self, ctx: &str, inst: &Instance, running: &[(usize, usize, f64, f64)]) -> Value {
        let image = match inst.kind {
            Kind::Enemy => render::enemy_key(
                self.enemies.get(inst.slot),
                inst.slot,
                self.spell_states(inst.slot),
                self.order(inst.slot),
                self.hidden(inst.slot),
                self.insight(inst.slot),
                self.edit_mode,
            ),
            Kind::Dial => {
                let entry = running
                    .get(inst.slot)
                    .and_then(|&(s, i, rem, tot)| self.enemies.get(s).map(|e| (e, i, rem, tot)));
                if inst.encoder {
                    return json!({ "event": "setFeedback", "context": ctx, "payload": render::dial(entry) });
                }
                render::timer_key(entry, inst.slot)
            }
            Kind::Insight => render::insight_key(self.edit_mode),
            Kind::Reset => render::reset_key(),
        };
        json!({ "event": "setImage", "context": ctx, "payload": { "image": image, "target": 0 } })
    }

    fn render(&mut self) {
        let running = self.running();
        let mut updates = Vec::new();
        for (ctx, inst) in &self.instances {
            let summary = self.summary(inst, &running);
            if summary != inst.shown {
                updates.push((ctx.clone(), summary, self.message(ctx, inst, &running)));
            }
        }
        for (ctx, summary, msg) in updates {
            if let Some(i) = self.instances.get_mut(&ctx) {
                i.shown = summary;
            }
            self.out.push(msg);
        }
    }
}

fn arg(args: &[String], key: &str) -> String {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = arg(&args, "-port").parse().expect("missing -port");
    let uuid = arg(&args, "-pluginUUID");
    let register = arg(&args, "-registerEvent");
    let info: Value = serde_json::from_str(&arg(&args, "-info")).unwrap_or(Value::Null);
    let device_cols: HashMap<String, usize> = info["devices"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|d| Some((d["id"].as_str()?.to_string(), d["size"]["columns"].as_u64()? as usize)))
                .collect()
        })
        .unwrap_or_default();

    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to Stream Deck");
    let (mut ws, _) = tungstenite::client(format!("ws://127.0.0.1:{port}"), stream).expect("websocket handshake");
    // Reads time out so the loop can update timers between events.
    ws.get_ref().set_read_timeout(Some(TICK)).ok();

    let settings = Arc::new(Mutex::new(Settings::default()));
    let league_running = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    game::spawn(settings.clone(), league_running.clone(), tx);

    let mut app = App {
        settings,
        league_running,
        instances: HashMap::new(),
        device_cols,
        enemies: Vec::new(),
        timers: [[None; 3]; 5],
        fresh: [[None; 3]; 5],
        manual_insight: [None; 5],
        edit_mode: false,
        history: VecDeque::new(),
        sync: sync::Client::spawn(),
        sync_status: "Off".into(),
        match_seed: None,
        sync_live: false,
        inspectors: Vec::new(),
        frozen: None,
        out: vec![
            json!({ "event": register, "uuid": uuid }),
            json!({ "event": "getGlobalSettings", "context": uuid }),
        ],
    };

    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                if let Ok(ev) = serde_json::from_str::<Value>(&t) {
                    app.handle(&ev);
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break, // Stream Deck closed: exit.
        }
        while let Ok(u) = rx.try_recv() {
            app.on_update(u);
        }
        app.tick();
        for msg in app.out.drain(..) {
            if ws.send(Message::Text(msg.to_string())).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let spell = |id: &str| game::Spell { id: id.into(), name: id.into(), cooldown: 300.0, icon: String::new() };
        let enemies = ["Ahri", "Lux"].into_iter().map(|name| Enemy {
            champ: name.into(), champ_id: name.into(), champ_icon: String::new(), spells: [spell("SummonerFlash"), spell("SummonerHeal")],
            boots: Boots::None, insight_api: None, level: 1, quest_tp: None,
        }).collect();
        App {
            settings: Arc::new(Mutex::new(Settings { sound: "off".into(), ..Settings::default() })),
            league_running: Arc::new(AtomicBool::new(false)), instances: HashMap::new(), device_cols: HashMap::new(), enemies,
            timers: [[None; 3]; 5], fresh: [[None; 3]; 5], manual_insight: [None; 5], edit_mode: false,
            history: VecDeque::new(), sync: sync::Client::spawn(), sync_status: "Off".into(),
            match_seed: Some("ab".repeat(32)), sync_live: false, inspectors: Vec::new(), frozen: None, out: Vec::new(),
        }
    }

    #[test]
    fn undo_walks_back_starts_and_restores_restart_deadline() {
        let mut a = app();
        a.start("a", 0, 0);
        let original = a.timers[0][0].unwrap().end;
        a.start("b", 1, 1);
        a.start("a", 0, 0);
        a.undo();
        assert_eq!(a.timers[0][0].unwrap().end, original);
        a.undo();
        assert!(a.timers[1][1].is_none());
        a.undo();
        assert!(a.timers[0][0].is_none());
        a.undo();
        assert!(a.history.is_empty());
    }

    #[test]
    fn changing_sound_settings_keeps_local_undo_history() {
        let mut a = app();
        a.start("a", 0, 0);
        a.handle(&json!({ "event": "didReceiveGlobalSettings", "payload": { "settings": { "sound": "off", "volume": 20 } } }));
        a.undo();
        assert!(a.timers[0][0].is_none());
    }

    #[test]
    fn delayed_taps_undo_in_press_order_and_reset_discards_pending() {
        let mut a = app();
        a.handle(&json!({ "event": "willAppear", "action": "test.enemy", "context": "a", "payload": { "settings": { "slot": 1 } } }));
        let at = Instant::now() - Duration::from_millis(200);
        a.instances.get_mut("a").unwrap().pending_tap = Some(at);
        a.start("b", 1, 0); // Immediate start resolves before the earlier tap.
        a.undo();
        assert!(a.timers[1][0].is_none());
        assert!(a.timers[0][0].is_some());
        a.undo();
        assert!(a.timers[0][0].is_none());
        a.instances.get_mut("a").unwrap().pending_tap = Some(at);
        a.clear_timers();
        a.tick();
        assert!(a.history.is_empty());
        assert!(a.timers.iter().flatten().all(Option::is_none));
    }

    #[test]
    fn late_resolving_copy_of_same_key_preserves_newer_start() {
        let mut a = app();
        let earlier = Instant::now() - Duration::from_millis(200);
        a.start("newer", 0, 0);
        let latest_end = a.timers[0][0].unwrap().end;
        a.start_at("older", 0, 0, earlier);
        assert_eq!(a.timers[0][0].unwrap().end, latest_end);
        a.undo();
        assert!(a.timers[0][0].unwrap().end < latest_end);
        a.undo();
        assert!(a.timers[0][0].is_none());
    }

    #[test]
    fn cancelled_and_expired_timers_do_not_block_undo() {
        let mut a = app();
        a.start("a", 0, 0);
        a.start("b", 1, 0);
        a.timers[1][0].as_mut().unwrap().end = Instant::now() - Duration::from_secs(1);
        a.undo();
        assert!(a.timers[0][0].is_none());
        a.clear_timers();
        a.start("a", 0, 0);
        a.start("b", 1, 0);
        a.cancel_rank(0);
        a.undo();
        assert!(a.timers.iter().flatten().all(Option::is_none));
    }

    #[test]
    fn disconnected_sync_keeps_editing_locally() {
        let mut a = app();
        a.settings.lock().unwrap().sync_enabled = true;
        a.refresh_sync(); // no relay URL: never connects
        a.start("a", 0, 0);
        a.start("b", 1, 0);
        assert!(a.timers[0][0].is_some() && a.timers[1][0].is_some());
        a.undo();
        assert!(a.timers[1][0].is_none());
        assert!(a.timers[0][0].is_some());
    }

    #[test]
    fn first_snapshot_merges_local_timers_and_later_ones_replace() {
        let mut a = app();
        a.start("a", 0, 0);
        let room = |a: &App| Snapshot { now_ms: 0, revision: 1, acknowledged: 0, timers: vec![
            summoner_watch::shared::SharedTimer { key: a.shared_key(1, 0), end_ms: 100_000, total_ms: 300_000 },
        ] };
        a.apply_snapshot(room(&a), Instant::now(), true);
        assert!(a.timers[0][0].is_some(), "local timer kept on connect");
        assert!(a.timers[1][0].is_some(), "room timer shown");
        a.apply_snapshot(room(&a), Instant::now(), false);
        assert!(a.timers[0][0].is_none(), "room is the source of truth once connected");
    }

    #[test]
    fn spectators_do_not_sync() {
        let mut a = app();
        a.match_seed = None;
        *a.settings.lock().unwrap() = Settings { sync_enabled: true, sync_url: "https://example.com".into(), ..Settings::default() };
        a.refresh_sync();
        assert_eq!(a.sync_status, "Only works while you play in the match");
    }

    #[test]
    fn snapshots_map_by_champion_and_spell_and_teleport_upgrade_keeps_identity() {
        let mut a = app();
        a.enemies[0].spells[1].id = "S12_SummonerTeleportUpgrade".into();
        assert_eq!(a.shared_key(0, 1).spell, "SummonerTeleport");
        let snapshot = Snapshot { now_ms: 50_000, revision: 1, acknowledged: 0, timers: vec![
            summoner_watch::shared::SharedTimer { key: a.shared_key(1, 0), end_ms: 150_000, total_ms: 300_000 },
        ] };
        a.enemies.swap(0, 1);
        a.apply_snapshot(snapshot, Instant::now(), false);
        assert!(a.timers[0][0].is_some());
        assert!(a.timers[1][0].is_none());
        let left = a.timers[0][0].unwrap().end.duration_since(Instant::now()).as_secs_f64();
        assert!((99.0..=100.0).contains(&left));
    }

    #[cfg(feature = "relay")]
    #[test]
    fn two_apps_converge_through_the_websocket_relay() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let config = summoner_watch::relay::Config { access_key: "cd".repeat(16), ip_header: None };
        std::thread::spawn(move || summoner_watch::relay::serve(listener, config));
        let mut a = app();
        let mut b = app();
        for app in [&mut a, &mut b] {
            *app.settings.lock().unwrap() = Settings { sync_enabled: true, sync_url: url.clone(), sync_key: "cd".repeat(16), sound: "off".into(), ..Settings::default() };
            app.refresh_sync();
        }
        let pump = |a: &mut App, b: &mut App, ready: fn(&App, &App) -> bool| {
            let until = Instant::now() + Duration::from_secs(5);
            while !ready(a, b) && Instant::now() < until {
                a.tick(); b.tick(); std::thread::sleep(Duration::from_millis(10));
            }
            assert!(ready(a, b), "apps did not converge before deadline");
        };
        pump(&mut a, &mut b, |a, b| a.sync.connected_at.is_some() && b.sync.connected_at.is_some());
        a.start("a", 0, 0);
        pump(&mut a, &mut b, |a, b| a.timers[0][0].is_some() && b.timers[0][0].is_some());
        let first = a.timers[0][0].unwrap().end;
        let second = b.timers[0][0].unwrap().end;
        let difference = first.saturating_duration_since(second).max(second.saturating_duration_since(first));
        assert!(difference < Duration::from_millis(100));
        b.start("b", 1, 1);
        pump(&mut a, &mut b, |a, b| a.timers[1][1].is_some() && b.timers[1][1].is_some());
        // Undo reverts only the presser's own start.
        a.undo();
        pump(&mut a, &mut b, |a, b| a.timers[0][0].is_none() && b.timers[0][0].is_none());
        assert!(a.timers[1][1].is_some() && b.timers[1][1].is_some());
        // Offline start on b is merged into the room when b reconnects.
        b.settings.lock().unwrap().sync_enabled = false;
        b.refresh_sync();
        b.start("b", 1, 0);
        b.settings.lock().unwrap().sync_enabled = true;
        b.refresh_sync();
        pump(&mut a, &mut b, |a, b| a.timers[1][0].is_some() && b.timers[1][0].is_some());
        // Reset clears only the presser's own starts.
        b.settings.lock().unwrap().sync_enabled = false;
        b.refresh_sync();
        a.start("a", 0, 0);
        a.share(Command::Reset);
        pump(&mut a, &mut b, |a, _| a.timers[0][0].is_none());
        assert!(a.timers[1][1].is_some(), "b's start survives a's reset");
        assert!(b.timers[1][1].is_some(), "sync-off must retain local countdowns");
    }

    fn at(col: u64, row: u64) -> Value {
        json!({ "column": col, "row": row })
    }

    #[test]
    fn frozen_timer_order_survives_a_pass() {
        // Dial 1 shows Flash (100 s), dial 2 Heal (120 s). Dial 1 adds 30 s.
        let after = vec![(0, 0, 130.0, 300.0), (1, 0, 120.0, 240.0), (2, 1, 50.0, 300.0)];
        let sorted = order_timers(after.clone(), None);
        assert_eq!(sorted.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(), [(2, 1), (1, 0), (0, 0)]);
        // While frozen, Flash stays on dial 1; the timer started since then goes last.
        let frozen = order_timers(after, Some(&[(0, 0), (1, 0)]));
        assert_eq!(frozen.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(), [(0, 0), (1, 0), (2, 1)]);
    }

    #[test]
    fn auto_enemy_slots_follow_device_width() {
        let none = json!({});
        // Stream Deck +: 4 columns, keys 1-4 top row then bottom-left.
        assert_eq!(slot_for(Kind::Enemy, &none, &at(3, 0), 4), 3);
        assert_eq!(slot_for(Kind::Enemy, &none, &at(0, 1), 4), 4);
        // Stream Deck XL: 8 columns, first 5 keys of the top row.
        assert_eq!(slot_for(Kind::Enemy, &none, &at(4, 0), 8), 4);
        assert_eq!(slot_for(Kind::Enemy, &none, &at(0, 1), 8), 3); // 8 % 5
        // Explicit setting wins.
        assert_eq!(slot_for(Kind::Enemy, &json!({ "slot": "2" }), &at(7, 3), 8), 1);
        assert_eq!(slot_for(Kind::Dial, &none, &at(6, 2), 8), 6);
    }
}
