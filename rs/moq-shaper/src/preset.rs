use std::{net::SocketAddr, time::Duration};

use anyhow::Context;
use serde::Deserialize;

use crate::{Batch, Config, Jitter, Options, Profile, Rate, Setup, Step};

/// The profiles shipped under `profiles/`, embedded so a binary needs no data files.
const BUILTIN: &[(&str, &str)] = &[
	("near-zero", include_str!("../profiles/near-zero.toml")),
	("mild", include_str!("../profiles/mild.toml")),
	("bursty", include_str!("../profiles/bursty.toml")),
	("step", include_str!("../profiles/step.toml")),
	("high-rtt", include_str!("../profiles/high-rtt.toml")),
	("lossy", include_str!("../profiles/lossy.toml")),
];

/// A named, seeded treatment of both directions, loaded from a TOML profile.
#[derive(Clone, Debug)]
pub struct Preset {
	/// The name a run records its counters under.
	pub name: String,
	seed: u64,
	shared: bool,
	up: (Profile, Options),
	down: (Profile, Options),
}

impl Preset {
	/// Load a built-in profile by name, or a profile file by path.
	pub fn load(name_or_path: &str) -> anyhow::Result<Self> {
		let text = match BUILTIN.iter().find(|(name, _)| *name == name_or_path) {
			Some((_, text)) => (*text).to_string(),
			// An unknown name is almost always a typo, so say what the choices were.
			None => std::fs::read_to_string(name_or_path).with_context(|| {
				format!(
					"no built-in profile or readable file named {name_or_path:?}; the built-ins are {}",
					Self::names().collect::<Vec<_>>().join(", ")
				)
			})?,
		};

		let file: File = toml::from_str(&text).with_context(|| format!("parse profile {name_or_path:?}"))?;
		file.preset()
			.with_context(|| format!("invalid profile {name_or_path:?}"))
	}

	/// The built-in profiles' names.
	pub fn names() -> impl Iterator<Item = &'static str> {
		BUILTIN.iter().map(|(name, _)| *name)
	}

	/// This preset listening on `bind` and forwarding to `target`, with its own seed.
	pub fn setup(&self, bind: SocketAddr, target: SocketAddr) -> Setup {
		Setup {
			config: Config {
				bind,
				target,
				seed: self.seed,
				up: self.up.0.clone(),
				down: self.down.0.clone(),
			},
			tcp_passthrough: false,
			shared: self.shared,
			up: self.up.1.clone(),
			down: self.down.1.clone(),
		}
	}
}

/// A profile file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
	name: String,
	seed: u64,
	#[serde(default)]
	shared: bool,
	#[serde(default)]
	up: Side,
	#[serde(default)]
	down: Side,
}

impl File {
	fn preset(self) -> anyhow::Result<Preset> {
		anyhow::ensure!(!self.name.is_empty(), "the profile has no name");
		Ok(Preset {
			up: self.up.split().context("up")?,
			down: self.down.split().context("down")?,
			name: self.name,
			seed: self.seed,
			shared: self.shared,
		})
	}
}

/// One direction of a profile file: its [`Profile`] and [`Options`] in one table.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Side {
	#[serde(with = "humantime_serde")]
	delay: Duration,
	#[serde(with = "humantime_serde")]
	jitter: Duration,
	jitter_model: Option<Jitter>,
	loss: f64,
	reorder: f64,
	rate: Option<Rate>,
	batch: Option<Batch>,
	steps: Vec<Step>,
}

impl Side {
	fn split(self) -> anyhow::Result<(Profile, Options)> {
		// Whether datagrams can overtake each other depends on the model as soon
		// as their delays differ, so a file has to say which it means rather
		// than inherit whichever is the default.
		let varies = !self.jitter.is_zero()
			|| self
				.steps
				.iter()
				.any(|step| step.delay.is_some() || step.jitter.is_some_and(|jitter| !jitter.is_zero()));
		anyhow::ensure!(
			self.jitter_model.is_some() || !varies,
			"the delay varies, so name a jitter_model: \"uniform\" lets a datagram overtake the one in front, \"gaussian\" never does"
		);

		let profile = Profile {
			delay: self.delay,
			jitter: self.jitter,
			loss: self.loss,
			reorder: self.reorder,
			rate: self.rate,
		};
		let options = Options {
			jitter_model: self.jitter_model.unwrap_or_default(),
			batch: self.batch,
			steps: self.steps,
		};
		options.validate(&profile)?;
		Ok((profile, options))
	}
}

