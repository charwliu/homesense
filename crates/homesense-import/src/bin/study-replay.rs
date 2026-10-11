//! Offline replay only: deliberately has no transport dependency.
use homesense_import::study::{Action, Evaluation, Rule, evaluate, targets};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    use std::io::Read;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: study-replay MIGRATION.json KEY MODE|unknown".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&args[0])
        .map_err(|e| e.to_string())?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("migration exceeds 1 MiB".into());
    }
    let d: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if d["automatic_execution"] != false {
        return Err("offline migration must disable automatic execution".into());
    }
    let rules: Vec<Rule> = serde_json::from_value(d["rules"].clone()).map_err(|e| e.to_string())?;
    let mode = if args[2] == "unknown" {
        None
    } else {
        Some(args[2].parse().map_err(|_| "invalid mode")?)
    };
    let result = evaluate(
        &rules,
        args[1].parse().map_err(|_| "invalid key")?,
        true,
        mode,
    )?;
    let output = match result {
        Evaluation::Unknown => serde_json::json!({"status":"unknown_skipped","bus_writes":0}),
        Evaluation::Ignored => serde_json::json!({"status":"ignored","bus_writes":0}),
        Evaluation::Planned { next_mode, actions } => {
            let mut steps = Vec::new();
            for action in actions {
                let members = if let Action::Scene {
                    scene_id, activate, ..
                } = &action
                {
                    serde_json::to_value(targets(&d, scene_id, *activate)?)
                        .map_err(|e| e.to_string())?
                } else {
                    serde_json::Value::Null
                };
                steps.push(serde_json::json!({"action":action,"members":members}));
            }
            serde_json::json!({"status":"offline_plan","next_mode":next_mode,"steps":steps,"bus_writes":0,"physical_state_confirmed":false,"timing_policy":"source units retained; live timing adapter pending"})
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
    );
    Ok(())
}
