//! PAT, PMT, and PES header parsing for the importer's demux.
//!
//! Sections arrive whole from the importer's reassembler, so a table spanning packets or
//! sitting behind a nonzero pointer_field parses like any other. A PAT or PMT section is
//! used only when its CRC-32/MPEG-2 matches: a corrupt one is dropped whole, and the last
//! good table stays in force until a repetition replaces it.

use std::collections::BTreeMap;

use anyhow::Context;
use mpeg2ts::ts::Pid;

use super::catalog::Descriptor;

const CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_MPEG_2);

/// Whether a section's trailing CRC-32/MPEG-2 matches the bytes before it.
pub(super) fn crc_ok(section: &[u8]) -> bool {
	let Some(split) = section.len().checked_sub(4) else {
		return false;
	};
	let (body, crc) = section.split_at(split);
	CRC.checksum(body) == u32::from_be_bytes(crc.try_into().expect("four bytes"))
}

/// The table_id of a PAT section.
pub(super) const PAT_TABLE_ID: u8 = 0x00;
/// The table_id of a PMT section.
pub(super) const PMT_TABLE_ID: u8 = 0x02;

/// The long-form section header a PAT or PMT carries (ISO/IEC 13818-1 2.4.4.4).
struct Header<'a> {
	/// transport_stream_id on a PAT, program_number on a PMT.
	extension: u16,
	version: u8,
	/// Clear on a table announced ahead of use, which is not in force yet.
	current: bool,
	number: u8,
	last: u8,
	/// Everything between the header and the CRC.
	body: &'a [u8],
}

impl<'a> Header<'a> {
	/// A whole section as the reassembler delivers it: exactly `3 + section_length` bytes.
	fn parse(section: &'a [u8]) -> Option<Self> {
		// section_syntax_indicator: PAT and PMT always use the long form.
		if section.len() < 12 || section[1] & 0x80 == 0 {
			return None;
		}
		Some(Self {
			extension: u16::from_be_bytes([section[3], section[4]]),
			version: (section[5] >> 1) & 0x1f,
			current: section[5] & 0x01 != 0,
			number: section[6],
			last: section[7],
			body: &section[8..section.len() - 4],
		})
	}
}

/// A 13-bit PID behind three reserved bits.
fn pid(bytes: [u8; 2]) -> Pid {
	Pid::new(u16::from_be_bytes(bytes) & 0x1fff).expect("13-bit PID")
}

/// A 12-bit length behind four reserved or unused bits.
fn length(bytes: [u8; 2]) -> usize {
	usize::from(u16::from_be_bytes(bytes) & 0x0fff)
}

/// One program the PAT lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Association {
	/// 0 names the network PID rather than a program.
	pub program_number: u16,
	pub pmt_pid: Pid,
}

/// A whole program association table, every section of its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pat {
	pub transport_stream_id: u16,
	pub programs: Vec<Association>,
}

impl Pat {
	/// The program numbers listed, without the network PID's.
	pub fn program_numbers(&self) -> Vec<u16> {
		self.programs
			.iter()
			.map(|entry| entry.program_number)
			.filter(|&number| number != 0)
			.collect()
	}
}

/// Collects PAT sections until every section of one version has arrived.
///
/// A PAT that needs more than one section is complete only once all of them are in, so a
/// program is never missed for having been listed in a section still on its way.
#[derive(Default)]
pub(super) struct PatAssembler {
	/// The version, transport_stream_id, and last_section_number being collected.
	table: Option<(u8, u16, u8)>,
	sections: BTreeMap<u8, Vec<Association>>,
}

impl PatAssembler {
	/// Fold in one CRC-checked PAT section, returning the whole table once it is complete.
	///
	/// Every repetition of a complete table returns it again, as the PAT repeats on the wire.
	pub fn section(&mut self, section: &[u8]) -> Option<Pat> {
		let header = Header::parse(section)?;
		if section[0] != PAT_TABLE_ID || !header.current || header.number > header.last {
			return None;
		}
		let (entries, []) = header.body.as_chunks::<4>() else {
			return None;
		};
		let programs = entries
			.iter()
			.map(|entry| Association {
				program_number: u16::from_be_bytes([entry[0], entry[1]]),
				pmt_pid: pid([entry[2], entry[3]]),
			})
			.collect();

		let table = (header.version, header.extension, header.last);
		if self.table != Some(table) {
			self.table = Some(table);
			self.sections.clear();
		}
		self.sections.insert(header.number, programs);
		if self.sections.len() <= usize::from(header.last) {
			return None;
		}
		Some(Pat {
			transport_stream_id: header.extension,
			programs: self.sections.values().flatten().copied().collect(),
		})
	}
}

