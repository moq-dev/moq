//! A step of the source's clock onto another timeline ends the import's instance.
//!
//! The stimulus is a real capture played twice, the second pass on another clock: the whole
//! multiplex steps at one packet, with every continuity counter broken once and no
//! `discontinuity_indicator`, as a changeover between two unsynchronised encoders does.

use std::time::Duration;

use super::{Ext, Import, Restart, Step};

/// 5 s of H.264, AAC and SCTE-35, with a PCR.
const CLIP: &[u8] = include_bytes!("test_data/scte35/bbb5s.ts");
const HZ: i64 = 90_000;
const WRAP: i64 = 1 << 33;
/// The step the issue's contribution took: two encoders started about 4 h 40 m apart.
const SWITCH: i64 = 16_844 * HZ + HZ / 2;

/// Which clock a timestamp belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clock {
	Pcr,
	Pes(u16),
}

fn pid(pkt: &[u8]) -> u16 {
	(u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2])
}

/// The PCR base (90 kHz) a packet carries, and where it sits.
fn pcr_at(pkt: &[u8]) -> Option<(usize, i64)> {
	if pkt[3] & 0x20 == 0 || pkt[4] < 7 || pkt[5] & 0x10 == 0 {
		return None;
	}
	let base = (i64::from(pkt[6]) << 25)
		| (i64::from(pkt[7]) << 17)
		| (i64::from(pkt[8]) << 9)
		| (i64::from(pkt[9]) << 1)
		| (i64::from(pkt[10]) >> 7);
	Some((6, base))
}

/// Where a PES start's PTS and DTS fields sit in its packet, when its header is whole.
fn pes_fields(pkt: &[u8]) -> Vec<usize> {
	if pkt[1] & 0x40 == 0 || pkt[3] & 0x10 == 0 {
		return Vec::new();
	}
	let start = if pkt[3] & 0x20 != 0 { 5 + usize::from(pkt[4]) } else { 4 };
	let Some(pes) = pkt.get(start..start + 19) else {
		return Vec::new();
	};
	if pes[..3] != [0, 0, 1] {
		return Vec::new();
	}
	match pes[7] >> 6 {
		0b10 => vec![start + 9],
		0b11 => vec![start + 9, start + 14],
		_ => Vec::new(),
	}
}

fn read_timestamp(field: &[u8]) -> i64 {
	(i64::from(field[0] >> 1 & 0x07) << 30)
		| (i64::from(field[1]) << 22)
		| (i64::from(field[2] >> 1) << 15)
		| (i64::from(field[3]) << 7)
		| i64::from(field[4] >> 1)
}

fn write_timestamp(field: &mut [u8], t: i64) {
	field[0] = (field[0] & 0xf1) | ((t >> 29) as u8 & 0x0e);
	field[1] = (t >> 22) as u8;
	field[2] = ((t >> 14) as u8 & 0xfe) | 1;
	field[3] = (t >> 7) as u8;
	field[4] = ((t << 1) as u8 & 0xfe) | 1;
}

/// `ts` with each PCR and PES timestamp moved by `by(clock)` 90 kHz ticks, modulo the wrap.
fn shift(ts: &[u8], by: impl Fn(Clock) -> i64) -> Vec<u8> {
	let mut out = ts.to_vec();
	for pkt in out.as_chunks_mut::<188>().0 {
		let pid = pid(pkt);
		if let Some((at, base)) = pcr_at(pkt) {
			let base = (base + by(Clock::Pcr)).rem_euclid(WRAP);
			pkt[at] = (base >> 25) as u8;
			pkt[at + 1] = (base >> 17) as u8;
			pkt[at + 2] = (base >> 9) as u8;
			pkt[at + 3] = (base >> 1) as u8;
			pkt[at + 4] = (pkt[at + 4] & 0x7f) | ((base as u8 & 1) << 7);
		}
		for at in pes_fields(pkt) {
			let t = (read_timestamp(&pkt[at..at + 5]) + by(Clock::Pes(pid))).rem_euclid(WRAP);
			write_timestamp(&mut pkt[at..at + 5], t);
		}
	}
	out
}

/// The program clock the clip spans, first PCR to last, in 90 kHz ticks.
fn span(ts: &[u8]) -> i64 {
	let pcrs: Vec<i64> = ts
		.as_chunks::<188>()
		.0
		.iter()
		.filter_map(|pkt| pcr_at(pkt).map(|(_, base)| base))
		.collect();
	pcrs.last().unwrap() - pcrs.first().unwrap()
}

/// The clip, then the clip again on a clock `gap` past where the first pass ended, each clock
/// moved by `by` on top. Returns where the second pass starts.
fn replay(gap: i64, by: impl Fn(Clock) -> i64) -> (Vec<u8>, usize) {
	let mut data = CLIP.to_vec();
	let second = span(CLIP) + gap;
	data.extend(shift(CLIP, |clock| second + by(clock)));
	(data, CLIP.len())
}

