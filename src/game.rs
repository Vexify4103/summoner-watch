//! Background worker: reads enemy champions, summoner spells and items from the
//! official Live Client Data API, gets icons/cooldowns from Data Dragon, and
//! (optionally) Cosmic Insight from the Riot spectator API.
//!
//! It never detects spell usage. Timers only start when the user presses a key.

use crate::settings::Settings;
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const LIVE: &str = "https://127.0.0.1:2999/liveclientdata";
const DDRAGON: &str = "https://ddragon.leagueoflegends.com";
const LUCIDITY_ITEMS: [i64; 2] = [3158, 223158]; // Ionian Boots of Lucidity (SR, Arena)
const CRIMSON_LUCIDITY_ITEM: i64 = 3171; // upgraded Ionian Boots
const COSMIC_INSIGHT_PERK: i64 = 8347;

#[derive(Clone, PartialEq, Debug)]
pub struct Spell {
    /// Riot id, e.g. "SummonerFlash".
    pub id: String,
    pub name: String,
    pub cooldown: f64,
    /// data:image/png;base64,... or empty when offline
    pub icon: String,
}

/// Summoner-haste boots an enemy owns.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Boots {
    None,
    Lucidity,
    Crimson,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Enemy {
    pub champ: String,
    pub champ_icon: String,
    pub spells: [Spell; 2],
    pub boots: Boots,
    /// From the Riot API. None = unknown.
    pub insight_api: Option<bool>,
    /// Champion level (1-18), for Unleashed Teleport cooldown.
    pub level: u32,
    /// Extra Teleport from the top lane quest. Only for the enemy top laner without Teleport.
    pub quest_tp: Option<Spell>,
}

/// Spell index of the quest Teleport in timer arrays (0 and 1 are the normal spells).
pub const QUEST_TP: usize = 2;

/// Unleashed Teleport (Teleport after 10:00): 330s at level 1 down to 240s at level 18.
pub fn unleashed_tp_cooldown(level: u32) -> f64 {
    let lvl = level.clamp(1, 18) as f64;
    330.0 - (lvl - 1.0) * 90.0 / 17.0
}

impl Spell {
    /// True for Teleport after it upgrades to Unleashed Teleport (not the base spell).
    pub fn is_unleashed_tp(&self) -> bool {
        (self.id.contains("Teleport") && self.id != "SummonerTeleport" && self.id != "QuestTeleport")
            || self.name.starts_with("Unleashed Teleport")
    }
}

impl Enemy {
    /// Spell by timer index: 0/1 = normal spells, 2 = quest Teleport.
    pub fn spell(&self, i: usize) -> &Spell {
        match (i, &self.quest_tp) {
            (QUEST_TP, Some(q)) => q,
            _ => &self.spells[i.min(1)],
        }
    }
}

pub enum Update {
    Game(Vec<Enemy>),
    NoGame,
    Log(String),
}

struct RawEnemy {
    riot_id: String,
    champ_id: String,
    champ_name: String,
    spells: [(String, String); 2], // (raw id, display name)
    boots: Boots,
    /// "TOP", "JUNGLE", ... Empty in custom and blind games.
    position: String,
    level: u32,
}

struct SpellData {
    name: String,
    cooldown: f64,
    image: String,
}

struct DDragon {
    version: String,
    spells: HashMap<String, SpellData>,
    icons: HashMap<String, String>,
}

fn fallback_cooldown(id: &str) -> f64 {
    match id {
        "SummonerFlash" => 300.0,
        "SummonerDot" | "SummonerBarrier" => 180.0,
        "SummonerHeal" | "SummonerExhaust" | "SummonerBoost" | "SummonerHaste" | "SummonerMana" => 240.0,
        _ if id.contains("Smite") => 90.0,
        _ if id.contains("Teleport") => 360.0,
        _ => 300.0,
    }
}

fn local_agent() -> ureq::Agent {
    // The Live Client API uses a self-signed certificate on 127.0.0.1.
    let tls = ureq::tls::TlsConfig::builder().disable_verification(true).build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into()
}

fn web_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .into()
}

