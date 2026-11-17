//! A change of source, its clock stepping onto another timeline or its PAT naming another
//! program, ends the import's instance.
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
pub(super) const SWITCH: i64 = 16_844 * HZ + HZ / 2;

/// Which clock a timestamp belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Clock {
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
pub(super) fn shift(ts: &[u8], by: impl Fn(Clock) -> i64) -> Vec<u8> {
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

/// The importer, its catalog kept alive, fed `data` in `chunk`-byte pieces until it restarts or
/// fails. A restart comes with the input the importer did not take: its remainder followed by
/// the pieces never fed.
fn feed(data: &[u8], chunk: usize) -> Result<Option<(Restart, Vec<u8>)>, super::Error> {
	let (_catalog, mut import) = open();
	for (index, piece) in data.chunks(chunk).enumerate() {
		if let Some(restart) = import.decode_or_restart(piece)? {
			let mut rest = restart.remainder.to_vec();
			rest.extend(data.chunks(chunk).skip(index + 1).flatten());
			// The instance before the step ends cleanly.
			let _finished = import.finish().unwrap();
			return Ok(Some((restart, rest)));
		}
	}
	let _finished = import.finish()?;
	Ok(None)
}

/// An importer onto a fresh broadcast, with the catalog it must outlive.
fn open() -> (crate::catalog::Producer<Ext>, Import<Ext>) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let import = Import::new(broadcast, catalog.reserve());
	(catalog, import)
}

/// Edit the one PSI section `pkt` starts behind its pointer_field, then mend its CRC.
fn mend(pkt: &mut [u8; 188], edit: impl FnOnce(&mut [u8])) {
	assert_eq!(pkt[1] & 0x40, 0x40, "a section starts here");
	let payload = if pkt[3] & 0x20 != 0 { 5 + usize::from(pkt[4]) } else { 4 };
	let section = payload + 1 + usize::from(pkt[payload]);
	let len = 3 + (usize::from(pkt[section + 1] & 0x0f) << 8 | usize::from(pkt[section + 2]));
	edit(&mut pkt[section..section + len - 4]);
	let crc = super::psi::CRC.checksum(&pkt[section..section + len - 4]);
	pkt[section + len - 4..section + len].copy_from_slice(&crc.to_be_bytes());
}

/// The clip with its PAT naming `transport_stream_id` 7.
fn retagged(ts: &[u8]) -> Vec<u8> {
	let mut out = ts.to_vec();
	for pkt in out.as_chunks_mut::<188>().0.iter_mut().filter(|pkt| pid(&pkt[..]) == 0) {
		mend(pkt, |section| section[3..5].copy_from_slice(&7u16.to_be_bytes()));
	}
	out
}

/// The clip, then a `discontinuity_indicator` on its clock PID, then the clip again as another
/// source: its PAT names `transport_stream_id` 7 and its streams and clock sit on other PIDs.
/// The old PIDs never speak again, so the flagged break stays judged until the input ends.
pub(super) fn flagged_switch() -> Vec<u8> {
	const PCR_PID: u16 = 0x100;
	const PMT_PID: u16 = 0x1000;
	let moved = |pid: u16| if matches!(pid, 0x100 | 0x101) { pid + 0x100 } else { pid };
	let mut second = retagged(CLIP);
	for pkt in second.as_chunks_mut::<188>().0 {
		let pid = moved(pid(&pkt[..]));
		pkt[1] = (pkt[1] & 0xe0) | (pid >> 8) as u8;
		pkt[2] = pid as u8;
		if pid == PMT_PID && pkt[1] & 0x40 != 0 {
			mend(pkt, |section| {
				let write = |section: &mut [u8], at: usize| {
					let pid = moved(u16::from_be_bytes([section[at], section[at + 1]]) & 0x1fff);
					section[at] = (section[at] & 0xe0) | (pid >> 8) as u8;
					section[at + 1] = pid as u8;
				};
				write(section, 8);
				let mut at = 12 + (usize::from(section[10] & 0x0f) << 8 | usize::from(section[11]));
				while at < section.len() {
					write(section, at + 1);
					at += 5 + (usize::from(section[at + 3] & 0x0f) << 8 | usize::from(section[at + 4]));
				}
			});
		}
	}
	let mut data = CLIP.to_vec();
	// An adaptation field alone, flagging the break.
	let mut flagged = [0xff; 188];
	flagged[..6].copy_from_slice(&[0x47, (PCR_PID >> 8) as u8, PCR_PID as u8, 0x20, 183, 0x80]);
	data.extend_from_slice(&flagged);
	data.extend_from_slice(&second);
	data
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
			let (restart, rest) = feed(&data, chunk).unwrap().expect("the switch ends the instance");
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
			let fed = feed(&rest, chunk);
			assert!(matches!(fed, Ok(None)), "{expected}: {fed:?}");
		}
	}
}

