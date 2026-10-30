//! The `ingest` target: a group stream's bytes through the incremental ingest a session
//! runs, moq-lite's [`lite::FrameIngest`] or moq-transport's [`ietf::GroupIngest`].
//!
//! The ingest is a state machine resumed wherever a read runs short, so what it builds
//! must not depend on where the transport split the stream. Each input runs whole, then
//! a byte at a time, then in chunks the input picks, and all three must leave the same
//! frames and the same ending. Every byte a frame holds came off the stream, and on a
//! clean end every byte of the stream is accounted for by a frame or its header.

use bytes::Bytes;

use super::Chunks;
use crate::{
	Timescale, broadcast, cache,
	coding::{self, Decode},
	group, ietf, lite,
	model::object::{Fields, TrackProperties},
	track,
};

/// The longest varint either codec writes: nine bytes for leading-ones, eight for QUIC.
const MAX_VARINT: usize = 9;

/// One frame as a relay reads it back out of the group.
#[derive(Debug, PartialEq)]
struct Seen {
	timestamp: Option<crate::Timestamp>,
	payload: Bytes,
	/// The Object ID and property block the frame kept, if it was not plain.
	fields: Option<(u64, Option<Bytes>)>,
	inflated: Option<u64>,
}

/// What one run of the ingest left behind.
#[derive(Debug, PartialEq)]
struct Ingested {
	frames: Vec<Seen>,
	/// How the stream ended: cleanly with what it said, or refused with the error.
	end: Result<String, String>,
}

/// How a run reads the stream: the session's settings, decoded from the input's first
/// bytes, and the stream itself.
#[derive(Clone, Copy)]
enum Session {
	Lite {
		version: lite::Version,
		/// The session negotiated the `ietf` extension, so each frame follows a record.
		records: bool,
		frame_start: u64,
	},
	Ietf {
		version: ietf::Version,
	},
}

/// The track a stream's group belongs to.
fn track_info(timed: bool, compress: bool, draft: Option<ietf::Version>) -> track::Info {
	let mut info = track::Info::default();
	if timed {
		info = info.with_timescale(Timescale::MILLI);
	}
	info.compress = compress;
	info.properties = draft.map(|version| {
		std::sync::Arc::new(TrackProperties {
			version,
			values: Vec::new(),
		})
	});
	info
}

/// Decode a group stream from arbitrary bytes.
///
/// Byte 0 picks the protocol and version, byte 1 the track's settings, byte 2 the chunk
/// size of the third run, and the rest is the stream. For moq-transport the stream opens
/// with the subgroup header, which is decoded first, as the session peeks it.
pub fn ingest(data: &[u8]) -> bool {
	let [selector, settings, chunk, rest @ ..] = data else {
		return false;
	};
	let timed = settings & 0x1 != 0;
	let compress = settings & 0x2 != 0;
	let draft = super::IETF_VERSIONS.get(((settings >> 2) & 0x3) as usize).copied();
	let chunk = (*chunk as usize).max(1);

	let (session, info, body) = match selector % 2 {
		0 => {
			let version = super::LITE_VERSIONS[(*selector as usize / 2) % super::LITE_VERSIONS.len()];
			// Lite05/06 timestamps may be send times, so their tracks arrive untimed.
			let timed = timed && version.has_media_time();
			let session = Session::Lite {
				version,
				records: settings & 0x10 != 0 && version.has_ietf(),
				frame_start: (settings >> 5) as u64,
			};
			// Only lite-07 can mark a track compressed.
			let compress = compress && version == lite::Version::Lite07;
			(session, track_info(timed, compress, draft), rest)
		}
		_ => {
			let version = super::IETF_VERSIONS[(*selector as usize / 2) % super::IETF_VERSIONS.len()];
			// A moq-transport track was always published over moq-transport.
			(
				Session::Ietf { version },
				track_info(timed, compress, Some(version)),
				rest,
			)
		}
	};

	let whole = run(session, &info, body, usize::MAX);
	let bytewise = run(session, &info, body, 1);
	assert_eq!(whole, bytewise, "a byte at a time ingests differently");
	if chunk != 1 {
		let chunked = run(session, &info, body, chunk);
		assert_eq!(whole, chunked, "{chunk}-byte chunks ingest differently");
	}

	let Some(whole) = whole else {
		return false;
	};
	check(session, &info, body, &whole);
	whole.end.is_ok()
}

