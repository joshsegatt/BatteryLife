use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Other,
    Mouse,
    Headset,
    Keyboard,
    Gamepad,
    Phone,
}

/// One battery-powered device as seen in a single poll.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Device {
    /// Stable identity across polls.
    pub key: String,
    pub name: String,
    pub kind: Kind,
    /// "Logitech receiver", "Phone Link", ...
    pub via: String,
    /// 0..=100, or None when the level is unknown right now.
    pub percent: Option<u8>,
    pub charging: bool,
    /// On power and fully charged.
    pub full: bool,
    /// Last known value, the device is not answering right now.
    pub asleep: bool,
    /// Battery voltage when the device reports it.
    pub millivolts: Option<u16>,
    /// Estimated minutes until empty, from the drain observed while discharging. None until there
    /// is enough history (at least 20 minutes and a 2-point drop).
    pub minutes_left: Option<u32>,
    /// The level is a coarse step (e.g. 100/60/20/5 from level flags), not a measured percentage:
    /// such steps are too big to estimate time left from.
    #[serde(skip)]
    pub coarse: bool,
    /// How long the last known level stays visible after the device stops answering.
    #[serde(skip)]
    pub grace: Duration,
    /// Its receiver says the device is switched off or out of range (BatteryHub's
    /// `connected = false`): it is hidden at once instead of shown asleep with its last level.
    #[serde(skip)]
    pub off: bool,
}

impl Device {
    pub fn new(
        key: impl Into<String>,
        name: impl Into<String>,
        kind: Kind,
        via: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            kind,
            via: via.into(),
            percent: None,
            charging: false,
            full: false,
            asleep: false,
            millivolts: None,
            minutes_left: None,
            coarse: false,
            grace: Duration::ZERO,
            off: false,
        }
    }

    /// The device `key` as its receiver reports it switched off or out of range.
    pub fn switched_off(
        key: impl Into<String>,
        name: impl Into<String>,
        kind: Kind,
        via: impl Into<String>,
    ) -> Self {
        let mut d = Self::new(key, name, kind, via);
        d.off = true;
        d
    }
}

/// Device kind from a product name, for sources that report no type.
pub fn guess_kind(name: &str) -> Kind {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&[
        "mouse",
        "superlight",
        "viper",
        "deathadder",
        "basilisk",
        "pulsefire",
        "rival",
        "aerox",
    ]) {
        Kind::Mouse
    } else if has(&[
        "headset",
        "headphone",
        "buds",
        "earbuds",
        "airpods",
        "cloud",
        "arctis",
        "kraken",
        "g435",
        "g733",
    ]) {
        Kind::Headset
    } else if has(&["keyboard", "keys", "keychron"]) {
        Kind::Keyboard
    } else if has(&[
        "controller",
        "gamepad",
        "dualsense",
        "dualshock",
        "joy-con",
        "xbox",
    ]) {
        Kind::Gamepad
    } else if has(&["phone", "iphone", "galaxy s", "pixel"]) {
        Kind::Phone
    } else {
        Kind::Other
    }
}

/// Piecewise-linear interpolation through (mv[i], pct[i]); tables sorted by descending voltage.
pub fn interpolate(mv: i32, mv_table: &[i32], pct_table: &[i32]) -> u8 {
    let n = mv_table.len();
    if mv >= mv_table[0] {
        return pct_table[0] as u8;
    }
    if mv <= mv_table[n - 1] {
        return pct_table[n - 1] as u8;
    }
    for i in 1..n {
        if mv >= mv_table[i] {
            let (v0, v1, p0, p1) = (mv_table[i - 1], mv_table[i], pct_table[i - 1], pct_table[i]);
            return (p1 + (mv - v1) * (p0 - p1) / (v0 - v1)) as u8;
        }
    }
    pct_table[n - 1] as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_clamps_and_maps_linearly() {
        let v = [4200, 3700, 3500];
        let p = [100, 50, 0];
        assert_eq!(interpolate(4300, &v, &p), 100);
        assert_eq!(interpolate(3400, &v, &p), 0);
        assert_eq!(interpolate(3950, &v, &p), 75);
        assert_eq!(interpolate(3600, &v, &p), 25);
    }
}
