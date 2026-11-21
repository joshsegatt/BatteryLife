//! Phone battery from Microsoft Phone Link.
//!
//! Phone Link writes an Adaptive Card for the Start menu companion with the connected phone's name and
//! battery. The file is an undocumented implementation detail and may change with Phone Link updates.
//! Read-only, no device or network access.
//!
//! Ported from BatteryHub (src/drivers/phonelink.cpp, phonelink_protocol.h, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0. Rewritten in Rust on serde_json.

use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::device::{Device, Kind};

/// A card not rewritten for this long is shown as a last known value.
const STALE: Duration = Duration::from_secs(3 * 60 * 60);
const MAX_FILE: u64 = 4 * 1024 * 1024;
const CONNECTED: &str = " - Connected to your PC";

#[derive(Debug, PartialEq)]
pub struct Reading {
    pub name: String,
    pub percent: u8,
    pub charging: bool,
}

pub fn poll() -> Option<Device> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let path = std::path::Path::new(&base).join(
        r"Packages\Microsoft.YourPhone_8wekyb3d8bbwe\LocalState\StartMenu\StartMenuCompanion.json",
    );
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() == 0 || meta.len() > MAX_FILE {
        return None;
    }
    let reading = parse(&std::fs::read_to_string(&path).ok()?)?;
    let age = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or_default();

    let mut d = Device::new("phonelink", reading.name, Kind::Phone, "Phone Link");
    d.percent = Some(reading.percent);
    d.charging = reading.charging;
    d.asleep = age > STALE;
    Some(d)
}

/// Parses the card. Returns None when the phone is not connected, the JSON is malformed, or the
/// battery readings disagree with each other.
pub fn parse(json: &str) -> Option<Reading> {
    let root: Value = serde_json::from_str(json).ok()?;
    let name = root.get("speak")?.as_str()?.strip_suffix(CONNECTED)?;
    if name.is_empty() {
        return None;
    }
    let mut found: Option<(u8, bool)> = None;
    let mut ambiguous = false;
    walk(&root, &mut found, &mut ambiguous);
    let (percent, charging) = found.filter(|_| !ambiguous)?;
    Some(Reading {
        name: name.to_string(),
        percent,
        charging,
    })
}

/// Only follows card layout fields (body, items, columns), never actions or other payloads.
fn walk(v: &Value, found: &mut Option<(u8, bool)>, ambiguous: &mut bool) {
    match v {
        Value::Array(items) => items.iter().for_each(|c| walk(c, found, ambiguous)),
        Value::Object(o) => {
            let layout = [
                "AdaptiveCard",
                "Container",
                "ColumnSet",
                "Column",
                "TextBlock",
                "Image",
            ];
            if let Some(t) = o.get("type").and_then(Value::as_str)
                && !layout.contains(&t)
            {
                return;
            }
            if let Some(r) = o
                .get("tooltip")
                .and_then(Value::as_str)
                .and_then(battery_from_tooltip)
            {
                match *found {
                    Some(prev) if prev != r => *ambiguous = true,
                    _ => *found = Some(r),
                }
            }
            for key in ["body", "items", "columns"] {
                if let Some(c) = o.get(key) {
                    walk(c, found, ambiguous);
                }
            }
        }
        _ => {}
    }
}

/// "Battery 84% remaining" or "Battery 84% remaining, charging".
fn battery_from_tooltip(t: &str) -> Option<(u8, bool)> {
    let rest = t.strip_prefix("Battery ")?;
    let (num, tail) = rest.split_once('%')?;
    if num.is_empty() || num.len() > 3 || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pct: u8 = num.parse().ok().filter(|p| *p <= 100)?;
    match tail {
        " remaining" => Some((pct, false)),
        " remaining, charging" => Some((pct, true)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(speak: &str, tooltips: &[&str]) -> String {
        let items: Vec<String> = tooltips
            .iter()
            .map(|t| format!(r#"{{"type":"Image","tooltip":"{t}"}}"#))
            .collect();
        format!(
            r#"{{"type":"AdaptiveCard","speak":"{speak}","body":[{{"type":"Container","items":[{}]}}]}}"#,
            items.join(",")
        )
    }

    #[test]
    fn reads_connected_phone() {
        let r = parse(&card(
            "X9 Ultra Josh - Connected to your PC",
            &["Connected to your PC", "Battery 84% remaining"],
        ));
        assert_eq!(
            r,
            Some(Reading {
                name: "X9 Ultra Josh".into(),
                percent: 84,
                charging: false
            })
        );
    }

    #[test]
    fn reads_charging() {
        let r = parse(&card(
            "Phone - Connected to your PC",
            &["Battery 7% remaining, charging"],
        ))
        .unwrap();
        assert_eq!((r.percent, r.charging), (7, true));
    }

    #[test]
    fn rejects_disconnected_or_bad_data() {
        assert_eq!(
            parse(&card("Phone - Not connected", &["Battery 84% remaining"])),
            None
        );
        assert_eq!(
            parse(&card(
                "Phone - Connected to your PC",
                &["Battery 140% remaining"]
            )),
            None
        );
        assert_eq!(parse(&card("Phone - Connected to your PC", &[])), None);
        assert_eq!(parse("{not json"), None);
    }

    #[test]
    fn rejects_conflicting_readings() {
        let r = parse(&card(
            "Phone - Connected to your PC",
            &["Battery 84% remaining", "Battery 20% remaining"],
        ));
        assert_eq!(r, None);
    }

    #[test]
    fn ignores_battery_text_inside_actions() {
        let json = r#"{"type":"AdaptiveCard","speak":"P - Connected to your PC","body":[
            {"type":"Action.OpenUrl","tooltip":"Battery 50% remaining"}]}"#;
        assert_eq!(parse(json), None);
    }
}