/// Hold one run to what the stream could have said.
fn check(session: Session, info: &track::Info, body: &[u8], ingested: &Ingested) {
	let mut ids = None;
	let mut payload = 0;
	for (index, frame) in ingested.frames.iter().enumerate() {
		assert_eq!(
			frame.timestamp.is_some(),
			info.timescale.is_some(),
			"frame {index} does not match its track's timeline"
		);
		assert_eq!(
			frame.inflated.is_some(),
			info.compress,
			"frame {index} lost its inflated size"
		);
		let id = frame.fields.as_ref().map_or(index as u64, |(id, _)| *id);
		assert!(
			ids.is_none_or(|prior| id > prior),
			"Object ID {id} does not follow {ids:?}"
		);
		ids = Some(id);
		payload += frame.payload.len();
	}

	// Each frame has at least a one-byte size ahead of its payload.
	let frames = ingested.frames.len();
	assert!(
		payload + frames <= body.len(),
		"{frames} frames hold {payload} bytes from a {}-byte stream",
		body.len()
	);

	// On a clean end nothing was skipped: every byte is a frame, a header field, or a
	// property block. A moq-transport extension block is decoded rather than kept, so its
	// length is unknown and the bound only applies to a subgroup without them.
	let Ok(_) = &ingested.end else { return };
	let (header, extensions) = subgroup_header(session, body);
	let fields = match session {
		Session::Lite { records, .. } => {
			// Timing, inflated size, and size; a record adds its code and block length.
			let fields = 3 + 2 * records as usize;
			// The Group State, and an END_OF_GROUP record with its block length.
			let trailer = (1 + 2 * records as usize) * MAX_VARINT;
			let blocks: usize = ingested
				.frames
				.iter()
				.filter_map(|frame| frame.fields.as_ref()?.1.as_ref())
				.map(Bytes::len)
				.sum();
			Some((fields, trailer, blocks))
		}
		// The ID delta and the size, then an empty object's status. A status object that
		// ends the group, or the track, repeats them.
		Session::Ietf { .. } => (!extensions).then_some((3, 3 * MAX_VARINT, 0)),
	};
	if let Some((fields, trailer, blocks)) = fields {
		let most = header + payload + blocks + frames * fields * MAX_VARINT + trailer;
		assert!(
			body.len() <= most,
			"{frames} frames of {payload} bytes account for at most {most} of a {}-byte stream",
			body.len()
		);
	}
}

/// The subgroup header's length, which a moq-transport run decodes before its objects,
/// and whether its objects carry extension blocks.
fn subgroup_header(session: Session, body: &[u8]) -> (usize, bool) {
	match session {
		Session::Lite { .. } => (0, false),
		Session::Ietf { version } => ietf::GroupHeader::decode_slice(body, version)
			.map_or((0, false), |(header, used)| (used, header.flags.has_extensions)),
	}
}

