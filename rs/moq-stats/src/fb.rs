//! The `.fb.z` flavor: a FlatBuffers snapshot per frame (`stats.fbs`), each
//! group one DEFLATE window, so a frame repeating the last one compresses to
//! a few bytes without any delta encoding.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::task::Poll;

use bytes::Bytes;
use moq_net::stats::{Content, Presence, Traffic};
use moq_net::{PathOwned, group, kio, track};
use planus::{Builder, Offset, ReadAsRoot};

use crate::fbs::moq::stats as schema;
use crate::{Error, Result};

/// Frames per group before a fresh window is forced, bounding what a late
/// joiner inflates before it reaches the newest frame.
const MAX_GROUP_FRAMES: usize = 256;

/// A group rolls once the frames after its first exceed this many times the
/// first frame's compressed size, the same budget `.json.z` uses.
const GROUP_RATIO: u64 = 8;

/// A frame value with a FlatBuffers encoding: [`Traffic`] or [`Presence`].
pub(crate) trait Entry: Copy + Sized + 'static {
	/// The schema's entry table.
	type Table;

	/// Write one keyed entry.
	fn write(builder: &mut Builder, key: &str, value: &Self) -> Offset<Self::Table>;

	/// Write the frame root over `entries` and finish the buffer.
	fn finish<'a>(builder: &'a mut Builder, entries: &[Offset<Self::Table>]) -> &'a [u8];

	/// Read a finished buffer back into an owned frame.
	fn decode(buf: &[u8]) -> planus::Result<BTreeMap<String, Self>>;
}

impl Entry for Traffic {
	type Table = schema::TrafficEntry;

	fn write(builder: &mut Builder, key: &str, t: &Self) -> Offset<Self::Table> {
		// Absent fields read as zero, so a table of zeros is left out entirely.
		let stale = (t.stale != Content::default()).then(|| {
			schema::Content::create(
				builder,
				t.stale.bytes,
				t.stale.frames,
				t.stale.groups,
				t.stale.datagrams,
			)
		});
		let traffic = schema::Traffic::create(
			builder,
			t.announces_started,
			t.announces_ended,
			t.announced_bytes,
			t.broadcasts_started,
			t.broadcasts_ended,
			t.subscriptions_started,
			t.subscriptions_ended,
			t.fetches,
			t.bytes,
			t.frames,
			t.groups,
			t.datagrams,
			stale,
		);
		schema::TrafficEntry::create(builder, key, traffic)
	}

	fn finish<'a>(builder: &'a mut Builder, entries: &[Offset<Self::Table>]) -> &'a [u8] {
		let entries = builder.create_vector(entries);
		let root = schema::TrafficFrame::create(builder, entries);
		builder.finish(root, None)
	}

	fn decode(buf: &[u8]) -> planus::Result<BTreeMap<String, Self>> {
		let frame = schema::TrafficFrameRef::read_as_root(buf)?;
		let mut out = BTreeMap::new();
		for entry in frame.entries()? {
			let entry = entry?;
			let r = entry.traffic()?;
			let mut t = Traffic::default();
			t.announces_started = r.announces_started()?;
			t.announces_ended = r.announces_ended()?;
			t.announced_bytes = r.announced_bytes()?;
			t.broadcasts_started = r.broadcasts_started()?;
			t.broadcasts_ended = r.broadcasts_ended()?;
			t.subscriptions_started = r.subscriptions_started()?;
			t.subscriptions_ended = r.subscriptions_ended()?;
			t.fetches = r.fetches()?;
			t.bytes = r.bytes()?;
			t.frames = r.frames()?;
			t.groups = r.groups()?;
			t.datagrams = r.datagrams()?;
			if let Some(stale) = r.stale()? {
				t.stale.bytes = stale.bytes()?;
				t.stale.frames = stale.frames()?;
				t.stale.groups = stale.groups()?;
				t.stale.datagrams = stale.datagrams()?;
			}
			out.insert(entry.path()?.to_string(), t);
		}
		Ok(out)
	}
}

impl Entry for Presence {
	type Table = schema::SessionsEntry;

	fn write(builder: &mut Builder, key: &str, p: &Self) -> Offset<Self::Table> {
		let presence = schema::Presence::create(builder, p.sessions_started, p.sessions_ended);
		schema::SessionsEntry::create(builder, key, presence)
	}

