//! `cargo run --bin upload-release -- [--environment staging|production] [--bundles <folder>]
//! [--packages <folder>]` (installers/03-the-bundle-agent-bundle.md section 3.16): see `release_upload`.
use std::path::PathBuf;

use retaskable_agent::release_check::{self, arg};
use retaskable_agent::release_upload::{self, R2};

fn main() {
    release_check::run_main(run);
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let environment = release_check::environment_arg(&args)?;
    let bundles = arg(&args, "--bundles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/agent"));
    let packages = arg(&args, "--packages")
        .map(PathBuf::from)
        .unwrap_or_else(|| bundles.join("packages"));
    let (uploads, skipped) = release_upload::release_uploads(environment, &bundles, &packages)?;
    for s in &skipped {
        println!("Skipped {s}");
    }
    if uploads.is_empty() {
        return Err("Nothing to upload: run bundle and package first".to_string());
    }
    let r2 = R2::from_env(environment, |k| std::env::var(k).ok())?;
    release_upload::publish(&uploads, &r2)?;
    for u in &uploads {
        println!("Published {} to {}: {}", u.target, r2.bucket(), u.entry["url"]);
    }
    Ok(())
}
