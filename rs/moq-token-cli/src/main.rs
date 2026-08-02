//! `moq-token`: generate, sign, and verify tokens for moq-relay.
//!
//! The command surface lives in this crate's library, so this binary and the
//! `moq token` subcommand of `moq-cli` stay the same tool.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "moq-token")]
#[command(about = "Generate, sign, and verify tokens for moq-relay", long_about = None)]
#[command(version = env!("VERSION"))]
struct Cli {
	#[command(flatten)]
	token: moq_token_cli::Args,
}

fn main() -> anyhow::Result<()> {
	Cli::parse().token.run()
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::CommandFactory;

	// The shared `Args` is flattened here and nested under `moq token` there, so a
	// clap-level conflict only shows up in one of the two. This is that check.
	#[test]
	fn valid() {
		Cli::command().debug_assert();
	}
}
