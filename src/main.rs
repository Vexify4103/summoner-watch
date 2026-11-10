// Console-subsystem exe on purpose: Stream Deck starts plugins hidden, and the
// windows_subsystem flag made Kaspersky lock the exe as suspicious.

mod game;
mod render;
mod settings;
mod sound;

use game::{Boots, Enemy, Update, QUEST_TP};
use render::SpellState;
use serde_json::{json, Value};
use settings::Settings;
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::Message;

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
    last_started: Option<(usize, usize)>,
    out: Vec<Value>,
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
        self.timers = [[None; 3]; 5];
        self.fresh = [[None; 3]; 5];
        self.last_started = None;
    }

    /// Running timers sorted by time left: (enemy, spell, remaining, total).
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
        list.sort_by(|a, b| a.2.total_cmp(&b.2));
        list
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
        self.timers[slot][idx] = Some(Timer { end: at + Duration::from_secs_f64(secs), total: secs });
        self.fresh[slot][idx] = None;
        self.last_started = Some((slot, idx));
    }

    /// Moves the Nth soonest timer by `delta` seconds (never below 1s left).
    fn adjust_rank(&mut self, rank: usize, delta: f64) {
        if let Some(&(s, i, ..)) = self.running().get(rank) {
            if let Some(t) = self.timers[s][i].as_mut() {
                let now = Instant::now();
                let left = (t.end.saturating_duration_since(now).as_secs_f64() + delta).max(1.0);
                t.end = now + Duration::from_secs_f64(left);
                t.total = t.total.max(left);
            }
        }
    }

    fn cancel_rank(&mut self, rank: usize) {
        if let Some(&(s, i, ..)) = self.running().get(rank) {
            self.timers[s][i] = None;
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
                    Kind::Reset if long => self.clear_timers(),
                    Kind::Reset => {
                        if let Some((s, i)) = self.last_started.take() {
                            self.timers[s][i] = None;
                        }
                    }
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
            Update::Game(list) => {
                let same_game = list.len() == self.enemies.len()
                    && list.iter().zip(&self.enemies).all(|(a, b)| a.champ == b.champ);
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
            }
            Update::NoGame => {
                self.instances.values_mut().for_each(|i| i.pending_tap = None);
                self.enemies.clear();
                self.clear_timers();
                self.manual_insight = [None; 5];
                self.edit_mode = false;
            }
            Update::Log(m) => self.log(m),
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        // Single taps whose double-tap window has passed.
        let due: Vec<(String, usize, Instant)> = self
            .instances
            .iter_mut()
            .filter_map(|(ctx, i)| {
                let first = i.pending_tap.filter(|t| now.duration_since(*t) >= DOUBLE_TAP)?;
                i.pending_tap = None;
                Some((ctx.clone(), i.slot, first))
            })
            .collect();
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
        last_started: None,
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
            if ws.send(Message::Text(msg.to_string().into())).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(col: u64, row: u64) -> Value {
        json!({ "column": col, "row": row })
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
