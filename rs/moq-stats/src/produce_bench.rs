//! One producer tick, without the publish interval. The benchmark drives this
//! over a registry it filled itself. Not a public API.

use std::time::Duration;

use moq_clock::Instant;
use moq_net::{PathOwned, origin, stats::Registry, track};

use super::{Drain, PendingPair, Task};

/// Group-track frame sizes from one tick, in bytes.
///
/// `held` is the largest snapshot any group's track is still holding (idle
/// paths included). `wrote` is the largest frame this tick wrote, a snapshot
/// or a merge-patch delta, or zero when every group was unchanged. Each is a
/// max across groups, so a single frame can be compared with the cache cap.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameBytes {
	/// Largest group snapshot still held.
	pub held: usize,
	/// Largest group frame written this tick.
	pub wrote: usize,
}

/// A stats publisher, driven one drain at a time.
pub struct Driver {
	drain: Drain,
	/// The last drain's instant. Each tick is one interval later, so it may open a group.
	now: Instant,
}

impl Driver {
	/// Publish `.stats/node` on `origin` and drain `registry` into one group
	/// track per `depth` leading path segments, the way [`crate::Producer::new`]
	/// does. `None` if the origin refuses the broadcast.
	pub fn new(registry: Registry, origin: origin::Producer, depth: usize) -> Option<Self> {
		let task = Task {
			registry,
			origin,
			prefix: PathOwned::from(".stats"),
			node: None,
			depth,
			linger: Duration::from_secs(300),
			interval: Duration::from_secs(1),
		};
		Some(Self {
			drain: Drain::new(task)?,
			now: moq_clock::now(),
		})
	}

	/// Drain the registry and encode every track once, one interval after the last tick.
	pub fn tick(&mut self) -> FrameBytes {
		self.now += self.drain.task.interval;
		self.drain.collect();
		self.drain.publish(self.now);
		self.drain.group_frame_bytes()
	}

	/// Hold `label`'s presence track as a reader would, so every tick updates
	/// it. Keep the returned subscriber: dropping it lets a tick reclaim the
	/// track.
	pub fn hold_presence(&mut self, label: &str) -> track::Subscriber {
		let stamp = self.drain.stamp(self.now);
		let node = &mut self.drain.node;
		let label = PathOwned::from(label.to_string());
		node.adopt_presence(&self.drain.report, label.clone(), PendingPair::default(), stamp);
		node.presence[&label].pair.plain.track.subscribe(None)
	}

	/// Hold broadcast prefix `path`'s track as a reader would; see
	/// [`Self::hold_presence`].
	pub fn hold_prefix(&mut self, path: &str) -> track::Subscriber {
		let tick = self.drain.tick;
		let stamp = self.drain.stamp(self.now);
		let node = &mut self.drain.node;
		node.adopt_prefix(
			&self.drain.report,
			PathOwned::from(path.to_string()),
			PendingPair::default(),
			tick,
			stamp,
		);
		node.prefixes[path].pair.plain.track.subscribe(None)
	}
}
