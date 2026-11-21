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
    /// How long the last known level stays visible after the device stops answering.
    #[serde(skip)]
    pub grace: Duration,
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
            grace: Duration::ZERO,
        }
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
