use serde_json::Value;

/// Plugin-wide settings, edited in the Stream Deck property inspector (global settings).
#[derive(Clone, Debug)]
pub struct Settings {
    pub api_key: String,
    pub platform: String,
    /// Seconds subtracted from every timer, to cover reaction time.
    pub offset: f64,
    pub insight_haste: f64,
    pub lucidity_haste: f64,
    pub crimson_haste: f64,
    /// Base cooldown of the top lane quest Teleport.
    pub quest_tp_cd: f64,
    /// 0-100, volume of the built-in sound.
    pub volume: f64,
    /// Optional custom .wav file. Overrides the built-in sound.
    pub sound_path: String,
    /// Spell importance, most important first (lowercase). Decides which spell is the short press.
    pub spell_order: Vec<String>,
    /// Spells that are never tracked (lowercase), e.g. Smite.
    pub ignore_spells: Vec<String>,
}

pub const DEFAULT_IGNORE: &str = "Smite";
pub const DEFAULT_SPELL_ORDER: &str = "Flash, Teleport, Ignite, Cleanse, Exhaust, Barrier, Heal, Ghost, Smite";

fn parse_order(s: &str) -> Vec<String> {
    s.split([',', '>', ';'])
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .map(|p| match p.as_str() {
            "tp" => "teleport".to_string(),
            "dot" => "ignite".to_string(),
            "exh" => "exhaust".to_string(),
            _ => p,
        })
        .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            api_key: String::new(),
            platform: "euw1".into(),
            offset: 3.0,
            insight_haste: 18.0,
            lucidity_haste: 10.0,
            crimson_haste: 20.0,
            quest_tp_cd: 390.0,
            volume: 60.0,
            sound_path: String::new(),
            spell_order: parse_order(DEFAULT_SPELL_ORDER),
            ignore_spells: parse_order(DEFAULT_IGNORE),
        }
    }
}

// The property inspector stores text fields as strings, so accept both.
fn num(v: &Value, key: &str, default: f64) -> f64 {
    match &v[key] {
        Value::Number(n) => n.as_f64().unwrap_or(default),
        Value::String(s) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn text(v: &Value, key: &str, default: &str) -> String {
    v[key]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Settings {
    pub fn from_json(v: &Value) -> Self {
        let d = Settings::default();
        let platform: String = text(v, "platform", &d.platform)
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        Settings {
            api_key: text(v, "riotApiKey", ""),
            platform: if platform.is_empty() { d.platform } else { platform },
            offset: num(v, "offset", d.offset).clamp(0.0, 30.0),
            insight_haste: num(v, "insightHaste", d.insight_haste).clamp(0.0, 200.0),
            lucidity_haste: num(v, "lucidityHaste", d.lucidity_haste).clamp(0.0, 200.0),
            crimson_haste: num(v, "crimsonHaste", d.crimson_haste).clamp(0.0, 200.0),
            quest_tp_cd: num(v, "questTpCooldown", d.quest_tp_cd).clamp(1.0, 1200.0),
            volume: num(v, "volume", d.volume).clamp(0.0, 100.0),
            sound_path: text(v, "soundPath", ""),
            spell_order: parse_order(&text(v, "spellOrder", DEFAULT_SPELL_ORDER)),
            // Empty field = default; "none" = track everything.
            ignore_spells: parse_order(&text(v, "ignoreSpells", DEFAULT_IGNORE)).into_iter().filter(|p| p != "none").collect(),
        }
    }

    pub fn is_ignored(&self, id: &str, name: &str) -> bool {
        let (id, name) = (id.to_lowercase(), name.to_lowercase());
        self.ignore_spells.iter().any(|p| name.contains(p.as_str()) || id.contains(p.as_str()))
    }

    /// Position of a spell in the importance list. Unlisted spells rank last.
    pub fn spell_rank(&self, id: &str, name: &str) -> usize {
        let (id, name) = (id.to_lowercase(), name.to_lowercase());
        self.spell_order
            .iter()
            .position(|p| name.contains(p.as_str()) || id.contains(p.as_str()))
            .unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_order_puts_flash_first() {
        let s = Settings::default();
        assert!(s.spell_rank("SummonerFlash", "Flash") < s.spell_rank("SummonerTeleport", "Teleport"));
        assert!(s.spell_rank("S12_SummonerTeleportUpgrade", "Unleashed Teleport") < s.spell_rank("SummonerDot", "Ignite"));
        assert_eq!(s.spell_rank("SummonerMana", "Clarity"), usize::MAX);
    }

    #[test]
    fn smite_ignored_by_default() {
        let s = Settings::default();
        assert!(s.is_ignored("S5_SummonerSmiteDuel", "Unleashed Smite"));
        assert!(!s.is_ignored("SummonerFlash", "Flash"));
        let all = Settings::from_json(&json!({ "ignoreSpells": "none" }));
        assert!(!all.is_ignored("SummonerSmite", "Smite"));
    }

    #[test]
    fn custom_order_with_aliases() {
        let s = Settings::from_json(&json!({ "spellOrder": "tp > flash > heal" }));
        assert_eq!(s.spell_order, vec!["teleport", "flash", "heal"]);
        assert!(s.spell_rank("SummonerTeleport", "Teleport") < s.spell_rank("SummonerFlash", "Flash"));
    }
}
