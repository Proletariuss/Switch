//! Rebuild the carrier registry or synthetic selection cases from preserved YAML.
//! Usage: carrier_export --output PATH [--cases] [--data-dir PATH] [--revision SHA]
//! No network, hardware, Go process, or existing generated JSON is consulted.
#[path = "carrier_export/support.rs"]
mod support;

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let mut output = None;
    let mut cases = false;
    let mut data_dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../data/carriers"
    ));
    let mut revision = support::PINNED_REVISION.to_owned();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--output" | "-output" => {
                output = Some(PathBuf::from(
                    arguments.next().ok_or("missing output path")?,
                ));
            }
            "--cases" | "-cases" => cases = true,
            "--data-dir" => {
                data_dir = PathBuf::from(arguments.next().ok_or("missing data directory")?)
            }
            "--revision" => revision = arguments.next().ok_or("missing source revision")?,
            _ => return Err(format!("unknown argument {argument}")),
        }
    }
    let output = output.ok_or("--output is required")?;
    let registry = support::rebuild_registry(&data_dir, &revision)?;
    let document = if cases {
        support::rebuild_selection_cases(&registry)?
    } else {
        registry
    };
    // Validate the entire input before opening/truncating the requested output.
    let encoded = support::encode(&document)?;
    std::fs::write(&output, encoded).map_err(|error| format!("{}: {error}", output.display()))?;
    println!("wrote {}", output.display());
    Ok(())
}