/// One elementary stream a PMT lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Es {
	/// The raw stream_type, kept whole so a type this crate has no name for still routes.
	pub stream_type: u8,
	pub pid: Pid,
	pub descriptors: Vec<Descriptor>,
}

/// A program map table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pmt {
	pub program_number: u16,
	/// `None` when the PMT names PID 0x1FFF: the program carries no clock of its own.
	pub pcr_pid: Option<Pid>,
	pub program_info: Vec<Descriptor>,
	pub streams: Vec<Es>,
}

impl Pmt {
	/// Parse one CRC-checked PMT section. `None` for a table not yet in force, one that
	/// claims more than one section (a PMT never does), or one whose lengths overrun it.
	pub fn parse(section: &[u8]) -> Option<Self> {
		let header = Header::parse(section)?;
		if section[0] != PMT_TABLE_ID || !header.current || header.number != 0 || header.last != 0 {
			return None;
		}
		let body = header.body;
		if body.len() < 4 {
			return None;
		}
		let pcr_pid = Some(pid([body[0], body[1]])).filter(|pid| pid.as_u16() != 0x1fff);
		let info_len = length([body[2], body[3]]);
		let program_info = descriptors(body.get(4..4 + info_len)?)?;

		let mut rest = &body[4 + info_len..];
		let mut streams = Vec::new();
		while !rest.is_empty() {
			if rest.len() < 5 {
				return None;
			}
			let len = length([rest[3], rest[4]]);
			streams.push(Es {
				stream_type: rest[0],
				pid: pid([rest[1], rest[2]]),
				descriptors: descriptors(rest.get(5..5 + len)?)?,
			});
			rest = &rest[5 + len..];
		}
		Some(Self {
			program_number: header.extension,
			pcr_pid,
			program_info,
			streams,
		})
	}
}

/// A descriptor loop, or `None` if a descriptor runs past it.
fn descriptors(mut data: &[u8]) -> Option<Vec<Descriptor>> {
	let mut out = Vec::new();
	while !data.is_empty() {
		let [tag, len, ..] = *data else {
			return None;
		};
		let body = data.get(2..2 + usize::from(len))?;
		out.push(Descriptor {
			tag,
			data: bytes::Bytes::copy_from_slice(body),
		});
		data = &data[2 + usize::from(len)..];
	}
	Some(out)
}

/// The start of a PES packet: its header, and the payload bytes that follow it in this
/// TS packet.
pub(super) struct PesStart<'a> {
	pub stream_id: u8,
	/// Raw 90 kHz.
	pub pts: Option<u64>,
	/// Raw 90 kHz.
	pub dts: Option<u64>,
	/// The payload length a bounded PES declares, or `None` for an unbounded one (the
	/// usual case for video).
	pub data_len: Option<usize>,
	pub data: &'a [u8],
}