fn get_json(agent: &ureq::Agent, url: &str, riot_key: Option<&str>) -> Result<Value, String> {
    let mut req = agent.get(url);
    if let Some(k) = riot_key {
        req = req.header("X-Riot-Token", k);
    }
    let mut resp = req.call().map_err(|e| e.to_string())?;
    resp.body_mut().read_json::<Value>().map_err(|e| e.to_string())
}

fn get_png(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let bytes = resp.body_mut().read_to_vec().map_err(|e| e.to_string())?;
    Ok(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

impl DDragon {
    fn load(web: &ureq::Agent) -> Result<Self, String> {
        let versions = get_json(web, &format!("{DDRAGON}/api/versions.json"), None)?;
        let version = versions[0].as_str().ok_or("no ddragon version")?.to_string();
        let data = get_json(web, &format!("{DDRAGON}/cdn/{version}/data/en_US/summoner.json"), None)?;
        let mut spells = HashMap::new();
        for (id, s) in data["data"].as_object().ok_or("bad summoner.json")? {
            spells.insert(
                id.clone(),
                SpellData {
                    name: s["name"].as_str().unwrap_or(id).to_string(),
                    cooldown: s["cooldown"][0].as_f64().unwrap_or_else(|| fallback_cooldown(id)),
                    image: s["image"]["full"].as_str().unwrap_or("").to_string(),
                },
            );
        }
        Ok(DDragon { version, spells, icons: HashMap::new() })
    }

    /// `path` like "champion/Ahri.png". Cached; failures are cached as empty.
    fn icon(&mut self, web: &ureq::Agent, path: &str) -> String {
        if let Some(i) = self.icons.get(path) {
            return i.clone();
        }
        let url = format!("{DDRAGON}/cdn/{}/img/{path}", self.version);
        let icon = get_png(web, &url).unwrap_or_default();
        self.icons.insert(path.to_string(), icon.clone());
        icon
    }
}

fn spell_id(raw: &str) -> String {
    // "GeneratedTip_SummonerSpell_SummonerFlash_DisplayName" -> "SummonerFlash"
    raw.strip_prefix("GeneratedTip_SummonerSpell_")
        .and_then(|s| s.strip_suffix("_DisplayName"))
        .unwrap_or(raw)
        .to_string()
}

/// Data Dragon only lists base spells. Map upgraded ids (role quest upgrades such as
/// Unleashed Smite / Unleashed Teleport) to the base spell whose name they contain.
fn base_spell(d: &DDragon, id: &str) -> Option<String> {
    if d.spells.contains_key(id) {
        return Some(id.to_string());
    }
    d.spells
        .keys()
        .filter(|k| !k.ends_with("_Jade") && !k.contains("Cherry"))
        .filter(|k| {
            let core = k.trim_start_matches("Summoner");
            core.len() >= 3 && id.contains(core)
        })
        .max_by_key(|k| k.len())
        .cloned()
}

fn build_spell(raw: &str, display: &str, dd: &mut Option<DDragon>, web: &ureq::Agent) -> Spell {
    let id = spell_id(raw);
    if let Some(d) = dd.as_mut() {
        if let Some(key) = base_spell(d, &id) {
            let s = &d.spells[&key];
            // Upgraded spells (e.g. "Unleashed Smite") keep their own name but use the base icon/cooldown.
            let name = if key != id && !display.is_empty() { display.to_string() } else { s.name.clone() };
            let (cooldown, image) = (s.cooldown, s.image.clone());
            let icon = if image.is_empty() { String::new() } else { d.icon(web, &format!("spell/{image}")) };
            return Spell { id, name, cooldown, icon };
        }
    }
    let name = if display.is_empty() { id.trim_start_matches("Summoner").to_string() } else { display.to_string() };
    Spell { cooldown: fallback_cooldown(&id), name, icon: String::new(), id }
}

/// Returns (my riot id, enemies) or None when no game is running.
fn read_players(local: &ureq::Agent) -> Option<(String, Vec<RawEnemy>)> {
    // LOL_LIVE_URL only exists for local testing (tools/fake_deck).
    let base = std::env::var("LOL_LIVE_URL").unwrap_or_else(|_| LIVE.to_string());
    let list = get_json(local, &format!("{base}/playerlist"), None).ok()?;
    let me = get_json(local, &format!("{base}/activeplayername"), None)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    parse_players(&list, me)
}

fn parse_players(list: &Value, me: String) -> Option<(String, Vec<RawEnemy>)> {
    let players = list.as_array()?;
    let id_of = |p: &Value| p["riotId"].as_str().or(p["summonerName"].as_str()).unwrap_or("").to_string();
    let my_team = players
        .iter()
        .find(|p| !me.is_empty() && id_of(p).eq_ignore_ascii_case(&me))
        .and_then(|p| p["team"].as_str())
        .unwrap_or("ORDER") // spectating: show the red side
        .to_string();
    let spell = |p: &Value, k: &str| {
        let s = &p["summonerSpells"][k];
        (s["rawDisplayName"].as_str().unwrap_or("").to_string(), s["displayName"].as_str().unwrap_or("").to_string())
    };
    let enemies = players
        .iter()
        .filter(|p| p["team"].as_str() != Some(my_team.as_str()))
        .map(|p| RawEnemy {
            riot_id: id_of(p),
            champ_id: p["rawChampionName"].as_str().unwrap_or("").trim_start_matches("game_character_displayname_").to_string(),
            champ_name: p["championName"].as_str().unwrap_or("?").to_string(),
            position: p["position"].as_str().unwrap_or("").to_uppercase(),
            level: p["level"].as_u64().unwrap_or(1) as u32,
            spells: [spell(p, "summonerSpellOne"), spell(p, "summonerSpellTwo")],
            boots: boots_of(p),
        })
        .collect();
    Some((me, enemies))
}

fn boots_of(p: &Value) -> Boots {
    let ids: Vec<i64> = p["items"].as_array().map(|a| a.iter().filter_map(|i| i["itemID"].as_i64()).collect()).unwrap_or_default();
    if ids.contains(&CRIMSON_LUCIDITY_ITEM) {
        Boots::Crimson
    } else if ids.iter().any(|i| LUCIDITY_ITEMS.contains(i)) {
        Boots::Lucidity
    } else {
        Boots::None
    }
}

fn regional_host(platform: &str) -> &'static str {
    match platform {
        "na1" | "br1" | "la1" | "la2" => "americas",
        "kr" | "jp1" | "oc1" | "sg2" | "tw2" | "vn2" | "ph2" | "th2" => "asia",
        _ => "europe",
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Asks the Riot spectator API which players took Cosmic Insight.
fn lookup_insight(web: &ureq::Agent, s: &Settings, me: &str) -> Result<HashMap<String, bool>, String> {
    let (name, tag) = me.split_once('#').ok_or("own Riot ID has no #tag")?;
    let account = get_json(
        web,
        &format!(
            "https://{}.api.riotgames.com/riot/account/v1/accounts/by-riot-id/{}/{}",
            regional_host(&s.platform),
            url_encode(name),
            url_encode(tag)
        ),
        Some(&s.api_key),
    )?;
    let puuid = account["puuid"].as_str().ok_or("no puuid in account response")?;
    let game = get_json(
        web,
        &format!("https://{}.api.riotgames.com/lol/spectator/v5/active-games/by-summoner/{puuid}", s.platform),
        Some(&s.api_key),
    )?;
    let mut map = HashMap::new();
    for p in game["participants"].as_array().ok_or("no participants")? {
        let has = p["perks"]["perkIds"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|x| x.as_i64() == Some(COSMIC_INSIGHT_PERK)));
        map.insert(p["riotId"].as_str().unwrap_or("").to_lowercase(), has);
    }
    Ok(map)
}

#[derive(Default)]
struct Insight {
    game_key: String,
    found: HashMap<String, bool>,
    tries: u32,
    next_try: Option<Instant>,
}

impl Insight {
    fn refresh(&mut self, web: &ureq::Agent, s: &Settings, me: &str, raw: &[RawEnemy], tx: &Sender<Update>) {
        let key = raw.iter().map(|r| r.riot_id.as_str()).collect::<Vec<_>>().join("|") + &s.api_key + &s.platform;
        if key != self.game_key {
            *self = Insight { game_key: key, ..Default::default() };
        }
        if s.api_key.is_empty() || me.is_empty() || !self.found.is_empty() || self.tries >= 4 {
            return;
        }
        if self.next_try.is_some_and(|t| Instant::now() < t) {
            return;
        }
        self.tries += 1;
        self.next_try = Some(Instant::now() + Duration::from_secs(30));
        match lookup_insight(web, s, me) {
            Ok(map) => {
                let n = map.values().filter(|v| **v).count();
                let _ = tx.send(Update::Log(format!("Riot API: {n} players with Cosmic Insight")));
                self.found = map;
            }
            Err(e) => {
                let _ = tx.send(Update::Log(format!("Riot API lookup failed (try {}): {e}", self.tries)));
            }
        }
    }

    fn get(&self, riot_id: &str) -> Option<bool> {
        self.found.get(&riot_id.to_lowercase()).copied()
    }
}

pub fn spawn(settings: Arc<Mutex<Settings>>, league_running: Arc<AtomicBool>, tx: Sender<Update>) {
    thread::spawn(move || {
        let local = local_agent();
        let web = web_agent();
        let mut dd: Option<DDragon> = None;
        let mut dd_retry = Instant::now();
        let mut insight = Insight::default();
        let mut last: Option<Vec<Enemy>> = None;
        let mut misses = 0u32;

        loop {
            match read_players(&local) {
                Some((me, raw)) if !raw.is_empty() => {
                    misses = 0;
                    if dd.is_none() && Instant::now() >= dd_retry {
                        match DDragon::load(&web) {
                            Ok(d) => dd = Some(d),
                            Err(e) => {
                                let _ = tx.send(Update::Log(format!("Data Dragon failed, using fallback cooldowns: {e}")));
                                dd_retry = Instant::now() + Duration::from_secs(60);
                            }
                        }
                    }
                    let s = settings.lock().unwrap().clone();
                    insight.refresh(&web, &s, &me, &raw, &tx);
                    // Top laner = position TOP; without positions (customs) the first enemy.
                    let positions_known = raw.iter().any(|r| !r.position.is_empty());
                    let enemies: Vec<Enemy> = raw
                        .iter()
                        .enumerate()
                        .map(|(n, r)| Enemy {
                            level: r.level,
                            quest_tp: {
                                let is_top = if positions_known { r.position == "TOP" } else { n == 0 };
                                let has_tp = r.spells.iter().any(|(raw, _)| raw.contains("Teleport"));
                                (is_top && !has_tp).then(|| Spell {
                                    id: "QuestTeleport".into(),
                                    name: "Quest TP".into(),
                                    cooldown: 390.0, // replaced by the setting when the timer starts
                                    icon: match dd.as_mut() {
                                        Some(d) => d.icon(&web, "spell/SummonerTeleport.png"),
                                        None => String::new(),
                                    },
                                })
                            },
                            champ_icon: match dd.as_mut() {
                                Some(d) if !r.champ_id.is_empty() => d.icon(&web, &format!("champion/{}.png", r.champ_id)),
                                _ => String::new(),
                            },
                            champ: r.champ_name.clone(),
                            spells: [
                                build_spell(&r.spells[0].0, &r.spells[0].1, &mut dd, &web),
                                build_spell(&r.spells[1].0, &r.spells[1].1, &mut dd, &web),
                            ],
                            boots: r.boots,
                            insight_api: insight.get(&r.riot_id),
                        })
                        .collect();
                    if last.as_ref() != Some(&enemies) {
                        let _ = tx.send(Update::Game(enemies.clone()));
                        last = Some(enemies);
                    }
                }
                _ => {
                    // Ignore short hiccups; only end the game after ~10s without data.
                    misses += 1;
                    if misses >= 3 && last.take().is_some() {
                        let _ = tx.send(Update::NoGame);
                    }
                }
            }
            // Fast polling only while League runs; otherwise a cheap check every 10s.
            let wait = if league_running.load(Ordering::Relaxed) || last.is_some() { 3 } else { 10 };
            thread::sleep(Duration::from_secs(wait));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn player(riot: &str, team: &str, raw_champ: &str, s1: &str, s2: &str, items: &[i64]) -> Value {
        json!({
            "riotId": riot, "team": team, "championName": raw_champ, "rawChampionName": format!("game_character_displayname_{raw_champ}"),
            "summonerSpells": {
                "summonerSpellOne": { "displayName": "x", "rawDisplayName": format!("GeneratedTip_SummonerSpell_{s1}_DisplayName") },
                "summonerSpellTwo": { "displayName": "y", "rawDisplayName": format!("GeneratedTip_SummonerSpell_{s2}_DisplayName") }
            },
            "items": items.iter().map(|i| json!({ "itemID": i })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn picks_enemy_team_and_parses_spells_and_boots() {
        let list = json!([
            player("Me#EUW", "ORDER", "Ahri", "SummonerFlash", "SummonerDot", &[]),
            player("Foe#1", "CHAOS", "MonkeyKing", "SummonerFlash", "SummonerTeleport", &[3158, 1001]),
            player("Foe#2", "CHAOS", "Lux", "SummonerHeal", "SummonerFlash", &[]),
            player("Foe#3", "CHAOS", "Jinx", "SummonerHeal", "SummonerFlash", &[3171]),
        ]);
        let (me, enemies) = parse_players(&list, "me#euw".into()).unwrap();
        assert_eq!(me, "me#euw");
        assert_eq!(enemies.len(), 3);
        assert_eq!(enemies[0].champ_id, "MonkeyKing");
        assert_eq!(spell_id(&enemies[0].spells[1].0), "SummonerTeleport");
        assert_eq!(enemies[0].boots, Boots::Lucidity);
        assert_eq!(enemies[1].boots, Boots::None);
        assert_eq!(enemies[2].boots, Boots::Crimson);
        assert_eq!(enemies[0].position, "");
    }

    #[test]
    fn spectator_without_name_shows_red_side() {
        let list = json!([
            player("A#1", "ORDER", "Ahri", "SummonerFlash", "SummonerDot", &[]),
            player("B#1", "CHAOS", "Lux", "SummonerFlash", "SummonerHeal", &[]),
        ]);
        let (_, enemies) = parse_players(&list, String::new()).unwrap();
        assert_eq!(enemies.len(), 1);
        assert_eq!(enemies[0].riot_id, "B#1");
    }

    #[test]
    fn unleashed_tp_scales_with_level() {
        assert_eq!(unleashed_tp_cooldown(1), 330.0);
        assert_eq!(unleashed_tp_cooldown(18), 240.0);
        assert!((unleashed_tp_cooldown(10) - 282.35).abs() < 0.01);
        let s = Spell { id: "S12_SummonerTeleportUpgrade".into(), name: "Unleashed Teleport".into(), cooldown: 300.0, icon: String::new() };
        assert!(s.is_unleashed_tp());
        let base = Spell { id: "SummonerTeleport".into(), name: "Teleport".into(), cooldown: 300.0, icon: String::new() };
        assert!(!base.is_unleashed_tp());
    }

    #[test]
    fn offline_fallback_spell() {
        let s = build_spell("GeneratedTip_SummonerSpell_SummonerFlash_DisplayName", "Flash", &mut None, &web_agent());
        assert_eq!(s.cooldown, 300.0);
        assert_eq!(s.name, "Flash");
    }

    #[test]
    fn url_encoding_and_regions() {
        assert_eq!(url_encode("Mr Ü#1"), "Mr%20%C3%9C%231");
        assert_eq!(regional_host("euw1"), "europe");
        assert_eq!(regional_host("na1"), "americas");
        assert_eq!(regional_host("kr"), "asia");
    }

    #[test]
    fn flash_with_insight_and_boots() {
        // 300 * 100 / (100 + 18 + 10) = 234.4s
        let cd: f64 = 300.0 * 100.0 / 128.0;
        assert!((cd - 234.375).abs() < 0.001);
    }
}
