//! Shared test support for moq-net integration tests.

pub mod harness;
pub mod mock;
pub mod schedule;

use std::ops::{Bound, RangeBounds};

use futures::channel::mpsc;
use moq_net::{Timestamp, datagram, goaway, group, origin, track};

/// Appending for tests that count groups rather than time them.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub trait AppendNext {
	/// Append a group starting one past the largest start the track holds, or at 0 on an empty
	/// track, so consecutive appends land on consecutive keys and skip nothing between them.
	/// Its first frame is `data` at that key.
	fn append_next(&self, data: impl moq_net::IntoBytes) -> moq_net::Result<group::Producer>;

	/// The timestamp of the key [`Self::append_next`] would take.
	fn next_start(&self) -> Timestamp;

	/// Group key `key` as a timestamp in the track's timescale, for a first frame at that key.
	fn key_time(&self, key: u64) -> Timestamp;
}

impl AppendNext for track::Producer {
	fn append_next(&self, data: impl moq_net::IntoBytes) -> moq_net::Result<group::Producer> {
		self.append_group(self.next_start(), data)
	}

	fn next_start(&self) -> Timestamp {
		self.key_time(self.largest().map_or(0, |largest| largest.group + 1))
	}

	fn key_time(&self, key: u64) -> Timestamp {
		match self.info().timescale {
			Some(scale) => Timestamp::new(key, scale).expect("a key the scale holds"),
			None => Timestamp::unscaled(key),
		}
	}
}

/// A group opened by its first frame, for tests that write each frame of a group in one loop.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub struct LazyGroup {
	track: track::Producer,
	/// Where the group starts unless its first frame says otherwise.
	start: Timestamp,
	/// Whether the group is appended, declaring the keys below it skipped, or created.
	append: bool,
	group: Option<group::Producer>,
}

#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
impl LazyGroup {
	/// The group [`AppendNext::append_next`] would append.
	pub fn append(track: &track::Producer) -> Self {
		Self {
			start: track.next_start(),
			track: track.clone(),
			append: true,
			group: None,
		}
	}

	/// A group created at `key`, as [`track::Producer::create_group`] does.
	pub fn create(track: &track::Producer, key: u64) -> Self {
		Self {
			start: track.key_time(key),
			track: track.clone(),
			append: false,
			group: None,
		}
	}

	/// Where the group starts.
	pub fn at_start(&self) -> Timestamp {
		self.start
	}

	/// Write a frame at `timestamp`, the first of which opens the group there.
	pub fn write_frame(&mut self, timestamp: Timestamp, data: impl moq_net::IntoBytes) -> moq_net::Result<()> {
		match &mut self.group {
			Some(group) => group.write_frame(timestamp, data),
			None => {
				let group = match self.append {
					true => self.track.append_group(timestamp, data)?,
					false => self.track.create_group(timestamp, data)?,
				};
				self.start = self.track.key_time(group.start);
				self.group = Some(group);
				Ok(())
			}
		}
	}

	/// The open group, once a frame opened it.
	pub fn group(&mut self) -> &mut group::Producer {
		self.group.as_mut().expect("a frame opened the group")
	}

	/// Finish the group, which a frame opened.
	pub fn finish(self) -> moq_net::Result<group::Finished> {
		self.group.expect("a frame opened the group").finish()
	}

	/// Finish the group at `end`, which a frame opened.
	pub fn finish_at(self, end: u64) -> moq_net::Result<group::Finished> {
		self.group.expect("a frame opened the group").finish_at(end)
	}
}

/// The uni streams a session of `version` keeps open toward each side for its whole
/// life, which a stream credit must cover besides the groups: draft 22's SETUP stream,
/// and lite-07's Announce Stream.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn held_uni_streams(version: moq_net::Version) -> usize {
	usize::from(!version.is_lite() || version.to_string() == "moq-lite-07-wip")
}

/// A frame time for tests whose frames carry no time of their own.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub trait AtStart {
	/// Where the group starts, as a timestamp in its timed track's timescale.
	fn at_start(&self) -> Timestamp;
}

impl AtStart for group::Producer {
	fn at_start(&self) -> Timestamp {
		Timestamp::new(self.start, self.timescale().expect("a timed track")).unwrap()
	}
}

/// The groups from the one containing `keys`' start through the one containing its end, as
/// times: on a track whose groups start at consecutive keys, the groups with those keys.
///
/// An omitted start is the oldest group held. `..0` holds nothing, so it panics.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn keys(keys: impl RangeBounds<u64>) -> track::Range {
	let start = match keys.start_bound() {
		Bound::Included(&start) => Some(start),
		Bound::Excluded(&start) => Some(start + 1),
		Bound::Unbounded => None,
	};
	let end = match keys.end_bound() {
		Bound::Included(&end) => Some(end),
		Bound::Excluded(&end) => Some(end.checked_sub(1).expect("an empty range")),
		Bound::Unbounded => None,
	};
	let start = match start {
		Some(start) => track::Start::Time(start),
		None => track::Start::Oldest,
	};
	track::Range { start, end }
}

