fn main() {
    let result = (|| -> Result<(), String> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 1
            && !(args.len() == 3 && args[1] == "--room")
            && !((args.len() == 3 || (args.len() == 5 && args[3] == "--export-dir"))
                && args[1] == "--config-plan")
        {
            return Err(
                "usage: homesense-import <project.c4p|project.xml> [--room ROOM | --config-plan plan.json [--export-dir NEW_DIRECTORY]]"
                    .into(),
            );
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&args[0])
            .map_err(|e| e.to_string())?
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let report = homesense_import::import_bytes(&bytes)?;
        let output = if args.len() == 3 && args[1] == "--room" {
            homesense_import::room::blockers(
                &report,
                args[2].to_str().ok_or("room must be UTF-8")?,
            )?
        } else if args.len() >= 3 {
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])
                .map_err(|e| e.to_string())?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 1024 * 1024 {
                return Err("plan exceeds 1 MiB".into());
            }
            let plan = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            serde_json::to_value(homesense_import::configuration::generate(&report, plan)?)
                .map_err(|e| e.to_string())?
        } else {
            serde_json::to_value(&report).map_err(|e| e.to_string())?
        };
        if args.len() == 5 {
            let directory = std::path::Path::new(&args[4]);
            // Refuse existing destinations, including symlinks; never overwrite evidence.
            let files = [
                ("configuration.json", serde_json::to_vec_pretty(&output["configuration"])),
                ("coverage-report.json", serde_json::to_vec_pretty(&report)),
                ("confirmation-checklist.json", serde_json::to_vec_pretty(&serde_json::json!({
                    "source":report.source,
                    "selected_disabled":output["selected_disabled"],
                    "excluded":output["excluded"],
                    "profile_basis":output["profile_basis"],
                    "devices":report.devices,
                    "required_checks":["Confirm room, load, address and channel for selected devices", "Verify module model and protocol profile", "Verify motor direction and STOP before enabling", "Verify LED mode before writes"],
                    "all_selected_devices_disabled":true,
                    "automatic_enable":false
                }))),
            ].into_iter().map(|(name, bytes)| bytes.map(|bytes| (name,bytes)).map_err(|e|e.to_string())).collect::<Result<Vec<_>,_>>()?;
            std::fs::create_dir(directory)
                .map_err(|e| format!("new export directory required: {e}"))?;
            for (name, bytes) in files {
                std::fs::write(directory.join(name), bytes)
                    .map_err(|e| format!("incomplete export at {}: {e}", directory.display()))?;
            }
            return Ok(());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
        );
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