#[cfg(test)]
mod tests {
	use std::net::{IpAddr, Ipv4Addr};

	use super::*;

	const LOCALHOST: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

	#[test]
	fn builtins_parse_and_are_named_after_their_file() {
		for name in Preset::names() {
			let preset = Preset::load(name).unwrap_or_else(|err| panic!("{name}: {err:#}"));
			assert_eq!(preset.name, name);
		}
	}

	#[test]
	fn builtins_treat_the_path_except_the_control() {
		for name in Preset::names() {
			let preset = Preset::load(name).unwrap();
			let untreated = (Profile::default(), Options::default());
			let idle = preset.up == untreated && preset.down == untreated;
			assert_eq!(idle, name == "near-zero", "{name} treats the wrong amount of traffic");
		}
	}

	#[test]
	fn the_step_builtin_steps_both_directions_at_thirty_seconds() {
		let setup = Preset::load("step").unwrap().setup(LOCALHOST, LOCALHOST);
		for (which, profile, options) in [
			("up", &setup.config.up, &setup.up),
			("down", &setup.config.down, &setup.down),
		] {
			assert_eq!(profile.delay, Duration::from_millis(5), "{which}");
			assert_eq!(options.jitter_model, Jitter::Gaussian, "{which}");
			assert_eq!(options.steps.len(), 1, "{which}");
			assert_eq!(options.steps[0].at, Duration::from_secs(30), "{which}");
			assert_eq!(options.steps[0].delay, Some(Duration::from_millis(60)), "{which}");
		}
	}

	#[test]
	fn an_unknown_name_lists_the_builtins() {
		let err = format!("{:#}", Preset::load("nope").unwrap_err());
		assert!(err.contains("near-zero"), "{err}");
	}

	#[test]
	fn an_unknown_field_is_refused() {
		// A field the schema does not know, like a rate in bytes, is refused rather than ignored.
		let Err(err) = toml::from_str::<File>("name = \"x\"\nseed = 1\n[up.rate]\nbytes_per_second = 1000\n") else {
			panic!("a rate in bytes parsed");
		};
		assert!(err.to_string().contains("bytes_per_second"), "{err}");
	}

	#[test]
	fn a_varying_delay_has_to_name_its_model() {
		let parse = |text: &str| toml::from_str::<File>(text).unwrap().preset();

		let err = parse("name = \"x\"\nseed = 1\n[up]\ndelay = \"5ms\"\njitter = \"5ms\"\n").unwrap_err();
		assert!(format!("{err:#}").contains("jitter_model"), "{err:#}");

		let err = parse("name = \"x\"\nseed = 1\n[up]\ndelay = \"5ms\"\n[[up.steps]]\nat = \"1s\"\ndelay = \"9ms\"\n")
			.unwrap_err();
		assert!(format!("{err:#}").contains("jitter_model"), "{err:#}");

		// A fixed delay cannot reorder anything, so it needs no model.
		parse("name = \"x\"\nseed = 1\n[up]\ndelay = \"5ms\"\n").unwrap();
		parse("name = \"x\"\nseed = 1\n[up]\ndelay = \"5ms\"\njitter = \"5ms\"\njitter_model = \"uniform\"\n").unwrap();
	}

	#[test]
	fn a_profile_file_loads_by_path() {
		let path = std::env::temp_dir().join(format!("moq-shaper-preset-{}.toml", std::process::id()));
		std::fs::write(
			&path,
			"name = \"file\"\nseed = 9\n[down.batch]\ncount = 3\nwindow = \"50ms\"\n",
		)
		.unwrap();
		let preset = Preset::load(path.to_str().unwrap());
		std::fs::remove_file(&path).unwrap();

		let setup = preset.unwrap().setup(LOCALHOST, LOCALHOST);
		assert_eq!(setup.config.seed, 9);
		assert_eq!(
			setup.down.batch,
			Some(Batch {
				count: 3,
				window: Duration::from_millis(50)
			})
		);
	}

	#[tokio::test]
	async fn the_builtins_all_bind() {
		for name in Preset::names() {
			// Binding proves the profile is usable, not just parseable.
			let preset = Preset::load(name).unwrap();
			let shaper = crate::Shaper::bind(preset.setup(LOCALHOST, LOCALHOST))
				.await
				.unwrap_or_else(|err| panic!("{name}: {err:#}"));
			shaper.verify().unwrap();
		}
	}
}
