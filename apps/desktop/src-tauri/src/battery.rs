//! Battery + power telemetry for the HUD. Uses the cross-platform `battery`
//! crate; returns `charging = None` / `percent = None` on desktops without a
//! battery so the front-end can hide the widget.

use serde::Serialize;

#[derive(Serialize, Default)]
pub struct BatteryInfo {
    /// Charge as a percentage 0..100, if a battery is present.
    pub percent: Option<f32>,
    /// Seconds until empty (discharging) or full (charging), if known.
    pub seconds_remaining: Option<u64>,
    pub charging: Option<bool>,
    /// Human-readable state: "charging" | "discharging" | "full" | "unknown".
    pub state: String,
    pub present: bool,
}

#[tauri::command]
pub fn battery_status() -> BatteryInfo {
    match read_battery() {
        Ok(Some(info)) => info,
        _ => BatteryInfo {
            state: "none".into(),
            present: false,
            ..Default::default()
        },
    }
}

fn read_battery() -> Result<Option<BatteryInfo>, battery::Error> {
    let manager = battery::Manager::new()?;
    let mut batteries = manager.batteries()?;
    let Some(battery) = batteries.next() else {
        return Ok(None);
    };
    let battery = battery?;

    use battery::State;
    let charging = matches!(battery.state(), State::Charging);
    let state = match battery.state() {
        State::Charging => "charging",
        State::Discharging => "discharging",
        State::Full => "full",
        State::Empty => "empty",
        _ => "unknown",
    };
    let percent = battery.state_of_charge().value * 100.0;
    let remaining = match battery.state() {
        State::Charging => battery.time_to_full(),
        _ => battery.time_to_empty(),
    }
    .map(|t| t.value as u64);

    Ok(Some(BatteryInfo {
        percent: Some(percent),
        seconds_remaining: remaining,
        charging: Some(charging),
        state: state.into(),
        present: true,
    }))
}
