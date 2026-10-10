fn main() {
    let result = (|| -> Result<(), String> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 1 && !(args.len() == 3 && args[1] == "--config-plan") {
            return Err(
                "usage: homesense-import <project.c4p|project.xml> [--config-plan plan.json]"
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
        let output = if args.len() == 3 {
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
