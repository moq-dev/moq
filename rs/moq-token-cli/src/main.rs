use anyhow::Context;
use clap::{Parser, Subcommand};
use moq_token::Algorithm;
use std::{io, path::PathBuf};

#[derive(Debug, Parser)]
#[command(name = "moq-token")]
#[command(about = "Generate, sign, and verify tokens for moq-relay", long_about = None)]
struct Cli {
	/// The path for the key.
	#[arg(long)]
	key: PathBuf,

	/// The command to execute.
	#[command(subcommand)]
	command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
	/// Generate a key (pair) for the given algorithm.
	///
	/// The key is output to the provided -key path.
	Generate {
		/// The algorithm to use.
		#[arg(long, default_value = "HS256")]
		algorithm: Algorithm,

		/// An optional key ID, useful for rotating keys.
		#[arg(long)]
		id: Option<String>,

		/// Optional path to save the public key (for asymmetric algorithms).
		#[arg(long)]
		public: Option<PathBuf>,
	},

	/// Sign a token to stdout, reading the key from stdin.
	Sign {
		/// The root path for the token.
		/// The user MUST connect to this WebTransport path and any broadcasts are relative to it.
		/// Any trailing/leading slashes are ignored.
		#[arg(long, default_value = "")]
		root: String,

		/// If specified, the user can publish any matching path prefixes.
		/// If not specified, the user will not publish any broadcasts.
		/// This can be specified multiple times to publish multiple paths.
		#[arg(long)]
		publish: Vec<String>,

		/// If true, then this client is considered a cluster node.
		/// Both the client and server will only announce broadcasts from non-cluster clients.
		/// This avoids convoluted routing, as only the primary origin will announce.
		#[arg(long)]
		cluster: bool,

		/// If specified, the user can subscribe to any matching path prefixes.
		/// If not specified, the user will not receive announcements and cannot subscribe to any broadcasts.
		/// This can be specified multiple times to subscribe to multiple paths.
		#[arg(long)]
		subscribe: Vec<String>,

		/// The expiration time of the token as a unix timestamp.
		#[arg(long, value_parser = parse_unix_timestamp)]
		expires: Option<std::time::SystemTime>,

		/// The issued time of the token as a unix timestamp.
		#[arg(long, value_parser = parse_unix_timestamp)]
		issued: Option<std::time::SystemTime>,
	},

	/// Verify a token from stdin, writing the payload to stdout.
	/// NOTE: You still need to verify that the path is valid for the token.
	/// This just verifies the signature.
	Verify,
}

fn main() -> anyhow::Result<()> {
	let cli = Cli::parse();

	match cli.command {
		Commands::Generate { algorithm, id, public } => {
			let key = moq_token::Key::generate(algorithm, id)?;

			if let Some(public) = public {
				key.to_public()?.to_file(public)?;
			}

			key.to_file(&cli.key)?;
		}

		Commands::Sign {
			root,
			publish,
			cluster,
			subscribe,
			expires,
			issued,
		} => {
			let key = moq_token::Key::from_file(cli.key)?;

			let payload = moq_token::Claims {
				root,
				publish,
				cluster,
				subscribe,
				expires,
				issued,
			};

			let token = key.encode(&payload)?;
			println!("{token}");
		}

		Commands::Verify => {
			let key = moq_token::Key::from_file(cli.key)?;
			let token = io::read_to_string(io::stdin())?.trim().to_string();
			let payload = key.decode(&token)?;

			println!("{payload:#?}");
		}
	}

	Ok(())
}

// A simpler parser for clap
fn parse_unix_timestamp(s: &str) -> anyhow::Result<std::time::SystemTime> {
	let timestamp = s.parse::<i64>().context("expected unix timestamp")?;
	let timestamp = timestamp.try_into().context("timestamp out of range")?;
	Ok(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp))
}
