//! Flags for serving QUIC from per-core workers.
//!
//! The workers themselves are [`moq_tokio::worker`]; this is the operator-facing
//! half, which is the only part specific to the relay. Unset (the default) keeps
//! QUIC on the shared runtime with everything else.

use std::{fmt, num::NonZeroU16, str::FromStr};

use serde::{Deserialize, Serialize};

/// How the relay lays its QUIC work out over threads.
#[derive(usage_rs::Args, Clone, Debug, Default, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Serve QUIC from single-threaded workers instead of the shared runtime, each
	/// with its own socket on the listen address: `shared` (the default), `auto`
	/// for one per core, or a count.
	///
	/// A connection is handled start to finish by one worker, which trades the
	/// shared runtime's load balancing for no cross-thread traffic per packet.
	/// Linux-only; every worker shares the relay's served identity and reloads.
	#[usage(
		long = "runtime-workers",
		env = "MOQ_RUNTIME_WORKERS",
		default = "shared",
		setting = "runtime.workers"
	)]
	pub workers: Workers,

	/// Pin each worker to a CPU core, defaulting to off.
	///
	/// The mode's measured win comes from each worker owning a socket and a
	/// runtime, not from pinning: on a single-socket machine, pinned and unpinned
	/// benchmark inside run-to-run noise of each other. Pinning stops the
	/// scheduler migrating a busy worker, which may matter on a multi-socket or
	/// NUMA machine with the cores to itself. It is off by default because every
	/// process pins its workers to the same first cores: two relays, or a relay
	/// limited to fewer CPUs than the host has, would crowd onto those cores.
	#[usage(long = "runtime-pin", env = "MOQ_RUNTIME_PIN", setting = "runtime.pin", bool_value)]
	pub pin: bool,

	/// Drive the QUIC workers with io_uring instead of tokio.
	///
	/// Each worker owns a `SINGLE_ISSUER` ring, batched UDP (multishot receive
	/// with `UDP_GRO`, `UDP_SEGMENT` send), userspace timers, and a local task
	/// set; sessions get every configured version (native raw QUIC and browser
	/// WebTransport alike), and everything else (HTTP, WebSocket, cluster
	/// dials, stats) stays on the shared runtime. Requires `runtime-workers`,
	/// Linux 6.12+, and refuses to start anywhere it cannot deliver.
	#[usage(
		long = "runtime-io-uring",
		env = "MOQ_RUNTIME_IO_URING",
		setting = "runtime.io_uring",
		bool_value
	)]
	pub io_uring: bool,
}

/// Where the relay serves QUIC, as `runtime.workers` spells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Workers {
	/// On the shared runtime with everything else.
	#[default]
	Shared,
	/// One worker per core this process may use.
	Auto,
	/// This many workers.
	Count(NonZeroU16),
}

impl FromStr for Workers {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value {
			"shared" => Ok(Self::Shared),
			"auto" => Ok(Self::Auto),
			count => count
				.parse()
				.map(Self::Count)
				.map_err(|_| format!("expected `shared`, `auto`, or a nonzero worker count, got {count:?}")),
		}
	}
}

impl fmt::Display for Workers {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Shared => f.write_str("shared"),
			Self::Auto => f.write_str("auto"),
			Self::Count(count) => count.fmt(f),
		}
	}
}

// A count is a TOML integer, so the value is a string or an integer.
impl Serialize for Workers {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			Self::Count(count) => serializer.serialize_u16(count.get()),
			named => serializer.collect_str(named),
		}
	}
}

impl<'de> Deserialize<'de> for Workers {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		#[derive(Deserialize)]
		#[serde(untagged)]
		enum Raw {
			Count(u64),
			Name(String),
		}
		match Raw::deserialize(deserializer)? {
			Raw::Count(count) => count.to_string().parse(),
			Raw::Name(name) => name.parse(),
		}
		.map_err(serde::de::Error::custom)
	}
}

impl Config {
	/// The worker group this asks for, or `None` to keep QUIC on the shared
	/// runtime.
	pub fn workers(&self) -> Option<moq_tokio::worker::Config> {
		let mut config = match self.workers {
			Workers::Shared => return None,
			Workers::Auto => moq_tokio::worker::Config::auto(),
			Workers::Count(count) => moq_tokio::worker::Config::new(count.get()),
		};
		config.pin = self.pin;
		Some(config)
	}

	/// Whether the workers should run on io_uring instead of tokio.
	pub fn io_uring(&self) -> bool {
		self.io_uring
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A count is a TOML integer and the modes are strings; zero has no meaning.
	#[test]
	fn workers_spellings() {
		let parse = |body: &str| toml::from_str::<Config>(body).map(|config| config.workers);
		assert_eq!(parse("").unwrap(), Workers::Shared);
		assert_eq!(parse("workers = \"shared\"").unwrap(), Workers::Shared);
		assert_eq!(parse("workers = \"auto\"").unwrap(), Workers::Auto);
		assert_eq!(
			parse("workers = 8").unwrap(),
			Workers::Count(NonZeroU16::new(8).unwrap())
		);
		assert!(parse("workers = 0").is_err());
		assert!(parse("workers = \"eight\"").is_err());

		for workers in [Workers::Shared, Workers::Auto, Workers::Count(NonZeroU16::MIN)] {
			assert_eq!(workers.to_string().parse(), Ok(workers));
			let config = Config {
				workers,
				..Default::default()
			};
			assert_eq!(parse(&toml::to_string(&config).unwrap()).unwrap(), workers);
		}
		assert!("0".parse::<Workers>().is_err());
	}
}