/// Run the ingest over `body`, delivered `chunk` bytes at a time, and read back the group.
///
/// `None` when the stream does not open a group at all.
fn run(session: Session, info: &track::Info, body: &[u8], chunk: usize) -> Option<Ingested> {
	let mut broadcast = broadcast::Info::new();
	// Unbounded, so eviction never aborts a group mid-frame.
	broadcast.pool = cache::Pool::unbounded();
	let broadcast = broadcast.produce();
	let track = broadcast.create_track("ingest", info.clone()).ok()?;

	let (end, group) = match session {
		Session::Lite {
			version,
			records,
			frame_start,
		} => {
			let mut reader = reader(Bytes::copy_from_slice(body), chunk, version);
			let producer = track.append_group().ok()?;
			let consumer = producer.consume();
			let mut group = crate::recv::Group::new(producer);
			let framing = lite::Framing::new(info, version);
			let mut ingest = lite::FrameIngest::new(framing, version, records, frame_start);
			let end = drive(body.len(), |waiter| ingest.poll(&mut reader, &mut group, waiter));
			(end.map(|()| "fin".to_string()), (group, consumer))
		}
		Session::Ietf { version } => {
			let (header, used) = ietf::GroupHeader::decode_slice(body, version).ok()?;
			let mut reader = reader(Bytes::copy_from_slice(&body[used..]), chunk, version);
			let producer = track
				.create_group(group::Info {
					sequence: header.group_id,
				})
				.ok()?;
			producer
				.set_wire(group::Wire {
					sparse: false,
					first_object: header.flags.first_object,
					first_id: 0,
					extensions: header.flags.has_extensions,
					has_end: header.flags.has_end,
					fetchable: true,
					last: None,
				})
				.ok()?;
			let consumer = producer.consume();
			let mut group = crate::recv::Group::new(producer);
			let mut ingest = ietf::GroupIngest::new(version, &header, info.timescale);
			let end = drive(body.len(), |waiter| ingest.poll(&mut reader, &mut group, waiter));
			(end.map(|ended| format!("{ended:?}")), (group, consumer))
		}
	};

	let (group, consumer) = group;
	// Settle the group as the session does, so the reader below never waits.
	match &end {
		Ok(_) => {
			let _ = group.finish();
		}
		Err(err) => {
			let _ = group.abort(err.clone());
		}
	}

	Some(Ingested {
		frames: read(consumer, info.compress),
		end: end.map_err(|err| err.to_string()),
	})
}

type Stream<V> = coding::Reader<crate::transport::poll::Buffered<Chunks>, V>;

fn reader<V: coding::StreamCodes>(payload: Bytes, chunk: usize, version: V) -> Stream<V> {
	let stream = Chunks {
		payload,
		chunk,
		ready: true,
	};
	coding::Reader::new(crate::transport::poll::Buffered::new(stream), version)
}

/// Poll the ingest until it settles.
///
/// The stream hands over a chunk every other poll and the ingest yields every few frames,
/// so a stream of `len` bytes settles within a few polls per byte. One that does not is
/// waiting on something other than the stream, which a peer could hold forever.
fn drive<T>(
	len: usize,
	mut poll: impl FnMut(&kio::Waiter) -> std::task::Poll<Result<T, crate::Error>>,
) -> Result<T, crate::Error> {
	let waiter = &kio::Waiter::noop();
	for _ in 0..4 * len + 64 {
		if let std::task::Poll::Ready(res) = poll(waiter) {
			return res;
		}
	}
	panic!("the ingest stalled on a complete {len}-byte stream");
}

/// Read every frame the group holds, in its stored form.
fn read(consumer: group::Consumer, compress: bool) -> Vec<Seen> {
	let mut consumer = match compress {
		true => consumer.compressed(),
		false => consumer,
	};
	let waiter = &kio::Waiter::noop();
	let mut frames = Vec::new();
	while let std::task::Poll::Ready(Ok(Some(object))) = consumer.poll_read_object(waiter) {
		let fields = object.fields.as_deref().map(|Fields { id, properties }| {
			let block = properties.block.as_ref().map(|block| block.bytes.clone());
			(*id, block)
		});
		frames.push(Seen {
			timestamp: object.frame.timestamp,
			payload: object.frame.payload.contiguous(0),
			fields,
			inflated: object.inflated,
		});
	}
	frames
}

