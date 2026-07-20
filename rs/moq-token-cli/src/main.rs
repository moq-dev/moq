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
	/// Output is base64url-encoded JSON.
	Generate {
		/// The algorithm to use.
		#[arg(long, default_value = "HS256")]
		algorithm: Algorithm,

		/// The key ID. Randomly generated if not provided.
		#[arg(long)]
		id: Option<String>,

		/// Write the key to a file path. Use `-` for stdout.
		#[arg(long)]
		out: Option<PathBuf>,

		/// Write the key to a directory as {kid}.jwk.
		#[arg(long, conflicts_with = "out")]
		out_dir: Option<PathBuf>,

		/// Write the public key to a file path (asymmetric algorithms only). Use `-` for stdout.
		#[arg(long)]
		public: Option<PathBuf>,

		/// Write the public key to a directory as {kid}.jwk (asymmetric algorithms only).
		#[arg(long, conflicts_with = "public")]
		public_dir: Option<PathBuf>,

		/// Root path for the optional key scope.
		#[arg(long, default_value = "")]
		root: String,

		/// Publish prefixes the key may grant (repeatable).
		#[arg(long)]
		publish: Vec<String>,

		/// Subscribe prefixes the key may grant (repeatable).
		#[arg(long)]
		subscribe: Vec<String>,
	},

	/// Sign a token, writing it to stdout.
	Sign {
		/// Path to the signing key file. Use `-` for stdin.
		#[arg(long)]
		key: PathBuf,

		/// The root path for the token.
		#[arg(long, default_value = "")]
		root: String,

		/// Paths the user can publish to (repeatable).
		#[arg(long)]
		publish: Vec<String>,

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

	/// Verify a token, writing the payload to stdout.
	Verify {
		/// Path to the key file. Use `-` for stdin (requires `--in` to be a file).
		#[arg(long)]
		key: PathBuf,

		/// Path to read the token from. Use `-` for stdin.
		#[arg(long = "in", default_value = "-")]
		token: PathBuf,
	},
}

fn is_dash(path: &std::path::Path) -> bool {
	path == std::path::Path::new("-")
}

fn write_key(key: &moq_token::Key, path: &std::path::Path) -> anyhow::Result<()> {
	if is_dash(path) {
		println!("{}", key.to_str()?);
		Ok(())
	} else {
		key.to_file(path)
			.with_context(|| format!("failed to write key to {}", path.display()))
	}
}

fn read_key(path: &std::path::Path) -> anyhow::Result<moq_token::Key> {
	if is_dash(path) {
		let contents = io::read_to_string(io::stdin())?;
		moq_token::Key::from_str(contents.trim()).context("failed to parse key from stdin")
	} else {
		moq_token::Key::from_file(path).with_context(|| format!("failed to read key from {}", path.display()))
	}
}

fn read_token(path: &std::path::Path) -> anyhow::Result<String> {
	let raw = if is_dash(path) {
		io::read_to_string(io::stdin())?
	} else {
		std::fs::read_to_string(path).with_context(|| format!("failed to read token from {}", path.display()))?
	};
	Ok(raw.trim().to_string())
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
			root,
			publish,
			subscribe,
		} => {
			let id = match id {
				Some(id) => moq_token::KeyId::decode(&id)?,
				None => moq_token::KeyId::random(),
			};

			let mut key = moq_token::Key::generate(algorithm, Some(id.clone()))?;
			if !publish.is_empty() || !subscribe.is_empty() {
				key = key.with_scope(moq_token::Scope {
					root,
					publish,
					subscribe,
				})?;
			}

			let public_to_stdout = public.as_deref().is_some_and(is_dash);
			let private_to_stdout = out_dir.is_none() && out.as_deref().is_none_or(is_dash);
			if public_to_stdout && private_to_stdout {
				anyhow::bail!(
					"cannot write both keys to stdout; use --out/--public with a file path, or --out-dir/--public-dir"
				);
			}

			if let Some(dir) = public_dir {
				let path = dir.join(format!("{id}.jwk"));
				write_key(&key.to_public()?, &path)?;
			} else if let Some(path) = public {
				write_key(&key.to_public()?, &path)?;
			}

			if let Some(dir) = out_dir {
				let path = dir.join(format!("{id}.jwk"));
				write_key(&key, &path)?;
			} else if let Some(path) = out {
				write_key(&key, &path)?;
			} else {
				let encoded = key.to_str()?;
				println!("{encoded}");
			}
		}

		Commands::Sign {
			key,
			root,
			publish,
			subscribe,
			expires,
			issued,
		} => {
			let key = read_key(&key)?;

			let payload = moq_token::Claims::default()
				.with_root(root)
				.with_publish(publish)
				.with_subscribe(subscribe)
				.with_expires(expires)
				.with_issued(issued);

			let token = key.sign(&payload)?;
			println!("{token}");
		}

		Commands::Verify { key, token } => {
			if is_dash(&key) && is_dash(&token) {
				anyhow::bail!("--key and --in cannot both read from stdin");
			}
			let key = read_key(&key)?;
			let token = read_token(&token)?;
			let payload = key.verify(&token)?;

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