	fn finish<'a>(builder: &'a mut Builder, entries: &[Offset<Self::Table>]) -> &'a [u8] {
		let entries = builder.create_vector(entries);
		let root = schema::SessionsFrame::create(builder, entries);
		builder.finish(root, None)
	}

	fn decode(buf: &[u8]) -> planus::Result<BTreeMap<String, Self>> {
		let frame = schema::SessionsFrameRef::read_as_root(buf)?;
		let mut out = BTreeMap::new();
		for entry in frame.entries()? {
			let entry = entry?;
			let r = entry.presence()?;
			let mut p = Presence::default();
			p.sessions_started = r.sessions_started()?;
			p.sessions_ended = r.sessions_ended()?;
			out.insert(entry.root()?.to_string(), p);
		}
		Ok(out)
	}
}

/// One encoded frame and the group boundary it implies.
pub(crate) struct Encoded {
	pub payload: Bytes,
	/// The first frame of a fresh window, which must open a new group.
	pub keyframe: bool,
}

/// The track-free half of a `.fb.z` track: entries in, frame payloads out.
/// Every buffer is kept across frames, so a steady frame only allocates its
/// owned payload.
pub(crate) struct Encoder<V: Entry> {
	builder: Builder,
	offsets: Vec<Offset<V::Table>>,
	/// The last emitted plaintext, so an unchanged frame is skipped.
	last: Vec<u8>,
	/// The open group's window, `None` until the first frame or after a reset.
	window: Option<Window>,
}

struct Window {
	flate: moq_flate::Encoder,
	/// Compressed size of the group's first frame.
	first: u64,
	/// Compressed bytes of every later frame.
	rest: u64,
	frames: usize,
}

impl<V: Entry> Encoder<V> {
	pub fn new() -> Self {
		Self {
			builder: Builder::new(),
			offsets: Vec::new(),
			last: Vec::new(),
			window: None,
		}
	}

	/// Encode a frame of sorted `entries`, `None` when it matches the last one.
	pub fn encode(&mut self, entries: &[(PathOwned, V)]) -> Result<Option<Encoded>> {
		let Self {
			builder,
			offsets,
			last,
			window,
		} = self;

		builder.clear();
		offsets.clear();
		for (key, value) in entries {
			offsets.push(V::write(builder, key.as_str(), value));
		}
		let plain = V::finish(builder, offsets);

		if window.is_some() && plain == last.as_slice() {
			return Ok(None);
		}
		// Every reader inflates with moq-flate's default cap, so a larger frame
		// could never be read back.
		if plain.len() as u64 > moq_flate::DEFAULT_MAX_FRAME_SIZE {
			return Err(moq_net::Error::FrameTooLarge.into());
		}
		last.clear();
		last.extend_from_slice(plain);

		if let Some(open) = window.as_mut()
			&& open.frames < MAX_GROUP_FRAMES
			&& open.rest <= GROUP_RATIO * open.first
		{
			let payload = open.flate.frame(plain);
			// A group past the cache bound would abort, stranding late joiners,
			// so roll a fresh window instead.
			if open.first + open.rest + payload.len() as u64 <= moq_net::group::MAX_CACHE_BYTES {
				open.rest += payload.len() as u64;
				open.frames += 1;
				return Ok(Some(Encoded {
					payload,
					keyframe: false,
				}));
			}
		}

		let mut flate = moq_flate::Encoder::new();
		let payload = flate.frame(plain);
		*window = Some(Window {
			flate,
			first: payload.len() as u64,
			rest: 0,
			frames: 1,
		});
		Ok(Some(Encoded {
			payload,
			keyframe: true,
		}))
	}

	/// Start the next frame in a fresh group, after a frame failed to land.
	pub fn reset(&mut self) {
		self.window = None;
	}
}

/// Publishes a `.fb.z` track: an [`Encoder`] plus the track and group it
/// writes to.
pub(crate) struct Writer<V: Entry> {
	track: track::Producer,
	group: Option<group::Producer>,
	encoder: Encoder<V>,
}

impl<V: Entry> Writer<V> {
	pub fn new(track: track::Producer) -> Self {
		Self {
			track,
			group: None,
			encoder: Encoder::new(),
		}
	}

	/// Whether any consumer still reads the track.
	pub fn is_used(&self) -> bool {
		self.track.is_used()
	}

	/// Publish a frame of sorted `entries`, skipping an unchanged one.
	pub fn publish(&mut self, entries: &[(PathOwned, V)]) -> Result<()> {
		let Some(frame) = self.encoder.encode(entries)? else {
			return Ok(());
		};
		if let Err(err) = self.write(frame) {
			// The window moved past what reached the wire; close the group so no
			// reader waits on it, and open the next frame in a fresh one.
			self.encoder.reset();
			if let Some(group) = self.group.take() {
				let _ = group.finish();
			}
			return Err(err.into());
		}
		Ok(())
	}

