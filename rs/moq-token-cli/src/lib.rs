//! The token command line surface: generate, sign, and verify tokens for moq-relay.
//!
//! Flatten [`Args`] into a Usage command and call [`Args::run`]. The standalone
//! `moq-token` binary and moq-cli's `moq token` are both built from this crate, so
//! they stay in sync. The `moq-token` library underneath stays free of CLI concerns.

use anyhow::Context;
use std::{io, path::PathBuf};

use moq_token::Algorithm;

#[derive(usage::Cli)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq-token")]
#[usage(about = "Generate, sign, and verify tokens for moq-relay")]
#[usage(version = env!("VERSION"))]
#[usage(completion)]
struct Root {
	#[usage(subcommand)]
	command: Command,
}

/// Generate, sign, and verify tokens for moq-relay.
#[derive(usage::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	#[usage(subcommand)]
	command: Command,
}

impl Args {
	/// Parse the standalone `moq-token` command line.
	pub fn parse() -> Self {
		Self {
			command: Root::parse().command,
		}
	}

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

			Command::Sign {
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
					.with_expires(expires.map(Into::into))
					.with_issued(issued.map(Into::into));

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

#[derive(usage::Subcommands, Clone, Debug)]
enum Command {
	/// Generate a new signing key.
	///
	/// A random key ID is assigned unless --id is specified.
	/// Output is base64url-encoded JSON.
	Generate {
		/// The algorithm to use.
		#[usage(long, default = "HS256")]
		algorithm: Algorithm,

		/// The key ID. Randomly generated if not provided.
		#[usage(long)]
		id: Option<String>,

		/// Write the key to a file path. Use `-` for stdout.
		#[usage(long)]
		out: Option<PathBuf>,

		/// Write the key to a directory as {kid}.jwk.
		#[usage(long, conflicts = "--out")]
		out_dir: Option<PathBuf>,

		/// Write the public key to a file path (asymmetric algorithms only). Use `-` for stdout.
		#[usage(long)]
		public: Option<PathBuf>,

		/// Write the public key to a directory as {kid}.jwk (asymmetric algorithms only).
		#[usage(long, conflicts = "--public")]
		public_dir: Option<PathBuf>,

		/// Root path for the optional key scope. Only applied alongside --publish or --subscribe.
		#[usage(long, default = "")]
		root: String,

		/// Publish prefixes the key may grant (repeatable).
		#[usage(long)]
		publish: Vec<String>,

		/// Subscribe prefixes the key may grant (repeatable).
		#[usage(long)]
		subscribe: Vec<String>,
	},

	/// Sign a token, writing it to stdout.
	Sign {
		/// Path to the signing key file. Use `-` for stdin.
		#[usage(long)]
		key: PathBuf,

		/// The root path for the token.
		#[usage(long, default = "")]
		root: String,

		/// Paths the user can publish to (repeatable).
		#[usage(long)]
		publish: Vec<String>,

		/// Paths the user can subscribe to (repeatable).
		#[usage(long)]
		subscribe: Vec<String>,

		/// Expiration time as a unix timestamp.
		#[usage(long)]
		expires: Option<UnixTimestamp>,

		/// Issued-at time as a unix timestamp.
		#[usage(long)]
		issued: Option<UnixTimestamp>,
	},

	/// Verify a token, writing the payload to stdout.
	Verify {
		/// Path to the key file. Use `-` for stdin (requires `--in` to be a file).
		#[usage(long)]
		key: PathBuf,

		/// Path to read the token from. Use `-` for stdin.
		#[usage(long = "in", default = "-")]
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

#[derive(Clone, Copy, Debug)]
struct UnixTimestamp(std::time::SystemTime);

impl From<UnixTimestamp> for std::time::SystemTime {
	fn from(value: UnixTimestamp) -> Self {
		value.0
	}
}

impl std::str::FromStr for UnixTimestamp {
	type Err = anyhow::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let timestamp = s.parse::<i64>().context("expected unix timestamp")?;
		let timestamp = timestamp.try_into().context("timestamp out of range")?;
		// checked_add, because plain `+` panics on overflow and how far a SystemTime
		// reaches is platform-dependent: a timespec holds i64 seconds, while Windows
		// counts 100ns ticks and runs out far sooner.
		let value = std::time::SystemTime::UNIX_EPOCH
			.checked_add(std::time::Duration::from_secs(timestamp))
			.context("timestamp out of range")?;
		Ok(Self(value))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	/// Drive the same Usage grammar the binaries expose, rather than building
	/// `Command` directly, so the flags stay part of what's under test.
	fn run(args: &[&str]) -> anyhow::Result<()> {
		let args = args.iter().skip(1).map(std::ffi::OsStr::new).collect::<Vec<_>>();
		let root =
			Root::parse_from(&args).map_err(|err| anyhow::anyhow!(usage::render_failure(Root::spec(), &args, &err)))?;
		Args { command: root.command }.run()
	}

	#[test]
	fn generate_writes_a_usable_keypair() {
		let dir = tempfile::tempdir().unwrap();
		let private = dir.path().join("private.jwk");
		let public = dir.path().join("public.jwk");

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

		// Sign through the CLI grammar rather than the library, so the value parsers
		// behind --expires / --issued are covered too.
		run(&[
			"moq-token",
			"sign",
			"--key",
			private.to_str().unwrap(),
			"--root",
			"demo",
			"--publish",
			"alice",
			"--issued",
			"1700000000",
			"--expires",
			"4102444800",
		])
		.unwrap();

		// What the relay actually does with these two files: the public half has to
		// verify what the private half signed. `sign` only prints to stdout, so the
		// token itself comes from the library.
		let token = moq_token::Key::from_file(&private)
			.unwrap()
			.sign(&moq_token::Claims::default().with_root("demo").with_publish(["alice"]))
			.unwrap();
		let path = dir.path().join("alice.jwt");
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
		let dir = tempfile::tempdir().unwrap();

		run(&["moq-token", "generate", "--out-dir", dir.path().to_str().unwrap()]).unwrap();

		let written: Vec<_> = std::fs::read_dir(dir.path())
			.unwrap()
			.map(|e| e.unwrap().path())
			.collect();
		assert_eq!(written.len(), 1, "expected exactly one key, got {written:?}");
		let key = moq_token::Key::from_file(&written[0]).unwrap();
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

	#[test]
	fn timestamp_before_the_epoch_is_rejected() {
		assert!("-1".parse::<UnixTimestamp>().is_err());
		assert!("not-a-number".parse::<UnixTimestamp>().is_err());
	}

	// Whether the largest parseable timestamp is representable depends on the
	// platform's SystemTime, so assert only that it never panics.
	#[test]
	fn timestamp_at_the_maximum_does_not_panic() {
		let _ = i64::MAX.to_string().parse::<UnixTimestamp>();
	}
}
