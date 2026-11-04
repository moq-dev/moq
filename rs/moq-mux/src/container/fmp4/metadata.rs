//! Inspect framing and expanding counts without building mp4-atom's metadata tree.

use mp4_atom::{Decode, FourCC, Header};

use super::{Error, MAX_FRAGMENT_SAMPLES, MAX_METADATA_BYTES, MAX_TRACKS, Result};

/// Check every box, including ones a caller or mp4-atom will discard.
pub(crate) fn check(data: &[u8]) -> Result<()> {
	let mut scan = Scan::default();
	scan.boxes(data, Namespace::Any)?;
	scan.finish()
}

/// Check a body already framed by the streaming importer.
pub(super) fn body(kind: FourCC, data: &[u8]) -> Result<()> {
	let mut scan = Scan::default();
	scan.body(kind, data)?;
	scan.finish()
}

#[derive(Clone, Copy)]
enum Namespace {
	Any,
	Udta,
	Mebx,
	Keys,
}

#[derive(Default)]
struct Scan {
	samples: u64,
	groups: u64,
	tracks: usize,
	any_bytes: u64,
	// Valid MP4 nesting never repeats a container along one path. Refuse cycles before
	// mp4-atom's recursive decoder can turn an input-sized tree into call-stack exhaustion.
	parents: Vec<FourCC>,
}

impl Scan {
	fn finish(self) -> Result<()> {
		if self.samples > MAX_FRAGMENT_SAMPLES {
			return Err(Error::TooManySamples(self.samples));
		}
		if self.groups > MAX_FRAGMENT_SAMPLES {
			return Err(mp4_atom::Error::TooLarge(b"sgpd".into()).into());
		}
		if self.tracks > MAX_TRACKS {
			return Err(Error::TooManyTracks(self.tracks));
		}
		Ok(())
	}

	fn boxes(&mut self, mut data: &[u8], namespace: Namespace) -> Result<()> {
		while !data.is_empty() {
			// mp4-atom permits a short zero terminator after nested boxes.
			if data.len() < 8 && data.iter().all(|&byte| byte == 0) {
				break;
			}
			let (header, body) = next(&mut data)?;
			// These containers retain full Any values even for tiny leaves. Account for their
			// in-memory entries, since a byte bound alone permits far more than a tenfold parse.
			if self
				.parents
				.last()
				.is_some_and(|kind| matches!(kind.as_ref(), b"meta" | b"ipco"))
			{
				self.retain(header.kind, 1, std::mem::size_of::<mp4_atom::Any>())?;
			}
			match namespace {
				Namespace::Udta if header.kind.as_ref() != b"meta" => {}
				Namespace::Mebx if header.kind.as_ref() == b"keys" => self.boxes(body, Namespace::Keys)?,
				Namespace::Keys => self.boxes(body, Namespace::Any)?,
				_ => self.body(header.kind, body)?,
			}
		}
		Ok(())
	}

	fn retain(&mut self, kind: FourCC, count: u64, size: usize) -> Result<()> {
		self.any_bytes += count * size as u64;
		if self.any_bytes > MAX_METADATA_BYTES as u64 {
			return Err(mp4_atom::Error::TooLarge(kind).into());
		}
		Ok(())
	}

