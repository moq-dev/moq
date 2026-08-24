//! `moq-token`: generate, sign, and verify tokens for moq-relay.
//!
//! The command surface lives in this crate's library, so this binary and the
//! `moq token` subcommand of `moq-cli` stay the same tool.

fn main() -> anyhow::Result<()> {
	moq_token_cli::Args::parse().run()
}