/// The streams the seeds open with: one group per protocol, built from our own framing.
pub(super) fn seeds() -> Vec<super::Seed> {
	let mut seeds = Vec::new();
	let mut push = |data: Vec<u8>| {
		seeds.push(super::Seed {
			target: "ingest",
			kind: 0,
			data,
		})
	};

	// moq-lite: three plain frames, at every version, timed and untimed. The timing
	// prefix is a zigzag delta, so 0x02 steps the clock forward by one.
	for (index, version) in super::LITE_VERSIONS.iter().enumerate() {
		for timed in [false, true] {
			let mut data = vec![(index * 2) as u8, timed as u8, 3];
			let timing = version.wire_timescale(timed.then_some(Timescale::MILLI)).is_some();
			for (n, payload) in [&b"abc"[..], b"", b"defgh"].into_iter().enumerate() {
				if timing {
					data.push(if n == 0 { 0 } else { 2 });
				}
				data.push(payload.len() as u8);
				data.extend_from_slice(payload);
			}
			push(data);
		}
	}

	// moq-lite-07 with the `ietf` extension: a plain Group State, then records.
	let lite07 = super::LITE_VERSIONS.len() - 1;
	let mut data = vec![(lite07 * 2) as u8, 0x10, 2, 0];
	for payload in [&b"ab"[..], b"c"] {
		// Object 0 is the first record's absolute ID, then each next one is a zero delta,
		// and neither has a property block.
		data.extend_from_slice(&[0, 0, payload.len() as u8]);
		data.extend_from_slice(payload);
	}
	push(data);

	// moq-transport: a subgroup header, then objects, each ID delta, size, payload.
	for (index, version) in super::IETF_VERSIONS.iter().enumerate() {
		let Some(encoded) = subgroup(*version, false) else {
			continue;
		};
		let mut data = vec![(index * 2 + 1) as u8, 0, 3];
		data.extend_from_slice(&encoded);
		// Object 0, then a zero delta to object 1.
		for payload in [&b"abc"[..], b"de"] {
			data.extend_from_slice(&[0, payload.len() as u8]);
			data.extend_from_slice(payload);
		}
		// An empty object with status 0, a plain zero-length frame.
		data.extend_from_slice(&[0, 0, 0]);
		push(data);
	}

	seeds
}

/// Streams that declare a size at or one past a cap, ahead of far fewer bytes, so the
/// fuzzer starts beside each rejection path instead of searching for its bound: a
/// frame's size, a moq-lite record's property block, and a moq-transport object's
/// extension block.
pub(super) fn limit_seeds() -> Vec<super::Seed> {
	let mut seeds = Vec::new();
	let mut push = |data: Vec<u8>| {
		seeds.push(super::Seed {
			target: "ingest",
			kind: 0,
			data,
		})
	};

	for (index, version) in super::IETF_VERSIONS.iter().enumerate() {
		let Some(encoded) = subgroup(*version, false) else {
			continue;
		};
		let varint = |value: u64| {
			let mut out = Vec::new();
			coding::Encoder::new(&mut out, (*version).into()).varint(value).unwrap();
			out
		};
		for size in super::limits(&[group::MAX_CACHE_BYTES]) {
			let mut data = vec![(index * 2 + 1) as u8, 0, 3];
			data.extend_from_slice(&encoded);
			data.push(0);
			data.extend(varint(size));
			data.extend_from_slice(&[0; 4]);
			push(data);
		}
		let Some(encoded) = subgroup(*version, true) else {
			continue;
		};
		for size in super::limits(&[ietf::MAX_OBJECT_EXTENSIONS as u64]) {
			let mut data = vec![(index * 2 + 1) as u8, 0, 3];
			data.extend_from_slice(&encoded);
			data.push(0);
			data.extend(varint(size));
			data.extend_from_slice(&[0; 4]);
			push(data);
		}
	}

	let lite07 = super::LITE_VERSIONS.len() - 1;
	// The caps on moq-lite-07: a frame's size, and a record's property block.
	let varint = |value: u64| {
		let mut out = Vec::new();
		coding::Encoder::new(&mut out, lite::Version::Lite07.into())
			.varint(value)
			.unwrap();
		out
	};
	for size in super::limits(&[group::MAX_CACHE_BYTES]) {
		let mut data = vec![(lite07 * 2) as u8, 0, 3];
		data.extend(varint(size));
		data.extend_from_slice(&[0; 4]);
		push(data);
	}
	for size in super::limits(&[lite::ietf::MAX_PROPERTIES as u64]) {
		let mut data = vec![(lite07 * 2) as u8, 0x10, 3, 0, 0];
		data.extend(varint(size));
		data.extend_from_slice(&[0; 4]);
		push(data);
	}

	seeds
}

/// Group 0's subgroup header, with or without object extensions.
fn subgroup(version: ietf::Version, has_extensions: bool) -> Option<Vec<u8>> {
	let header = ietf::GroupHeader {
		track_alias: 1,
		group_id: 0,
		sub_group_id: 0,
		publisher_priority: 0,
		flags: ietf::GroupFlags {
			has_extensions,
			..Default::default()
		},
	};
	coding::Encode::encode_bytes(&header, version).ok().map(Vec::from)
}