/// Wait until anyone wants the track.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn wanted(mut demand: track::Demand) -> moq_net::Result<()> {
	while !demand.wanted_changed().await? {}
	Ok(())
}

/// Wait until nobody wants the track.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn unwanted(mut demand: track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await? {}
	Ok(())
}

/// Wait until a datagram track's highest subscriber priority is `want`, where `None` means
/// nobody reads it.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn datagram_priority(producer: &datagram::Producer, want: Option<i8>) {
	let mut watch = producer.clone();
	while watch.priority_changed().await != want {}
}

/// Wait until `consumer` sees a route announced at exactly `path`, and return it.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn announced(consumer: &origin::Consumer, path: &str) -> origin::RouteInfo {
	let mut announced = consumer.routes();
	loop {
		match announced.next().await.expect("the origin closed") {
			origin::RouteEvent {
				kind: origin::RouteKind::Start | origin::RouteKind::Update | origin::RouteKind::Restart,
				announce,
				..
			} if announce.prefix.as_str() == path => {
				return announce.route;
			}
			_ => {}
		}
	}
}

/// The kind of an [`origin::RouteEvent`] for a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub enum Kind {
	Start,
	Update,
	Restart,
	End,
	Offline,
}

/// Every update per prefix the watcher has seen. Await [`kio::clock::sim::idle`]
/// first, so every link, hold and quarantine in the mesh has settled.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn drain(watched: &mut mpsc::UnboundedReceiver<(String, Kind)>) -> std::collections::HashMap<String, Vec<Kind>> {
	let mut updates = std::collections::HashMap::<String, Vec<Kind>>::new();
	while let Ok((prefix, kind)) = watched.try_recv() {
		updates.entry(prefix).or_default().push(kind);
	}
	updates
}

/// Watch `announced` from its own task, the way a session's announce writer does:
/// it runs when woken, between the relays' own tasks, rather than only once the
/// test task is polled again, which would coalesce every intermediate update.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn watch(mut announced: origin::Routes) -> mpsc::UnboundedReceiver<(String, Kind)> {
	let (tx, rx) = mpsc::unbounded();
	kio::clock::sim::spawn(async move {
		while let Some(event) = announced.next().await {
			let (kind, announce) = match event {
				origin::RouteEvent {
					kind: origin::RouteKind::Start,
					announce,
					..
				} => (Kind::Start, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::Update,
					announce,
					..
				} => (Kind::Update, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::Restart,
					announce,
					..
				} => (Kind::Restart, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::End,
					announce,
					..
				} => (Kind::End, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::Offline,
					announce,
					..
				} => (Kind::Offline, announce),
				other => panic!("unexpected route event {other:?}"),
			};
			if tx.unbounded_send((announce.prefix.to_string(), kind)).is_err() {
				break;
			}
		}
	});
	rx
}

/// Reads of the peer's GOAWAY over [`goaway::Consumer::poll`].
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub trait Draining {
	/// The peer's GOAWAY, or `None` if it has not sent one.
	fn peek(&self) -> Option<goaway::Goaway>;

	/// Wait for the peer's GOAWAY, or `None` if the session closes without one.
	async fn recv(&self) -> Option<goaway::Goaway>;
}

impl Draining for goaway::Consumer {
	fn peek(&self) -> Option<goaway::Goaway> {
		match self.poll(&kio::Waiter::noop()) {
			std::task::Poll::Ready(Ok(goaway)) => Some(goaway),
			_ => None,
		}
	}

	async fn recv(&self) -> Option<goaway::Goaway> {
		kio::wait(|waiter| self.poll(waiter)).await.ok()
	}
}

/// Every version this crate speaks, including the opt-in `-wip` drafts that
/// [`moq_net::Versions::all`] leaves out.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn every_version() -> Vec<moq_net::Version> {
	moq_net::Version::ALL.to_vec()
}

/// Whether a copy received over `version` can be asked for a group by key: lite-05/06 number
/// groups by sequence, so a copy over them answers only requests naming none.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn keyed(version: &moq_net::Version) -> bool {
	!matches!(version.alpn(), "moq-lite-05" | "moq-lite-06")
}

/// [`every_version`] whose copies can be asked for a group by key; see [`keyed`].
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn keyed_versions() -> Vec<moq_net::Version> {
	every_version().into_iter().filter(keyed).collect()
}

/// splitmix64, so every platform builds the same seeded cases.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub struct Rng(pub u64);

#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
impl Rng {
	pub fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
		let mut z = self.0;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		z ^ (z >> 31)
	}

	/// A value in `lo..=hi`.
	pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
		lo + self.next() % (hi - lo + 1)
	}

	/// An index into a slice of `len` items.
	pub fn pick(&mut self, len: usize) -> usize {
		(self.next() % len as u64) as usize
	}
}
