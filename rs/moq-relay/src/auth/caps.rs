//! The bitrate caps a grant puts on a session, and the QUIC connection that holds it to them.

use std::{num::NonZeroU64, sync::Arc};

use moq_auth::{Grant, Rate};

/// Why a capped grant is refused: nothing under the session would hold it to the cap.
pub(crate) const UNENFORCED: &str = "this transport cannot enforce bitrate caps";

/// What a session may publish and what it may receive for its subscriptions, each
/// uncapped when `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Caps {
	pub publish: Option<Rate>,
	pub subscribe: Option<Rate>,
}

impl Caps {
	/// The caps one grant names.
	pub fn of(grant: &Grant) -> Self {
		Self {
			publish: grant.publish_rate,
			subscribe: grant.subscribe_rate,
		}
	}

	/// The caps of a union of grants, matching the union's permissive meaning: each
	/// direction takes the highest cap among the grants that cap it, and a grant that
	/// gives the direction paths without a cap lifts it. A grant naming neither has no say.
	pub fn union<'a>(grants: impl IntoIterator<Item = &'a Grant> + Clone) -> Self {
		fn highest<'a>(roles: impl Iterator<Item = (&'a moq_auth::Patterns, Option<Rate>)>) -> Option<Rate> {
			let mut highest = None;
			for (paths, rate) in roles {
				match rate {
					Some(rate) => highest = highest.max(Some(rate)),
					None if !paths.is_empty() => return None,
					None => {}
				}
			}
			highest
		}
		Self {
			publish: highest(
				grants
					.clone()
					.into_iter()
					.map(|grant| (&grant.publish, grant.publish_rate)),
			),
			subscribe: highest(grants.into_iter().map(|grant| (&grant.subscribe, grant.subscribe_rate))),
		}
	}

	/// Whether either direction is capped.
	pub fn is_capped(&self) -> bool {
		self.publish.is_some() || self.subscribe.is_some()
	}

	/// The QUIC limits that hold a connection to these caps: the relay receives what the
	/// session publishes and sends what it subscribes to, in bytes rather than bits.
	pub fn limits(&self) -> moq_quic::Limits {
		let rate = |cap: Option<Rate>| match cap {
			None => moq_quic::Rate::Unlimited,
			Some(rate) => {
				moq_quic::Rate::BytesPerSecond(NonZeroU64::new(rate.as_bps().div_ceil(8)).expect("a positive rate"))
			}
		};
		let mut limits = moq_quic::Limits::default();
		limits.receive_rate = Some(rate(self.publish));
		limits.send_rate = Some(rate(self.subscribe));
		limits
	}
}

impl std::fmt::Display for Caps {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let cap = |rate: Option<Rate>| rate.map_or_else(|| "uncapped".to_string(), |rate| rate.to_string());
		write!(f, "publish {}, subscribe {}", cap(self.publish), cap(self.subscribe))
	}
}

/// Applies a session's caps to the QUIC connection carrying it, on whichever runtime
/// drives that connection.
///
/// Only a transport that can hold a session to its caps has one, so a session without
/// one refuses a capped grant at admission, in band, and at revalidation.
#[derive(Clone)]
pub struct Pacer(Arc<dyn Fn(Caps) + Send + Sync>);

impl Pacer {
	/// Pace a live connection by calling `apply` with each new set of caps.
	pub(crate) fn new(apply: impl Fn(Caps) + Send + Sync + 'static) -> Self {
		Self(Arc::new(apply))
	}

	/// Hold the connection to `caps` from now on.
	pub(crate) fn pace(&self, caps: Caps) {
		(self.0)(caps)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rate(bps: u64) -> Option<Rate> {
		Some(Rate::from_bps(bps).unwrap())
	}

	fn grant(publish: &[&str], publish_rate: Option<Rate>, subscribe: &[&str], subscribe_rate: Option<Rate>) -> Grant {
		let patterns = |texts: &[&str]| {
			texts
				.iter()
				.map(|text| text.parse().unwrap())
				.collect::<moq_auth::Patterns>()
		};
		let mut grant = Grant::new(patterns(publish), patterns(subscribe));
		grant.publish_rate = publish_rate;
		grant.subscribe_rate = subscribe_rate;
		grant
	}

	#[test]
	fn a_union_takes_the_highest_cap_and_an_uncapped_grant_lifts_it() {
		let low = grant(&["a/**"], rate(1_000), &["**"], rate(8_000));
		let high = grant(&["b/**"], rate(2_000), &["**"], rate(4_000));
		let caps = Caps::union([&low, &high]);
		assert_eq!(
			caps,
			Caps {
				publish: rate(2_000),
				subscribe: rate(8_000)
			}
		);

		let uncapped = grant(&["c/**"], None, &[], None);
		let caps = Caps::union([&low, &high, &uncapped]);
		assert_eq!(
			caps,
			Caps {
				publish: None,
				subscribe: rate(8_000)
			},
			"only the direction it grants"
		);

		let silent = grant(&[], None, &["d"], rate(16_000));
		assert_eq!(
			Caps::union([&low, &silent]),
			Caps {
				publish: rate(1_000),
				subscribe: rate(16_000)
			}
		);
		assert_eq!(Caps::union([]), Caps::default());
	}

	#[test]
	fn limits_convert_bits_to_bytes_rounding_up() {
		let caps = Caps {
			publish: rate(8_000_001),
			subscribe: None,
		};
		let limits = caps.limits();
		assert_eq!(
			limits.receive_rate,
			Some(moq_quic::Rate::BytesPerSecond(NonZeroU64::new(1_000_001).unwrap()))
		);
		assert_eq!(limits.send_rate, Some(moq_quic::Rate::Unlimited));
		let one = Caps {
			publish: None,
			subscribe: rate(1),
		}
		.limits();
		assert_eq!(one.send_rate, Some(moq_quic::Rate::BytesPerSecond(NonZeroU64::MIN)));
	}
}
