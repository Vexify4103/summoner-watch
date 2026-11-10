//! Builds key images (SVG data URLs) and touch strip feedback.

use crate::game::{Boots, Enemy};
use base64::Engine;
use serde_json::{json, Value};

const BG: &str = "#0d1117";
const GOLD: &str = "#e8b33a";
const GREEN: &str = "#1f8f3a";
const PURPLE: &str = "#6b4bd1";
const EMPTY_PNG: &str =
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SpellState {
    Ready,
    /// Came back up recently: highlighted so you notice it.
    Fresh,
    Running { remaining: f64, total: f64 },
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.ceil().max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Fits a spell name into a 72px cell when there is no icon.
fn short_name(name: &str) -> String {
    let name = name.strip_prefix("Unleashed ").unwrap_or(name);
    if name.chars().count() <= 10 {
        name.to_string()
    } else {
        name.chars().take(9).collect::<String>() + "\u{2026}"
    }
}

fn svg_url(body: &str) -> String {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="144" height="144" viewBox="0 0 144 144"><rect width="144" height="144" fill="{BG}"/>{body}</svg>"#
    );
    format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg))
}

fn text(x: i32, y: i32, size: i32, fill: &str, s: &str) -> String {
    format!(
        r#"<text x="{x}" y="{y}" font-family="Arial" font-weight="bold" font-size="{size}" fill="{fill}" text-anchor="middle">{}</text>"#,
        esc(s)
    )
}