impl<'a> PesStart<'a> {
	/// Parse the PES header at the start of a payload-unit-start packet's payload.
	///
	/// Optional fields this demux has no use for (ESCR, ES rate, trick mode, copy info, the
	/// PES CRC, extensions, stuffing) are skipped by `PES_header_data_length`. A payload that
	/// is not a PES header at all is an error, as it was when an external reader parsed it.
	pub fn parse(payload: &'a [u8]) -> anyhow::Result<Self> {
		anyhow::ensure!(payload.len() >= 6, "PES header truncated");
		anyhow::ensure!(payload[..3] == [0, 0, 1], "missing PES start code prefix");
		let stream_id = payload[3];
		let packet_len = usize::from(u16::from_be_bytes([payload[4], payload[5]]));

		// Streams that carry no optional header (ISO/IEC 13818-1 table 2-21).
		if matches!(stream_id, 0xbc | 0xbe | 0xbf | 0xf0 | 0xf1 | 0xf2 | 0xf8 | 0xff) {
			return Ok(Self {
				stream_id,
				pts: None,
				dts: None,
				data_len: Some(packet_len).filter(|&len| len != 0),
				data: &payload[6..],
			});
		}

		anyhow::ensure!(payload.len() >= 9, "PES header truncated");
		anyhow::ensure!(payload[6] & 0xc0 == 0x80, "unexpected PES header marker bits");
		anyhow::ensure!(payload[6] & 0x30 == 0, "scrambled PES is not supported");
		let (pts_flag, dts_flag) = (payload[7] & 0x80 != 0, payload[7] & 0x40 != 0);
		anyhow::ensure!(pts_flag || !dts_flag, "PES DTS without a PTS");
		let header_len = usize::from(payload[8]);
		let optional = payload.get(9..9 + header_len).context("PES header truncated")?;

		let (pts, dts) = match (pts_flag, dts_flag) {
			(false, _) => (None, None),
			(true, false) => (Some(timestamp(optional.get(..5), 0b0010)?), None),
			(true, true) => (
				Some(timestamp(optional.get(..5), 0b0011)?),
				Some(timestamp(optional.get(5..10), 0b0001)?),
			),
		};
		Ok(Self {
			stream_id,
			pts,
			dts,
			data_len: Some(packet_len)
				.filter(|&len| len != 0)
				.and_then(|len| len.checked_sub(3 + header_len)),
			data: &payload[9 + header_len..],
		})
	}
}

/// A 33-bit PTS or DTS behind its 4-bit prefix and three marker bits.
fn timestamp(bytes: Option<&[u8]>, prefix: u8) -> anyhow::Result<u64> {
	let bytes = bytes.context("PES header truncated")?;
	anyhow::ensure!(bytes[0] >> 4 == prefix, "unexpected PES timestamp prefix");
	anyhow::ensure!(
		bytes[0] & 1 == 1 && bytes[2] & 1 == 1 && bytes[4] & 1 == 1,
		"unexpected PES timestamp marker bit"
	);
	Ok((u64::from(bytes[0] >> 1 & 0x07) << 30)
		| (u64::from(bytes[1]) << 22)
		| (u64::from(bytes[2] >> 1) << 15)
		| (u64::from(bytes[3]) << 7)
		| u64::from(bytes[4] >> 1))
}

