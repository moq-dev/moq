//! The token command line surface, behind the `cli` feature.
//!
//! Flatten [`Args`] into a `clap` command and call [`Args::run`]. Both the standalone
//! `moq-token` binary and `moq token` are built from this module, so they stay in sync.

use anyhow::Context;
use clap::Subcommand;
use std::{io, path::PathBuf};

use crate::Algorithm;

/// Generate, sign, and verify tokens for moq-relay.
#[derive(clap::Args, Clone, Debug)]
pub struct Args {
	#[command(subcommand)]
	command: Command,
}

impl Args {
	/// Run the requested command, writing the key, token, or payload to the chosen
	/// destination (stdout by default).
	pub fn run(self) -> anyhow::Result<()> {
		match self.command {
			Command::Generate {
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
					Some(id) => crate::KeyId::decode(&id)?,
					None => crate::KeyId::random(),
				};

				let mut key = crate::Key::generate(algorithm, Some(id.clone()))?;
				if !publish.is_empty() || !subscribe.is_empty() {
					key = key.with_scope(crate::Scope {
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

			Command::Sign {
				key,
				root,
				publish,
				subscribe,
				expires,
				issued,
			} => {
				let key = read_key(&key)?;

				let payload = crate::Claims::default()
					.with_root(root)
					.with_publish(publish)
					.with_subscribe(subscribe)
					.with_expires(expires)
					.with_issued(issued);

				let token = key.sign(&payload)?;
				println!("{token}");
			}

			Command::Verify { key, token } => {
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
}

#[derive(Subcommand, Clone, Debug)]
enum Command {
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

fn write_key(key: &crate::Key, path: &std::path::Path) -> anyhow::Result<()> {
	if is_dash(path) {
		println!("{}", key.to_str()?);
		Ok(())
	} else {
		key.to_file(path)
			.with_context(|| format!("failed to write key to {}", path.display()))
	}
}

fn read_key(path: &std::path::Path) -> anyhow::Result<crate::Key> {
	if is_dash(path) {
		let contents = io::read_to_string(io::stdin())?;
		crate::Key::from_str(contents.trim()).context("failed to parse key from stdin")
	} else {
		crate::Key::from_file(path).with_context(|| format!("failed to read key from {}", path.display()))
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

fn parse_unix_timestamp(s: &str) -> anyhow::Result<std::time::SystemTime> {
	let timestamp = s.parse::<i64>().context("expected unix timestamp")?;
	let timestamp = timestamp.try_into().context("timestamp out of range")?;
	Ok(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp))
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	/// Drive the same clap grammar the binaries expose, rather than building
	/// `Command` directly, so the flags stay part of what's under test.
	#[derive(Parser)]
	struct Harness {
		#[command(flatten)]
		args: Args,
	}

	fn run(args: &[&str]) -> anyhow::Result<()> {
		Harness::try_parse_from(args)?.args.run()
	}

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(name);
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("scratch dir");
		dir
	}

	#[test]
	fn generate_writes_a_usable_keypair() {
		let dir = scratch("moq-token-cli-keypair");
		let private = dir.join("private.jwk");
		let public = dir.join("public.jwk");

		run(&[
			"moq-token",
			"generate",
			// ES256 rather than an RSA algorithm: keygen dominates this test's runtime.
			"--algorithm",
			"ES256",
			"--out",
			private.to_str().unwrap(),
			"--public",
			public.to_str().unwrap(),
		])
		.unwrap();

		// What the relay actually does with these two files: the public half has to
		// verify what the private half signed.
		let token = crate::Key::from_file(&private)
			.unwrap()
			.sign(&crate::Claims::default().with_root("demo").with_publish(["alice"]))
			.unwrap();
		let path = dir.join("alice.jwt");
		std::fs::write(&path, &token).unwrap();

		run(&[
			"moq-token",
			"verify",
			"--key",
			public.to_str().unwrap(),
			"--in",
			path.to_str().unwrap(),
		])
		.unwrap();
	}

	#[test]
	fn generate_to_a_directory_names_the_file_after_the_kid() {
		let dir = scratch("moq-token-cli-outdir");

		run(&["moq-token", "generate", "--out-dir", dir.to_str().unwrap()]).unwrap();

		let written: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
		assert_eq!(written.len(), 1, "expected exactly one key, got {written:?}");
		let key = crate::Key::from_file(&written[0]).unwrap();
		let kid = key.kid.as_ref().expect("generate assigns a kid");
		assert_eq!(written[0].file_name().unwrap().to_str().unwrap(), format!("{kid}.jwk"));
	}

	// Both halves on stdout would interleave into one unparseable blob, so it's
	// rejected up front rather than written.
	#[test]
	fn both_keys_to_stdout_is_rejected() {
		let err = run(&["moq-token", "generate", "--algorithm", "ES256", "--public", "-"]).unwrap_err();
		assert!(err.to_string().contains("cannot write both keys to stdout"), "{err}");
	}

	#[test]
	fn both_inputs_from_stdin_is_rejected() {
		let err = run(&["moq-token", "verify", "--key", "-", "--in", "-"]).unwrap_err();
		assert!(err.to_string().contains("cannot both read from stdin"), "{err}");
	}
}