	fn write(&mut self, frame: Encoded) -> moq_net::Result<()> {
		if frame.keyframe {
			if let Some(group) = self.group.take() {
				group.finish()?;
			}
			self.group = Some(self.track.append_group()?);
		}
		let group = self.group.as_mut().expect("a keyframe opened the group");
		group.write_frame(moq_net::Timestamp::now(), frame.payload)
	}

	/// Finish the track. An error means it already ended.
	pub fn finish(&mut self) {
		if let Some(group) = self.group.take() {
			let _ = group.finish();
		}
		let _ = self.track.finish();
	}
}

/// The track-free half of a `.fb.z` reader: frame payloads in, frames out.
pub(crate) struct Decoder<V: Entry> {
	flate: moq_flate::Decoder,
	/// The newest inflated frame, reused across frames.
	buf: Vec<u8>,
	_marker: PhantomData<fn() -> V>,
}

impl<V: Entry> Decoder<V> {
	pub fn new() -> Self {
		Self {
			flate: moq_flate::Decoder::new(),
			buf: Vec::new(),
			_marker: PhantomData,
		}
	}

	/// Inflate the next frame of the group; `first` starts a fresh window.
	pub fn inflate(&mut self, payload: &[u8], first: bool) -> Result<()> {
		if first {
			self.flate = moq_flate::Decoder::new();
		}
		Ok(self.flate.frame_into(payload, &mut self.buf)?)
	}

	/// Decode the newest inflated frame.
	pub fn decode(&self) -> Result<BTreeMap<String, V>> {
		V::decode(&self.buf).map_err(|err| Error::FlatBuffers(err.to_string()))
	}
}

/// Reads a `.fb.z` track, yielding the newest frame; a slow reader collapses
/// the backlog, which is safe because every frame is a full snapshot.
pub(crate) struct Reader<V: Entry> {
	track: track::Ordered,
	group: Option<group::Consumer>,
	/// Whether the next frame read is its group's first.
	first: bool,
	decoder: Decoder<V>,
}