/// The importer, its catalog kept alive, fed `data` in `chunk`-byte pieces until it fails.
/// Returns the failure, if any, and the input it did not take: a restart's remainder followed
/// by the pieces never fed.
fn feed(data: &[u8], chunk: usize) -> (Option<anyhow::Error>, Vec<u8>) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let mut import = Import::new(broadcast, catalog.reserve());
	for (index, piece) in data.chunks(chunk).enumerate() {
		if let Err(err) = import.decode(piece) {
			let Some(restart) = err.downcast_ref::<Restart>() else {
				return (Some(err), Vec::new());
			};
			let mut rest = restart.remainder.to_vec();
			rest.extend(data.chunks(chunk).skip(index + 1).flatten());
			// The instance before the step ends cleanly.
			import.finish().unwrap();
			return (Some(err), rest);
		}
	}
	(import.finish().err(), Vec::new())
}

/// The input from the first packet of `pass` that carries a timestamp.
fn first_timed(pass: &[u8]) -> &[u8] {
	let at = pass
		.as_chunks::<188>()
		.0
		.iter()
		.position(|pkt| pcr_at(pkt).is_some() || !pes_fields(pkt).is_empty())
		.unwrap();
	&pass[at * 188..]
}

/// A switch to an encoder whose clock started hours away restarts the import either way, at
/// the switch, however the input is split.
#[test]
fn an_unflagged_switch_restarts_either_way() {
	for (gap, expected) in [(-SWITCH, "rewind"), (SWITCH, "leap")] {
		let (data, second) = replay(gap, |_| 0);
		for chunk in [data.len(), 188 * 7, 1_000] {
			let (err, rest) = feed(&data, chunk);
			let err = err.expect("the switch ends the instance");
			let restart = err.downcast_ref::<Restart>().expect("a restart");
			let by = match (restart.step, expected) {
				(Step::Rewind { by, flagged: false }, "rewind") | (Step::Leap { by }, "leap") => by,
				(step, _) => panic!("{expected}: {step:?}"),
			};
			let switch = Duration::from_micros((SWITCH * 1_000_000 / HZ) as u64);
			assert!(by.abs_diff(switch) < Duration::from_secs(1), "{expected} by {by:?}");

			// The new instance takes everything from the second pass's first timestamp, which
			// is where the old one stopped reading: none of it is published twice or lost.
			assert!(data[second..].ends_with(&rest), "{expected} {chunk}");
			assert!(rest.len() >= first_timed(&data[second..]).len(), "{expected} {chunk}");

			// What follows imports as a broadcast of its own.
			let (err, _) = feed(&rest, chunk);
			assert!(err.is_none(), "{expected}: {err:?}");
		}
	}
}

/// A clock that steps forward by less than the bound, such as across lost packets, carries
/// the same instance on.
#[test]
fn a_step_under_the_bound_carries_on() {
	let (data, _) = replay(9 * HZ, |_| 0);
	let (err, _) = feed(&data, 188 * 7);
	assert!(err.is_none(), "{err:?}");

	// Two seconds of the clip lost in the middle.
	let packets = CLIP.len() / 188;
	let mut lossy = CLIP[..packets / 3 * 188].to_vec();
	lossy.extend_from_slice(&CLIP[packets * 2 / 3 * 188..]);
	let (err, _) = feed(&lossy, 188 * 7);
	assert!(err.is_none(), "{err:?}");
}

/// A stream the new source does not carry is waited for only as long as a cadenced stream may
/// stay silent: the clocks that stepped decide, and the instance does not stall.
#[test]
fn a_stream_the_new_source_lacks_is_not_waited_for() {
	let (mut data, second) = replay(-SWITCH, |_| 0);
	let audio = audio_pid(CLIP);
	let without: Vec<u8> = data[second..]
		.as_chunks::<188>()
		.0
		.iter()
		.filter(|pkt| pid(&pkt[..]) != audio)
		.flatten()
		.copied()
		.collect();
	data.truncate(second);
	data.extend(&without);

	let (err, rest) = feed(&data, 188 * 7);
	let err = err.expect("the switch ends the instance");
	let restart = err.downcast_ref::<Restart>().expect("a restart");
	assert!(
		matches!(restart.step, Step::Rewind { flagged: false, .. }),
		"{restart:?}"
	);
	assert!(without.ends_with(&rest) && rest.len() >= first_timed(&without).len());
}

/// One clock stepping without the rest is not another source, so the import judges it as
/// before: a PCR alone, or the streams with a stream left behind on the old timeline.
#[test]
fn a_clock_stepping_alone_is_no_switch() {
	let (data, _) = replay(HZ, |clock| if clock == Clock::Pcr { -SWITCH } else { 0 });
	let (err, _) = feed(&data, 188 * 7);
	assert!(err.as_ref().is_none_or(|err| !err.is::<Restart>()), "{err:?}");

	// The audio carries on a second after the first pass while the rest rewinds.
	let audio = audio_pid(CLIP);
	let (data, _) = replay(
		-SWITCH,
		|clock| if clock == Clock::Pes(audio) { SWITCH + HZ } else { 0 },
	);
	let (err, _) = feed(&data, 188 * 7);
	assert!(err.as_ref().is_none_or(|err| !err.is::<Restart>()), "{err:?}");
}

/// The PID of the clip's audio: the PES that are not video.
fn audio_pid(ts: &[u8]) -> u16 {
	ts.as_chunks::<188>()
		.0
		.iter()
		.find_map(|pkt| {
			let at = *pes_fields(pkt).first()?;
			(!(0xe0..=0xef).contains(&pkt[at - 9 + 3])).then(|| pid(pkt))
		})
		.unwrap()
}
