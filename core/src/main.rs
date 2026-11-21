//! Command line for the core: `batterylife-core --list` (default) or `--json`.

fn main() {
    let devices = batterylife_core::poll_all();
    match std::env::args().nth(1).as_deref() {
        Some("--json") => println!(
            "{}",
            serde_json::to_string(&devices).expect("devices serialize")
        ),
        None | Some("--list") => {
            if devices.is_empty() {
                println!("No devices found.");
            }
            for d in &devices {
                let level = d.percent.map_or("?".to_string(), |p| format!("{p}%"));
                let mut state = Vec::new();
                if d.charging {
                    state.push(if d.full { "full" } else { "charging" });
                }
                if d.asleep {
                    state.push("last known");
                }
                let state = if state.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", state.join(", "))
                };
                println!(
                    "{:<8} {:<32} {:>4}{}  via {}",
                    format!("{:?}", d.kind),
                    d.name,
                    level,
                    state,
                    d.via
                );
            }
        }
        Some(other) => {
            eprintln!("Unknown option {other}. Use --list or --json.");
            std::process::exit(2);
        }
    }
}
