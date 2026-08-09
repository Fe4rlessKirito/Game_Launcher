use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use launcher_domain::BuildState;
use launcher_manifests::validate_json;
use launcher_packager::{PackageOptions, package_directory};
use launcher_worker::IngestionProgress;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Parser)]
#[command(
    name = "launcher-admin",
    about = "Safe operator commands for authorized launcher content"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Ingest {
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "synthetic-game")]
        game_id: String,
        #[arg(long, default_value = "build-local")]
        build_id: String,
        #[arg(long, default_value = "0.1.0")]
        display_version: String,
        #[arg(long)]
        executable: Option<String>,
    },
    ManifestVerify {
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::ManifestVerify { path } => {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            let manifest = validate_json(&bytes).map_err(|error| anyhow::anyhow!(error))?;
            println!(
                "manifest={} game={} build={} files={} status=VALID",
                manifest.manifest_id,
                manifest.game_id,
                manifest.build_id,
                manifest.files.len()
            );
        }
        Commands::Ingest {
            input,
            output,
            game_id,
            build_id,
            display_version,
            executable,
        } => {
            let mut progress = IngestionProgress::new();
            println!("stage={:?}", progress.state);
            let analysis_path = output.join("analysis.json");
            std::fs::create_dir_all(&output)?;
            let status = Command::new("python")
                .args(["-m", "launcher_analyzer", "analyze"])
                .arg(&input)
                .args(["--output"])
                .arg(&analysis_path)
                .status()
                .context("could not start Python analyzer")?;
            if !status.success() {
                anyhow::bail!("analyzer failed with status {status}");
            }
            progress.advance(BuildState::Analyzed)?;
            println!(
                "stage={:?} report={}",
                progress.state,
                analysis_path.display()
            );
            let report = package_directory(
                &input,
                &output,
                &PackageOptions {
                    game_id,
                    build_id,
                    display_version,
                    executable,
                    ..PackageOptions::default()
                },
            )?;
            progress.advance(BuildState::Packaged)?;
            progress.advance(BuildState::Uploaded)?;
            progress.advance(BuildState::Verified)?;
            progress.advance(BuildState::Ready)?;
            println!(
                "stage={:?} report={}",
                progress.state,
                serde_json::to_string_pretty(&report)?
            );
            println!("publication=EXPLICIT_OPERATOR_ACTION_REQUIRED");
        }
    }
    Ok(())
}