	fn body(&mut self, kind: FourCC, mut data: &[u8]) -> Result<()> {
		if kind.as_ref() == b"trak" {
			self.tracks += 1;
		}
		let namespace = match kind.as_ref() {
			b"trun" => {
				u32::decode(&mut data)?; // version and flags
				self.samples += u64::from(u32::decode(&mut data)?);
				return Ok(());
			}
			b"sgpd" => {
				self.groups += sgpd(data)?;
				return Ok(());
			}
			b"iloc" => return iloc(data),
			b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"dinf" | b"edts" | b"mvex" | b"moof" | b"traf"
			| b"mfra" | b"iprp" | b"ipco" => Namespace::Any,
			b"udta" => Namespace::Udta,
			b"meta" => {
				// QuickTime omits the full-box prefix, with hdlr immediately after the header.
				if data.get(4..8) != Some(b"hdlr") {
					take(&mut data, 4)?;
				}
				Namespace::Any
			}
			b"stsd" => {
				u32::decode(&mut data)?; // full-box prefix
				let count = u32::decode(&mut data)?;
				self.retain(kind, count.into(), std::mem::size_of::<mp4_atom::Codec>())?;
				Namespace::Any
			}
			b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"vp08" | b"vp09" | b"av01" | b"jpeg" | b"uncv" => {
				take(&mut data, 78)?; // VisualSampleEntry
				Namespace::Any
			}
			b"mp4a" | b".mp3" | b"Opus" | b"fLaC" | b"ac-3" | b"ec-3" | b"sowt" | b"twos" | b"lpcm" | b"ipcm"
			| b"fpcm" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"s16l" => {
				// Includes the variable-sized QuickTime v1/v2 fields; this decoder allocates nothing.
				mp4_atom::Audio::decode(&mut data)?;
				Namespace::Any
			}
			b"samr" => {
				take(&mut data, 28)?; // AMRSampleEntry
				Namespace::Any
			}
			b"wvtt" | b"evte" => {
				take(&mut data, 8)?; // SampleEntry
				Namespace::Any
			}
			b"mebx" => {
				take(&mut data, 8)?;
				Namespace::Mebx
			}
			b"tx3g" => {
				take(&mut data, 38)?; // SampleEntry and text defaults
				Namespace::Any
			}
			b"mett" | b"metx" => {
				take(&mut data, 8)?;
				for _ in 0..if kind.as_ref() == b"mett" { 2 } else { 3 } {
					let end = data.iter().position(|&b| b == 0).ok_or(mp4_atom::Error::OutOfBounds)?;
					take(&mut data, end + 1)?;
				}
				Namespace::Any
			}
			b"urim" => {
				take(&mut data, 8)?;
				next(&mut data)?; // the typed URIBox, not Any
				Namespace::Any
			}
			// Leaves and private namespaces do not dispatch their bytes through Any. In particular,
			// mdat, wave, ilst and codec descriptions may contain arbitrary box-looking bytes.
			_ => return Ok(()),
		};
		if self.parents.contains(&kind) {
			return Err(mp4_atom::Error::InvalidCombination("recursive metadata boxes").into());
		}
		self.parents.push(kind);
		self.boxes(data, namespace)?;
		self.parents.pop();
		Ok(())
	}
}

/// Split a complete box without copying its body or trusting a narrowed 64-bit size.
fn next<'a>(data: &mut &'a [u8]) -> Result<(Header, &'a [u8])> {
	let size = u32::decode(data)?;
	let kind = FourCC::decode(data)?;
	let size = match size {
		0 => data.len(),
		1 => usize::try_from(u64::decode(data)?.checked_sub(16).ok_or(mp4_atom::Error::InvalidSize)?)
			.map_err(|_| mp4_atom::Error::InvalidSize)?,
		_ => size.checked_sub(8).ok_or(mp4_atom::Error::InvalidSize)? as usize,
	};
	// Media has a transport-owned payload bound and is never expanded as metadata.
	if kind.as_ref() != b"mdat" && size > MAX_METADATA_BYTES {
		return Err(Error::MetadataTooLarge { kind, size });
	}
	Ok((Header { kind, size: Some(size) }, take(data, size)?))
}

fn take<'a>(data: &mut &'a [u8], size: usize) -> Result<&'a [u8]> {
	let (body, rest) = data.split_at_checked(size).ok_or(mp4_atom::Error::OutOfBounds)?;
	*data = rest;
	Ok(body)
}

// mp4-atom 0.17.0 still accepts zero-byte descriptions. Keep the upstream #264 bound here
// until our dependency includes it: every entry must be paid for by at least one input byte.
fn sgpd(mut data: &[u8]) -> Result<u64> {
	let version = u32::decode(&mut data)? >> 24;
	take(&mut data, 4)?; // grouping_type
	if version >= 1 {
		take(&mut data, 4)?; // default_length
	}
	if version >= 2 {
		take(&mut data, 4)?; // default_group_description_index
	}
	let count = u32::decode(&mut data)?;
	if count as usize > data.len() {
		return Err(mp4_atom::Error::OutOfBounds.into());
	}
	Ok(count.into())
}

