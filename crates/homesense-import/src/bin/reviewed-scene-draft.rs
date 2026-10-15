//! Offline only: print disabled strict scene drafts plus full local provenance.
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn read(path: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err("input must be a file of at most 32 MiB".into());
    }
    std::fs::read(path).map_err(|e| e.to_string())
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 && args.len() != 4 {
        return Err("usage: reviewed-scene-draft PROJECT.xml CONFIG.json REVIEW.json [EXACT_EVENT.xml]; output contains private source XML: keep local".into());
    }
    let project = String::from_utf8(read(&args[0])?).map_err(|e| e.to_string())?;
    let config = homesense_config::Configuration::parse(
        &read(&args[1])?,
        homesense_core::ExecutionContext::Simulation,
    )
    .map_err(|e| e.to_string())?;
    let review = serde_json::from_slice(&read(&args[2])?).map_err(|e| e.to_string())?;
    let bundle = homesense_import::reviewed_scenes::convert(&project, &config, &review)?;
    let result = if args.len() == 4 {
        let event = String::from_utf8(read(&args[3])?).map_err(|e| e.to_string())?;
        homesense_import::reviewed_scenes::link_event(&project, &event, &config, &review, &bundle)?
    } else {
        serde_json::to_value(bundle).map_err(|e| e.to_string())?
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
    );
    Ok(())
}
