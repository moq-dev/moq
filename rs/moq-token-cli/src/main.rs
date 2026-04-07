use anyhow::Context;
use clap::{Parser, Subcommand};
use moq_token::Algorithm;
use std::{io, path::PathBuf};

#[derive(Debug, Parser)]
#[command(name = "moq-token")]
#[command(about = "Generate, sign, and verify tokens for moq-relay", long_about = None)]
#[command(version = env!("VERSION"))]
struct Cli {
	/// The command to execute.
	#[command(subcommand)]
	command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
	/// Generate a new signing key.
	///
	/// A random key ID is assigned unless --id is specified.
	/// Output is JSON by default; use --base64 for base64url encoding.
	Generate {
		/// The algorithm to use.
		#[arg(long, default_value = "HS256")]
		algorithm: Algorithm,

		/// The key ID. Randomly generated if not provided.
		#[arg(long)]
		id: Option<String>,

		/// Write the key to a file path.
		#[arg(long)]
		out: Option<PathBuf>,

		/// Write the key to a directory as {kid}.jwk.
		#[arg(long, conflicts_with = "out")]
		out_dir: Option<PathBuf>,

		/// Write the public key to a file path (asymmetric algorithms only).
		#[arg(long)]
		public: Option<PathBuf>,

		/// Write the public key to a directory as {kid}.jwk (asymmetric algorithms only).
		#[arg(long, conflicts_with = "public")]
		public_dir: Option<PathBuf>,

		/// Output as base64url instead of JSON.
		#[arg(long)]
		base64: bool,
	},

	/// Sign a token, writing it to stdout.
	Sign {
		/// Path to the signing key file.
		#[arg(long)]
		key: PathBuf,

		/// The root path for the token.
		#[arg(long, default_value = "")]
		root: String,

		/// Paths the user can publish to (repeatable).
		#[arg(long)]
		publish: Vec<String>,

		/// Mark this token as a cluster node.
		#[arg(long)]
		cluster: bool,

		/// Paths the user can subscribe to (repeatable).
		#[arg(long)]
		subscribe: Vec<String>,

		/// Expiration time as a unix timestamp.
		#[arg(long, value_parser = parse_unix_timestamp)]
		expires: Option<std::time::SystemTime>,

		/// Issued-at time as a unix timestamp.
		#[arg(long, value_parser = parse_unix_timestamp)]
		issued: Option<std::time::SystemTime>,
	},

	/// Verify a token from stdin, writing the payload to stdout.
	Verify {
		/// Path to the key file.
		#[arg(long)]
		key: PathBuf,
	},
}

fn write_key(key: &moq_token::Key, path: &std::path::Path, base64: bool) -> anyhow::Result<()> {
	if base64 {
		Ok(key.to_file_base64url(path)?)
	} else {
		Ok(key.to_file(path)?)
	}
}

fn main() -> anyhow::Result<()> {
	let cli = Cli::parse();

	match cli.command {
		Commands::Generate {
			algorithm,
			id,
			out,
			out_dir,
			public,
			public_dir,
			base64,
		} => {
			let id = match id {
				Some(id) => moq_token::KeyId::decode(&id)?,
				None => moq_token::KeyId::random(),
			};

			let key = moq_token::Key::generate(algorithm, Some(id.clone()))?;

			if let Some(dir) = public_dir {
				let path = dir.join(format!("{id}.jwk"));
				write_key(&key.to_public()?, &path, base64)?;
			} else if let Some(path) = public {
				write_key(&key.to_public()?, &path, base64)?;
			}

			if let Some(dir) = out_dir {
				let path = dir.join(format!("{id}.jwk"));
				write_key(&key, &path, base64)?;
			} else if let Some(path) = out {
				write_key(&key, &path, base64)?;
			} else {
				let json = key.to_str()?;
				println!("{json}");
			}
		}

		Commands::Sign {
			key,
			root,
			publish,
			cluster,
			subscribe,
			expires,
			issued,
		} => {
			let key = moq_token::Key::from_file(key)?;

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

		Commands::Verify { key } => {
			let key = moq_token::Key::from_file(key)?;
			let token = io::read_to_string(io::stdin())?.trim().to_string();
			let payload = key.decode(&token)?;

			println!("{payload:#?}");
		}
	}

	Ok(())
}

fn parse_unix_timestamp(s: &str) -> anyhow::Result<std::time::SystemTime> {
	let timestamp = s.parse::<i64>().context("expected unix timestamp")?;
	let timestamp = timestamp.try_into().context("timestamp out of range")?;
	Ok(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp))
}