fn image(x: i32, y: i32, size: i32, href: &str) -> String {
    format!(r#"<image x="{x}" y="{y}" width="{size}" height="{size}" xlink:href="{href}"/>"#)
}

/// Enemy key: champion on top, both summoner spells below with their timers.
/// `states` is indexed like `enemy.spells`; `order` says which spell goes left (short press).
/// `hidden` (by spell index) marks ignored spells; one visible spell is drawn centered.
pub fn enemy_key(
    enemy: Option<&Enemy>,
    slot: usize,
    states: [SpellState; 3],
    order: [usize; 2],
    hidden: [bool; 2],
    insight: bool,
    edit_mode: bool,
) -> String {
    let mut b = String::new();
    let Some(e) = enemy else {
        b += &text(72, 62, 24, "#8b949e", &format!("Enemy {}", slot + 1));
        b += &text(72, 92, 16, "#6e7681", "no game");
        if edit_mode {
            b += &format!(r#"<rect x="3" y="3" width="138" height="138" fill="none" stroke="{GOLD}" stroke-width="6"/>"#);
        }
        return svg_url(&b);
    };

    if e.champ_icon.is_empty() {
        b += &text(72, 46, 20, "#ffffff", &e.champ);
    } else {
        b += &image(38, 2, 68, &e.champ_icon);
    }
    if insight {
        b += &format!(r#"<rect x="2" y="2" width="32" height="20" rx="4" fill="{PURPLE}"/>"#);
        b += &text(18, 17, 14, "#ffffff", "CI");
    }
    match e.boots {
        Boots::Lucidity => {
            b += r##"<rect x="110" y="2" width="32" height="20" rx="4" fill="#2f81f7"/>"##;
            b += &text(126, 17, 14, "#ffffff", "B");
        }
        Boots::Crimson => {
            b += r##"<rect x="106" y="2" width="36" height="20" rx="4" fill="#c0392b"/>"##;
            b += &text(124, 17, 14, "#ffffff", "B+");
        }
        Boots::None => {}
    }

    let mut shown: Vec<usize> = order.into_iter().filter(|i| !hidden[*i]).collect();
    if shown.is_empty() {
        shown = order.to_vec();
    }
    let single = shown.len() == 1;
    for (pos, &idx) in shown.iter().enumerate() {
        let (spell, state) = (&e.spells[idx], states[idx]);
        let x0 = if single { 36 } else { pos as i32 * 72 };
        if state == SpellState::Fresh {
            b += &format!(r#"<rect x="{}" y="72" width="70" height="72" rx="6" fill="{GREEN}"/>"#, x0 + 1);
        }
        if spell.icon.is_empty() {
            b += &text(x0 + 36, 100, 12, "#c9d1d9", &short_name(&spell.name));
        } else {
            b += &image(x0 + 17, 76, 38, &spell.icon);
        }
        match state {
            SpellState::Running { remaining, .. } => {
                // Dim the icon while the spell is down.
                b += &format!(r##"<rect x="{}" y="76" width="38" height="38" fill="#000000" fill-opacity="0.65"/>"##, x0 + 17);
                let color = if remaining <= 10.0 { GOLD } else { "#ffffff" };
                b += &text(x0 + 36, 138, 22, color, &fmt_time(remaining));
            }
            SpellState::Fresh => b += &text(x0 + 36, 138, 22, "#ffffff", "UP"),
            SpellState::Ready => {}
        }
    }
    // Quest Teleport: small icon left of the champion (long press to start).
    if let Some(q) = &e.quest_tp {
        let state = states[crate::game::QUEST_TP];
        if state == SpellState::Fresh {
            b += &format!(r#"<rect x="2" y="24" width="34" height="46" rx="4" fill="{GREEN}"/>"#);
        }
        if q.icon.is_empty() {
            b += &text(19, 46, 12, "#c9d1d9", "TP");
        } else {
            b += &image(5, 26, 28, &q.icon);
        }
        match state {
            SpellState::Running { remaining, .. } => {
                b += r##"<rect x="5" y="26" width="28" height="28" fill="#000000" fill-opacity="0.65"/>"##;
                let color = if remaining <= 10.0 { GOLD } else { "#ffffff" };
                b += &text(19, 67, 11, color, &fmt_time(remaining));
            }
            SpellState::Fresh => b += &text(19, 67, 11, "#ffffff", "UP"),
            SpellState::Ready => {}
        }
    }
    if edit_mode {
        b += &format!(r#"<rect x="3" y="3" width="138" height="138" fill="none" stroke="{GOLD}" stroke-width="6"/>"#);
    }
    svg_url(&b)
}

pub fn insight_key(edit_mode: bool) -> String {
    let mut b = String::new();
    if edit_mode {
        b += &format!(r#"<rect width="144" height="144" fill="{PURPLE}"/>"#);
    }
    b += &text(72, 50, 22, "#ffffff", "COSMIC");
    b += &text(72, 78, 22, "#ffffff", "INSIGHT");
    b += &text(72, 116, 16, if edit_mode { GOLD } else { "#8b949e" }, if edit_mode { "tap enemy" } else { "edit" });
    svg_url(&b)
}

pub fn reset_key() -> String {
    let mut b = String::new();
    b += &text(72, 66, 28, "#ffffff", "UNDO");
    b += &text(72, 100, 14, "#8b949e", "hold: reset all");
    svg_url(&b)
}

/// Timer on a normal key (devices without dials): Nth soonest running timer.
pub fn timer_key(entry: Option<(&Enemy, usize, f64, f64)>, rank: usize) -> String {
    let mut b = String::new();
    let Some((e, idx, remaining, total)) = entry else {
        b += &text(72, 62, 20, "#6e7681", &format!("#{}", rank + 1));
        b += &text(72, 92, 16, "#6e7681", "no timer");
        return svg_url(&b);
    };
    let spell = e.spell(idx);
    if e.champ_icon.is_empty() {
        b += &text(72, 40, 16, "#ffffff", &short_name(&e.champ));
    } else {
        b += &image(8, 6, 64, &e.champ_icon);
    }
    if spell.icon.is_empty() {
        b += &text(106, 44, 12, "#c9d1d9", &short_name(&spell.name));
    } else {
        b += &image(80, 14, 50, &spell.icon);
    }
    let color = if remaining <= 10.0 { GOLD } else { "#ffffff" };
    b += &text(72, 112, 30, color, &fmt_time(remaining));
    // Progress bar: drains as the spell comes back.
    let w = if total > 0.0 { (remaining / total * 128.0).clamp(0.0, 128.0) } else { 0.0 };
    b += r##"<rect x="8" y="124" width="128" height="10" rx="3" fill="#333333"/>"##;
    b += &format!(r#"<rect x="8" y="124" width="{w:.0}" height="10" rx="3" fill="{GOLD}"/>"#);
    svg_url(&b)
}

/// Touch strip segment for one running timer, or empty.
pub fn dial(entry: Option<(&Enemy, usize, f64, f64)>) -> Value {
    match entry {
        Some((e, idx, remaining, total)) => {
            let spell = e.spell(idx);
            let pct = if total > 0.0 { (remaining / total * 100.0).clamp(0.0, 100.0).round() } else { 0.0 };
            json!({
                "champ": if e.champ_icon.is_empty() { EMPTY_PNG } else { &e.champ_icon },
                "spell": if spell.icon.is_empty() { EMPTY_PNG } else { &spell.icon },
                "name": format!("{} {}", e.champ, spell.name),
                "time": fmt_time(remaining),
                "bar": { "value": pct },
            })
        }
        None => json!({
            "champ": EMPTY_PNG,
            "spell": EMPTY_PNG,
            "name": "no timer",
            "time": "",
            "bar": { "value": 0 },
        }),
    }
}
