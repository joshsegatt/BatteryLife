//! Command line for the core.
//!
//!   batterylife-core [--list]         print every device once, as a table
//!   batterylife-core --json           print every device once, as a JSON array
//!   batterylife-core --watch [secs]   keep polling (default every 60 s), one JSON array per line;
//!                                     a "refresh" line on stdin polls now, closing stdin exits

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use batterylife_core::{Device, Tracker, poll_all};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("--list") => print_table(&poll_all()),
        Some("--json") => println!("{}", to_json(&poll_all())),
        Some("--watch") => {
            let secs = match args.get(1).map(|s| s.parse::<u64>()) {
                None => 60,
                Some(Ok(s)) if s >= 5 => s,
                _ => fail("--watch takes an interval of at least 5 seconds"),
            };
            watch(Duration::from_secs(secs));
        }
        Some(other) => fail(&format!(
            "unknown option {other}; use --list, --json or --watch [secs]"
        )),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn to_json(devices: &[Device]) -> String {
    serde_json::to_string(devices).expect("devices serialize")
}

enum Command {
    Refresh,
    Quit,
}

fn watch(interval: Duration) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) if l.trim() == "refresh" => {
                    let _ = tx.send(Command::Refresh);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // The parent closed our stdin (or exited): stop instead of polling forever as an orphan.
        let _ = tx.send(Command::Quit);
    });

    let mut tracker = Tracker::default();
    let mut out = std::io::stdout().lock();
    loop {
        let devices = tracker.update(poll_all(), Instant::now());
        if writeln!(out, "{}", to_json(&devices))
            .and_then(|_| out.flush())
            .is_err()
        {
            return; // nobody is reading anymore
        }
        match rx.recv_timeout(interval) {
            Ok(Command::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Command::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn print_table(devices: &[Device]) {
    if devices.is_empty() {
        println!("No devices found.");
    }
    for d in devices {
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
