//! One producer tick, without the publish interval. The benchmark drives this
//! over a registry it filled itself. Not a public API.

use std::time::Duration;

use moq_net::{PathOwned, origin, stats::Registry};
use web_async::time::Instant;

use super::{Drain, Task};

/// Plain and compressed publisher-frame sizes from one tick, in bytes.
///
/// `held` is the largest plain snapshot any tier is still holding (idle paths
/// included). `plain` and `compressed` are what this tick wrote, or zero when
/// the encoder skipped an unchanged value. Each is the max across tiers, so a
/// single frame can be compared with the cache cap.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameBytes {
	/// Largest plain publisher snapshot still held.
	pub held: usize,
	/// Largest plain publisher payload written this tick.
	pub plain: usize,
	/// Largest compressed publisher payload written this tick.
	pub compressed: usize,
}

/// A depth-0 stats publisher, driven one drain at a time.
pub struct Driver {
	drain: Drain,
}

impl Driver {
	/// Publish `.stats/node` on `origin` and drain `registry`, the way
	/// [`crate::Producer::new`] does at depth 0. `None` if the origin refuses
	/// the broadcast.
	pub fn new(registry: Registry, origin: origin::Producer) -> Option<Self> {
		let task = Task {
			registry,
			origin,
			prefix: PathOwned::from(".stats"),
			node: None,
			depth: 0,
			linger: Duration::from_secs(300),
			interval: Duration::from_secs(1),
		};
		Some(Self {
			drain: Drain::new(task)?,
		})
	}

	/// Drain the registry and encode every track once.
	pub fn tick(&mut self) -> FrameBytes {
		self.drain.collect();
		self.drain.publish(Instant::now());
		self.drain.publisher_frame_bytes()
	}
}