// The same dependency also lacks #267. A single zero-size extent names the whole reference;
// several would allocate without consuming input, and are refused before decoding any item.
fn iloc(mut data: &[u8]) -> Result<()> {
	let version = u32::decode(&mut data)? >> 24;
	let sizes0 = u8::decode(&mut data)?;
	let sizes1 = u8::decode(&mut data)?;
	let offset = sizes0 >> 4;
	let length = sizes0 & 15;
	let base = sizes1 >> 4;
	let index = if version == 0 { 0 } else { sizes1 & 15 };
	if version > 2
		|| [offset, length, base, index]
			.iter()
			.any(|size| ![0, 4, 8].contains(size))
	{
		return Err(mp4_atom::Error::InvalidSize.into());
	}
	let count = if version < 2 {
		u32::from(u16::decode(&mut data)?)
	} else {
		u32::decode(&mut data)?
	};
	for _ in 0..count {
		take(
			&mut data,
			if version == 0 {
				4
			} else if version == 1 {
				6
			} else {
				8
			},
		)?;
		take(&mut data, base as usize)?;
		let extents = u16::decode(&mut data)? as usize;
		let width = usize::from(offset + length + index);
		if width == 0 && extents > 1 {
			return Err(mp4_atom::Error::OutOfBounds.into());
		}
		take(&mut data, extents * width)?;
	}
	Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use bytes::Bytes;
	use mp4_atom::Encode;

	#[test]
	fn all_top_level_headers_are_checked() {
		for kind in [b"moov", b"moof", b"trun", b"free", b"zzzz"] {
			for extended in [false, true] {
				let size = MAX_METADATA_BYTES + 1;
				let mut data = if extended { 1u32 } else { (size + 8) as u32 }.to_be_bytes().to_vec();
				data.extend_from_slice(kind);
				if extended {
					data.extend_from_slice(&((size + 16) as u64).to_be_bytes());
				}
				assert!(matches!(check(&data), Err(Error::MetadataTooLarge { size: refused, .. }) if refused == size));
			}
			let mut data = boxed(kind, &vec![0; MAX_METADATA_BYTES + 1]);
			data[..4].fill(0); // the body extends to the end of the input
			assert!(matches!(check(&data), Err(Error::MetadataTooLarge { .. })));
		}
		// Media stays under the transport's payload bound rather than the metadata bound.
		check(&boxed(b"mdat", &vec![0; MAX_METADATA_BYTES + 1])).unwrap();
	}

	fn inherited(count: u32) -> Vec<u8> {
		let mut body = vec![0; 4];
		body.extend_from_slice(&count.to_be_bytes());
		boxed(b"trun", &body)
	}

	#[test]
	fn counts_are_checked_across_runs_and_tracks_before_parsing() {
		let half = (MAX_FRAGMENT_SAMPLES / 2) as u32;
		let mut data = boxed(b"traf", &inherited(half));
		data.extend(boxed(b"traf", &inherited(half)));
		check(&boxed(b"moof", &data)).unwrap();
		data.extend(boxed(b"traf", &inherited(1)));
		assert!(
			matches!(check(&boxed(b"moof", &data)), Err(Error::TooManySamples(count)) if count == MAX_FRAGMENT_SAMPLES + 1)
		);
		// A known box is expanded even when it appears outside its normal parent.
		assert!(matches!(check(&inherited(u32::MAX)), Err(Error::TooManySamples(_))));
	}

	#[test]
	fn nested_counts_respect_box_namespaces_and_sample_entry_prefixes() {
		let run = inherited(u32::MAX);
		let mut avc = vec![0; 78];
		avc.extend_from_slice(&run);
		let avc = boxed(b"avc1", &avc);
		let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
		stsd.extend(avc);
		let stsd = boxed(b"stsd", &stsd);
		assert!(matches!(check(&stsd), Err(Error::TooManySamples(_))));
		for kind in [b"mdat", b"zzzz", b"ilst", b"wave", b"udta"] {
			// These bytes are private data, not Any children.
			check(&boxed(kind, &run)).unwrap();
		}
		for full in [false, true] {
			let mut meta = if full { vec![0; 4] } else { Vec::new() };
			mp4_atom::Hdlr::default().encode(&mut meta).unwrap();
			meta.extend(&run);
			assert!(matches!(check(&boxed(b"meta", &meta)), Err(Error::TooManySamples(_))));
		}
	}

	#[test]
	fn zero_byte_entries_cannot_amplify() {
		for version in 0..=3 {
			let mut data = vec![version, 0, 0, 0];
			data.extend_from_slice(b"roll");
			if version >= 1 {
				data.extend_from_slice(&0u32.to_be_bytes());
			}
			if version >= 2 {
				data.extend_from_slice(&0u32.to_be_bytes());
			}
			data.extend_from_slice(&1u32.to_be_bytes());
			assert!(check(&boxed(b"sgpd", &data)).is_err());
		}
		for version in 0..=2 {
			for count in [1u16, 2] {
				let mut data = vec![version, 0, 0, 0, 0, 0];
				if version < 2 {
					data.extend_from_slice(&1u16.to_be_bytes());
				} else {
					data.extend_from_slice(&1u32.to_be_bytes());
				}
				data.extend_from_slice(&vec![
					0;
					if version == 0 {
						4
					} else if version == 1 {
						6
					} else {
						8
					}
				]);
				data.extend_from_slice(&count.to_be_bytes());
				assert_eq!(check(&boxed(b"iloc", &data)).is_ok(), count == 1);
			}
		}
	}

	#[test]
	fn recursive_metadata_is_refused_before_the_recursive_decoder() {
		let data = boxed(
			b"moov",
			&boxed(b"meta", &{
				let mut data = vec![0; 4];
				mp4_atom::Hdlr::default().encode(&mut data).unwrap();
				data.extend(boxed(b"moov", &[]));
				data
			}),
		);
		assert!(check(&data).is_err());
	}

	pub(crate) fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
		let mut data = Vec::new();
		mp4_atom::Header {
			kind: kind.into(),
			size: Some(body.len()),
		}
		.encode(&mut data)
		.unwrap();
		data.extend_from_slice(body);
		data
	}

	pub(crate) fn attacks(kind: &[u8; 4]) -> Vec<(&'static str, Bytes)> {
		// A four-byte entry becomes a forty-byte TrunEntry, even when its caller discards it.
		let count = super::super::MAX_METADATA_BYTES / 2;
		let mut run = vec![0, 0, 2, 0];
		run.extend_from_slice(&(count as u32).to_be_bytes());
		run.resize(8 + count * 4, 0);
		let run = boxed(b"trun", &run);
		// Unknown v0 descriptions consume no input after the first entry.
		let mut group = b"\0\0\0\0roll".to_vec();
		group.extend_from_slice(&(count as u32).to_be_bytes());
		let group = boxed(b"sgpd", &group);
		// A vector of Any values also amplifies tiny, otherwise harmless boxes.
		let mut meta = vec![0; 4];
		mp4_atom::Hdlr::default().encode(&mut meta).unwrap();
		for _ in 0..65_536 {
			meta.extend(boxed(b"free", &[]));
		}
		let meta = boxed(b"meta", &meta);
		let mut codecs = vec![0; 4];
		codecs.extend_from_slice(&65_536u32.to_be_bytes());
		for _ in 0..65_536 {
			codecs.extend(boxed(b"free", &[]));
		}
		let codecs = boxed(b"stsd", &codecs);
		vec![
			("large body", boxed(kind, &run).into()),
			("small body, large count", boxed(kind, &group).into()),
			("many tiny boxes", boxed(kind, &meta).into()),
			("many tiny codec entries", boxed(kind, &codecs).into()),
		]
	}

	pub(crate) fn measure(name: &str, decode: impl FnOnce() -> bool) -> bool {
		let (refused, usage) = crate::allocation::measure(decode);
		eprintln!("{name}: peak {} bytes", usage.peak);
		refused && usage.peak <= super::super::MAX_HELD_BYTES
	}
}