/// A long-form section with a valid CRC around `body`.
#[cfg(test)]
pub(super) fn section(table_id: u8, extension: u16, version: u8, number: u8, last: u8, body: &[u8]) -> Vec<u8> {
	let len = 5 + body.len() + 4;
	let mut out = vec![table_id, 0xb0 | (len >> 8) as u8, len as u8];
	out.extend_from_slice(&extension.to_be_bytes());
	out.extend_from_slice(&[0xc1 | (version << 1), number, last]);
	out.extend_from_slice(body);
	let crc = CRC.checksum(&out);
	out.extend_from_slice(&crc.to_be_bytes());
	out
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn crc_matches_mpeg2() {
		// The single-program PAT ffmpeg writes: TSID 1, program 1 on PID 0x1000.
		let pat = [
			0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00, 0x2a, 0xb1, 0x04, 0xb2,
		];
		assert!(crc_ok(&pat));
		let mut flipped = pat;
		flipped[9] ^= 0x01;
		assert!(!crc_ok(&flipped));
		assert!(!crc_ok(&pat[..3]));
	}

	#[test]
	fn pat_waits_for_every_section() {
		let mut pat = PatAssembler::default();
		let first = section(0, 7, 3, 0, 1, &[0x00, 0x01, 0xe1, 0x00]);
		let second = section(0, 7, 3, 1, 1, &[0x00, 0x02, 0xe2, 0x00]);
		assert_eq!(pat.section(&first), None);
		let whole = pat.section(&second).expect("both sections are in");
		assert_eq!(whole.transport_stream_id, 7);
		assert_eq!(whole.program_numbers(), [1, 2]);
		// A repetition of either section repeats the whole table.
		assert_eq!(pat.section(&first), Some(whole));

		// A new version starts over rather than mixing with the old one's sections.
		let next = section(0, 7, 4, 0, 1, &[0x00, 0x03, 0xe3, 0x00]);
		assert_eq!(pat.section(&next), None);
	}

	#[test]
	fn pat_skips_a_table_not_yet_in_force() {
		let mut next = section(0, 1, 0, 0, 0, &[0x00, 0x01, 0xe1, 0x00]);
		next[5] &= !0x01;
		assert_eq!(PatAssembler::default().section(&next), None);
	}

	#[test]
	fn pmt_keeps_unnamed_stream_types_and_descriptors() {
		let body = [
			0xe1, 0x00, // PCR PID 0x100
			0xf0, 0x06, 0x05, 0x04, b'C', b'U', b'E', b'I', // program_info: CUEI registration
			0x2d, 0xe1, 0x01, 0xf0, 0x00, // MPEG-H 3D audio, no descriptors
			0x1b, 0xe1, 0x00, 0xf0, 0x03, 0x0a, 0x01, 0x00, // H.264 with a language descriptor
		];
		let pmt = Pmt::parse(&section(0x02, 9, 0, 0, 0, &body)).unwrap();
		assert_eq!(pmt.program_number, 9);
		assert_eq!(pmt.pcr_pid, Some(Pid::new(0x100).unwrap()));
		assert_eq!(pmt.program_info[0].tag, 0x05);
		assert_eq!(&pmt.program_info[0].data[..], b"CUEI");
		assert_eq!(pmt.streams.len(), 2);
		assert_eq!(pmt.streams[0].stream_type, 0x2d);
		assert_eq!(pmt.streams[1].pid, Pid::new(0x100).unwrap());
		assert_eq!(pmt.streams[1].descriptors[0].tag, 0x0a);
	}

	#[test]
	fn pmt_refuses_an_overrun() {
		let body = [0xff, 0xff, 0xf0, 0x00, 0x1b, 0xe1, 0x00, 0xf0, 0x09, 0x0a];
		assert_eq!(Pmt::parse(&section(0x02, 1, 0, 0, 0, &body)), None);
		// PCR PID 0x1FFF: no clock.
		let body = [0xff, 0xff, 0xf0, 0x00];
		assert_eq!(Pmt::parse(&section(0x02, 1, 0, 0, 0, &body)).unwrap().pcr_pid, None);
	}

	#[test]
	fn pes_header_reads_timestamps_and_skips_optional_fields() {
		// PTS 0x1_2345_6789 and DTS 90000, then an ES rate field and three stuffing bytes.
		let mut payload = vec![0, 0, 1, 0xe0, 0x00, 0x00, 0x80, 0xd0, 16];
		payload.extend_from_slice(&[0x39, 0x8d, 0x15, 0xcf, 0x13]);
		payload.extend_from_slice(&[0x11, 0x00, 0x05, 0xbf, 0x21]);
		payload.extend_from_slice(&[0x80, 0x00, 0x01, 0xff, 0xff, 0xff]);
		payload.extend_from_slice(b"data");
		let pes = PesStart::parse(&payload).unwrap();
		assert_eq!(pes.stream_id, 0xe0);
		assert_eq!(pes.pts, Some(0x1_2345_6789));
		assert_eq!(pes.dts, Some(90_000));
		assert_eq!(pes.data_len, None);
		assert_eq!(pes.data, b"data");
	}

	#[test]
	fn bounded_pes_length_counts_header_stuffing() {
		// PTS only, one stuffing byte, four payload bytes: 3 + 6 + 4 = 13.
		let mut payload = vec![0, 0, 1, 0xc0, 0x00, 13, 0x80, 0x80, 6];
		payload.extend_from_slice(&[0x21, 0x00, 0x01, 0x00, 0x01, 0xff]);
		payload.extend_from_slice(b"mp2!");
		let pes = PesStart::parse(&payload).unwrap();
		assert_eq!(pes.pts, Some(0));
		assert_eq!(pes.data_len, Some(4));
		assert_eq!(pes.data, b"mp2!");
	}

	#[test]
	fn pes_without_optional_header() {
		let payload = [0, 0, 1, 0xbf, 0x00, 0x02, 0xaa, 0xbb];
		let pes = PesStart::parse(&payload).unwrap();
		assert_eq!((pes.pts, pes.data_len, pes.data), (None, Some(2), &[0xaa, 0xbb][..]));
	}

	#[test]
	fn not_a_pes_header() {
		assert!(PesStart::parse(&[0xfc, 0x30, 0x11, 0x00, 0x00, 0x00, 0x00]).is_err());
		assert!(PesStart::parse(&[0, 0, 1, 0xe0, 0, 0, 0x80, 0x40, 0]).is_err());
	}
}