/// A raw import has no next instance to carry on into, so a switch fails `decode`, and every
/// later call, while `finish` still ends the instance cleanly before the step.
#[test]
fn a_switch_fails_a_raw_import() {
	let (data, _) = replay(SWITCH, |_| 0);
	let (_catalog, mut import) = open();
	let err = import.decode(&data).unwrap_err();
	assert!(matches!(err, super::Error::Restarted(Step::Leap { .. })), "{err:?}");
	let again = import.decode(&[]).unwrap_err();
	assert!(matches!(again, super::Error::Restarted(Step::Leap { .. })), "{again:?}");
	let _finished = import.finish().unwrap();
}

/// A switch the input ends while judging is found by `finish`, which fails as `decode` would
/// have rather than drop the broadcast after the step.
#[test]
fn a_switch_found_at_the_end_fails_a_raw_finish() {
	let (_catalog, mut import) = open();
	import.decode(&flagged_switch()).unwrap();
	let err = import.finish().err().expect("the switch fails the finish");
	let identity = Step::Identity {
		transport_stream_id: 7,
		program_number: 1,
	};
	assert!(
		matches!(&err, super::Error::Restarted(step) if *step == identity),
		"{err:?}"
	);
}

/// A source switch whose PAT names another `transport_stream_id` before its first clock
/// reading restarts at that PAT, where the old program's identity check would otherwise fail
/// the whole import. The new instance reads the PAT itself, however the input is split.
#[test]
fn another_identity_restarts_at_its_pat() {
	let second = retagged(&shift(CLIP, |_| -SWITCH));
	let pat = second
		.as_chunks::<188>()
		.0
		.iter()
		.position(|pkt| pid(pkt) == 0)
		.unwrap()
		* 188;
	assert!(
		second.len() - pat > first_timed(&second).len(),
		"the new PAT precedes the new clock"
	);
	let mut data = CLIP.to_vec();
	data.extend_from_slice(&second);
	for chunk in [data.len(), 188 * 7, 1_000] {
		let (restart, rest) = feed(&data, chunk)
			.unwrap()
			.unwrap_or_else(|| panic!("{chunk}: the switch ends the instance"));
		assert_eq!(
			restart.step,
			Step::Identity {
				transport_stream_id: 7,
				program_number: 1
			},
			"{chunk}"
		);
		assert_eq!(rest, second[pat..], "{chunk}: the new instance starts at its PAT");
		let fed = feed(&rest, chunk);
		assert!(matches!(fed, Ok(None)), "{chunk}: {fed:?}");
	}
}

/// A clock that steps forward by less than the bound, such as across lost packets, carries
/// the same instance on.
#[test]
fn a_step_under_the_bound_carries_on() {
	let (data, _) = replay(9 * HZ, |_| 0);
	let fed = feed(&data, 188 * 7);
	assert!(matches!(fed, Ok(None)), "{fed:?}");

	// Two seconds of the clip lost in the middle.
	let packets = CLIP.len() / 188;
	let mut lossy = CLIP[..packets / 3 * 188].to_vec();
	lossy.extend_from_slice(&CLIP[packets * 2 / 3 * 188..]);
	let fed = feed(&lossy, 188 * 7);
	assert!(matches!(fed, Ok(None)), "{fed:?}");
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

	let (restart, rest) = feed(&data, 188 * 7).unwrap().expect("the switch ends the instance");
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
	let fed = feed(&data, 188 * 7);
	assert!(matches!(fed, Ok(None)), "{fed:?}");

	// The audio carries on a second after the first pass while the rest rewinds.
	let audio = audio_pid(CLIP);
	let (data, _) = replay(
		-SWITCH,
		|clock| if clock == Clock::Pes(audio) { SWITCH + HZ } else { 0 },
	);
	let fed = feed(&data, 188 * 7);
	assert!(matches!(fed, Ok(None)), "{fed:?}");
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
