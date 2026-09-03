use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Validate the quest tree: the interlinked Markdown plans under quest/, whose
/// contract is quest/AGENTS.md.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
	/// Repository root holding the quest/ directory.
	#[arg(long, default_value = ".", global = true)]
	root: PathBuf,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	/// Report every structural mistake in the tree; exit non-zero if any.
	Check,
}

fn main() -> Result<ExitCode> {
	let cli = Cli::parse();
	match cli.command {
		Command::Check => {
			let findings = quest::check(&cli.root)?;
			if findings.is_empty() {
				let total = quest::collect(&cli.root)?.len();
				println!("quest: {total} documents ok");
				return Ok(ExitCode::SUCCESS);
			}
			for finding in &findings {
				eprintln!("quest: {finding}");
			}
			Ok(ExitCode::FAILURE)
		}
	}
}