impl<V: Entry> Reader<V> {
	pub fn new(track: track::Subscriber) -> Self {
		Self {
			track: track.ordered(),
			group: None,
			first: true,
			decoder: Decoder::new(),
		}
	}

	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<BTreeMap<String, V>>>> {
		let finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => {
					self.group = Some(group);
					self.first = true;
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		// Every frame inflates in order, since each one's window builds on the
		// last, but only the newest is decoded.
		let mut advanced = false;
		let mut pending = false;
		while let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					self.decoder.inflate(&frame.payload, self.first)?;
					self.first = false;
					advanced = true;
				}
				Poll::Ready(Ok(None)) => {
					self.group = None;
					break;
				}
				// The group is gone (superseded, evicted, or lagged). Only the
				// newest snapshot matters, so wait for the next group.
				Poll::Ready(Err(err)) => {
					tracing::warn!(track = self.track.name(), error = ?err, "stats group lost; waiting for a newer one");
					self.group = None;
					break;
				}
				Poll::Pending => {
					pending = true;
					break;
				}
			}
		}

		if advanced {
			return Poll::Ready(self.decoder.decode().map(Some));
		}
		if pending || !finished {
			return Poll::Pending;
		}
		Poll::Ready(Ok(None))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn traffic(bytes: u64) -> Traffic {
		let mut t = Traffic::default();
		t.announces_started = 1;
		t.subscriptions_started = 2;
		t.bytes = bytes;
		t.stale.frames = 3;
		t
	}

	#[test]
	fn traffic_round_trips() {
		let entries = vec![
			(PathOwned::from("a/cam"), traffic(10)),
			(PathOwned::from("b/cam"), Traffic::default()),
		];
		let mut encoder = Encoder::<Traffic>::new();
		let mut decoder = Decoder::<Traffic>::new();

		let frame = encoder.encode(&entries).unwrap().expect("first frame");
		assert!(frame.keyframe);
		decoder.inflate(&frame.payload, true).unwrap();
		let decoded = decoder.decode().unwrap();
		assert_eq!(decoded.len(), 2);
		assert_eq!(decoded["a/cam"], traffic(10));
		assert_eq!(decoded["b/cam"], Traffic::default());

		// Unchanged: nothing to write.
		assert!(encoder.encode(&entries).unwrap().is_none());

		// Changed: rides the open window.
		let entries = vec![(PathOwned::from("a/cam"), traffic(20))];
		let frame = encoder.encode(&entries).unwrap().expect("changed");
		assert!(!frame.keyframe);
		decoder.inflate(&frame.payload, false).unwrap();
		assert_eq!(decoder.decode().unwrap()["a/cam"].bytes, 20);

		// A reset reopens a window, even for an unchanged frame.
		encoder.reset();
		let frame = encoder.encode(&entries).unwrap().expect("resynced");
		assert!(frame.keyframe);
		decoder.inflate(&frame.payload, true).unwrap();
		assert_eq!(decoder.decode().unwrap()["a/cam"].bytes, 20);
	}

	#[test]
	fn presence_round_trips() {
		let mut p = Presence::default();
		p.sessions_started = 5;
		p.sessions_ended = 2;
		let entries = vec![
			(PathOwned::from("acme"), p),
			(PathOwned::from("empty"), Presence::default()),
		];

		let mut encoder = Encoder::<Presence>::new();
		let mut decoder = Decoder::<Presence>::new();
		let frame = encoder.encode(&entries).unwrap().unwrap();
		decoder.inflate(&frame.payload, true).unwrap();
		let decoded = decoder.decode().unwrap();
		assert_eq!(decoded["acme"], p);
		assert_eq!(decoded["empty"], Presence::default());
	}

	#[test]
	fn empty_frame_round_trips() {
		let mut encoder = Encoder::<Traffic>::new();
		let mut decoder = Decoder::<Traffic>::new();
		let frame = encoder.encode(&[]).unwrap().unwrap();
		decoder.inflate(&frame.payload, true).unwrap();
		assert!(decoder.decode().unwrap().is_empty());
	}

	#[test]
	fn malformed_frame_fails_loud() {
		let mut flate = moq_flate::Encoder::new();
		let payload = flate.frame(b"not a flatbuffer");
		let mut decoder = Decoder::<Traffic>::new();
		decoder.inflate(&payload, true).unwrap();
		assert!(matches!(decoder.decode(), Err(Error::FlatBuffers(_))));
	}

	#[test]
	fn groups_roll_within_budget() {
		let mut encoder = Encoder::<Traffic>::new();
		let mut keyframes = 0;
		for tick in 0..(MAX_GROUP_FRAMES as u64 * 2) {
			let entries = [(PathOwned::from("a/cam"), traffic(tick))];
			let frame = encoder.encode(&entries).unwrap().unwrap();
			keyframes += usize::from(frame.keyframe);
		}
		assert!(keyframes >= 2, "a group never exceeds MAX_GROUP_FRAMES");
	}

	/// A changed frame costs a constant number of allocations however many
	/// entries it carries: planus's staged offsets, and the owned payload.
	#[test]
	fn changed_frame_allocates_per_frame_not_per_entry() {
		for entries in [1, 64, 1024] {
			let paths: Vec<PathOwned> = (0..entries).map(|i| PathOwned::from(format!("room{i}/cam"))).collect();
			let frame = |tick: u64| -> Vec<(PathOwned, Traffic)> {
				paths.iter().map(|path| (path.clone(), traffic(tick))).collect()
			};
			let (warm, measured) = (frame(1), frame(2));

			let mut encoder = Encoder::<Traffic>::new();
			encoder.encode(&warm).unwrap().expect("first");
			encoder.encode(&frame(3)).unwrap().expect("changed");

			let before = crate::counting::allocs();
			let encoded = encoder.encode(&measured).unwrap().expect("changed");
			let allocs = crate::counting::allocs() - before;
			assert!(!encoded.keyframe, "stays in the open window");
			assert!(allocs <= 3, "{allocs} allocations for {entries} entries");
		}
	}

	/// The checked-in `src/fbs.rs` matches what planus generates from
	/// `stats.fbs`. Set `MOQ_STATS_BLESS=1` to regenerate it.
	///
	/// Paths stay relative to the crate root (cargo's test working directory),
	/// since the generated docs name the schema file and rustfmt picks up the
	/// repository's config from there.
	#[test]
	fn fbs_is_current() {
		let declarations = planus_translation::translate_files(&["stats.fbs"]).expect("stats.fbs must translate");
		let generated = planus_codegen::generate_rust(&declarations, true).expect("generate");
		let path = "src/fbs.rs";
		if std::env::var_os("MOQ_STATS_BLESS").is_some() {
			std::fs::write(path, generated).expect("write src/fbs.rs");
			return;
		}
		let current = std::fs::read_to_string(path).expect("read src/fbs.rs");
		assert!(
			current == generated,
			"src/fbs.rs is stale; run `MOQ_STATS_BLESS=1 cargo test -p moq-stats fbs_is_current`"
		);
	}
}
