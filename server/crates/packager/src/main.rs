use anyhow::Result;
use clap::{Parser, Subcommand};
use launcher_packager::{PackageOptions, package_directory};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "launcher-packager",
    about = "Package an authorized build into content-addressed chunks"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Package {
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Package {
            input,
            output,
            game_id,
            build_id,
            display_version,
            executable,
        } => {
            let options = PackageOptions {
                game_id,
                build_id,
                display_version,
                executable,
                ..PackageOptions::default()
            };
            let report = package_directory(input, output, &options)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parser_requires_output() {
        assert!(Cli::try_parse_from(["launcher-packager", "package", "input"]).is_err());
    }
}
