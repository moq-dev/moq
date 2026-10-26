mod export;

pub(crate) use export::{Codec, Convert, Export};

use std::collections::BTreeMap;

use bytes::{Buf, Bytes, BytesMut};

/// The four-byte Annex B start code preceding a NAL unit.
pub const START_CODE: Bytes = Bytes::from_static(&[0, 0, 0, 1]);

/// Annex B parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The buffer does not begin with an Annex B start code.
	#[error("missing Annex B start code")]
	MissingStartCode,

	/// The start code has an invalid byte sequence.
	#[error("invalid Annex B start code")]
	InvalidStartCode,

	/// The NAL length prefix size is outside one through four bytes.
	#[error("invalid avc1/hvc1 length size {0}")]
	InvalidLengthSize(usize),

	/// A NAL length prefix or payload extends past the buffer.
	#[error("truncated length-prefixed NAL unit")]
	Truncated,

	/// The NAL payload length does not fit a four-byte prefix.
	#[error("NAL too large for a 4-byte length prefix")]
	NalTooLarge,

	/// A keyframe has neither an inline nor a retained parameter set of the named type.
	#[error("keyframe has no {0}, in the access unit or earlier in the track")]
	MissingParameterSet(&'static str),

	/// A parameter set NAL of this type ends before the id it defines.
	#[error("parameter set NAL type {0} has no readable id")]
	ParameterSetId(u8),
}

/// A result using this module's error type.
pub type Result<T> = std::result::Result<T, Error>;

/// True when the buffer is an out-of-band config record (avcC / hvcC) rather than an
/// Annex-B elementary stream: a 3- or 4-byte start code means Annex-B, anything else
/// non-empty is a config record. An empty buffer is Annex-B, since there's no record to
/// parse and the in-band shapes self-initialize from the first keyframe (e.g. moqsink
/// hands an empty init for inline-parameter-set streams).
pub(crate) fn is_config_record(bytes: &[u8]) -> bool {
	!(bytes.is_empty() || matches!(bytes, [0, 0, 1, ..]) || matches!(bytes, [0, 0, 0, 1, ..]))
}

/// Iterate the NAL units of a length-prefixed (avc1 / hvc1) access unit, where each NAL
/// is preceded by a `length_size`-byte big-endian length.
///
/// Yields [`Error::Truncated`] as the final item when a prefix or length runs past the end
/// of the buffer: callers that must reject a malformed AU propagate it, while a lenient
/// scan can stop at it (`.map_while(Result::ok)`).
pub(crate) fn length_prefixed_nals(data: &[u8], length_size: usize) -> Result<LengthPrefixedNals<'_>> {
	if !(1..=4).contains(&length_size) {
		return Err(Error::InvalidLengthSize(length_size));
	}
	Ok(LengthPrefixedNals { data, length_size })
}

/// The iterator [`length_prefixed_nals`] returns.
#[derive(Clone)]
pub(crate) struct LengthPrefixedNals<'a> {
	data: &'a [u8],
	length_size: usize,
}

impl<'a> LengthPrefixedNals<'a> {
	/// Take a leading access unit delimiter, which must stay ahead of injected parameter sets.
	pub(crate) fn take_delimiter(&mut self, codec: InBandCodec) -> Result<Option<&'a [u8]>> {
		if let Some(nal) = self.clone().next().transpose()?
			&& !nal.is_empty()
			&& codec.nal_type(nal) == codec.delimiter()
		{
			return self.next().transpose();
		}
		Ok(None)
	}
}

impl<'a> Iterator for LengthPrefixedNals<'a> {
	type Item = Result<&'a [u8]>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.data.is_empty() {
			return None;
		}
		if self.data.len() < self.length_size {
			self.data = &[];
			return Some(Err(Error::Truncated));
		}
		let (prefix, rest) = self.data.split_at(self.length_size);
		let len = prefix.iter().fold(0usize, |acc, &byte| (acc << 8) | byte as usize);
		if rest.len() < len {
			self.data = &[];
			return Some(Err(Error::Truncated));
		}
		let (nal, rest) = rest.split_at(len);
		self.data = rest;
		Some(Ok(nal))
	}
}

/// Convert a length-prefixed NALU payload (avc1 / hvc1 wire shape) to Annex-B,
/// injecting `prefix` bytes (typically VPS/SPS/PPS NAL units already in Annex-B
/// form) after a leading access unit delimiter, or before the other NAL units.
pub fn from_length_prefixed(payload: &[u8], length_size: usize, prefix: Option<&[u8]>) -> Result<Bytes> {
	let mut out = BytesMut::with_capacity(payload.len() + prefix.map(|p| p.len()).unwrap_or(0) + 16);
	let mut nals = length_prefixed_nals(payload, length_size)?;
	if let Some(prefix) = prefix {
		let codec = after_start_code(prefix)
			.ok()
			.flatten()
			.and_then(|start| prefix.get(start))
			.and_then(|&header| InBandCodec::of_parameter_set(header));
		if let Some(codec) = codec
			&& let Some(aud) = nals.take_delimiter(codec)?
		{
			out.extend_from_slice(&START_CODE);
			out.extend_from_slice(aud);
		}
		out.extend_from_slice(prefix);
	}
	for nal in nals {
		out.extend_from_slice(&START_CODE);
		out.extend_from_slice(nal?);
	}
	Ok(out.freeze())
}

/// Concatenate `start_code | nal` for every NAL in `nals` and freeze the
/// result. Used to build a keyframe parameter-set prefix for an Annex-B
/// elementary stream.
pub fn build_prefix<'a, I: IntoIterator<Item = &'a Bytes>>(nals: I) -> Bytes {
	let nals: Vec<&Bytes> = nals.into_iter().collect();
	let total: usize = nals.iter().map(|n| n.len() + START_CODE.len()).sum();
	let mut out = BytesMut::with_capacity(total);
	for nal in nals {
		out.extend_from_slice(&START_CODE);
		out.extend_from_slice(nal);
	}
	out.freeze()
}

/// Parameter sets of one type keyed by the id they define, in id order.
///
/// A later set with an id replaces the earlier one, as it does in a decoder, so a source may
/// define several (e.g. two PPS that slices reference by id) and a repeated or redefined id
/// costs one entry rather than a growing list.
pub(crate) type ParameterSets = BTreeMap<u32, Bytes>;

/// The NAL syntax an [`InBand`] transform reads.
#[derive(Debug, Clone, Copy)]
pub(crate) enum InBandCodec {
	H264,
	H265,
}

impl InBandCodec {
	/// The parameter-set NAL types, each referencing only those before it.
	fn parameter_sets(self) -> &'static [(u8, &'static str)] {
		match self {
			Self::H264 => &[(7, "SPS"), (8, "PPS")],
			Self::H265 => &[(32, "VPS"), (33, "SPS"), (34, "PPS")],
		}
	}

	/// The codec whose parameter-set NAL begins with `header`.
	///
	/// NAL headers overlap between codecs (an H.265 slice can look like an H.264 AUD), so
	/// only a parameter set identifies one.
	fn of_parameter_set(header: u8) -> Option<Self> {
		match (header & 0x1f, (header >> 1) & 0x3f) {
			(7 | 8, _) => Some(Self::H264),
			(_, 32..=34) => Some(Self::H265),
			_ => None,
		}
	}

	/// Insert parameter set `nal` (header included) under the id it defines, replacing any
	/// earlier set with that id.
	pub(crate) fn insert_parameter_set(self, sets: &mut ParameterSets, nal: &Bytes) -> Result<()> {
		use h264_parser::{bitreader::BitReader, eg::read_ue};

		let nal_type = self.nal_type(nal);
		let header = match self {
			Self::H264 => 1,
			Self::H265 => 2,
		};
		let rbsp = h264_parser::nal::ebsp_to_rbsp(nal.get(header..).unwrap_or_default());
		let r = &mut BitReader::new(&rbsp);
		let id = match (self, nal_type) {
			// seq_parameter_set_id follows profile_idc, the constraint flags, and level_idc.
			(Self::H264, 7) => r.skip_bits(24).and_then(|()| read_ue(r)),
			// pic_parameter_set_id opens a PPS in both codecs.
			(Self::H264, 8) | (Self::H265, 34) => read_ue(r),
			(Self::H265, 32) => r.read_bits(4), // vps_video_parameter_set_id
			(Self::H265, 33) => crate::codec::h265::sps_id(r),
			_ => return Err(Error::ParameterSetId(nal_type)),
		};
		sets.insert(id.map_err(|_| Error::ParameterSetId(nal_type))?, nal.clone());
		Ok(())
	}

	fn delimiter(self) -> u8 {
		match self {
			Self::H264 => 9,
			Self::H265 => 35,
		}
	}

	fn nal_type(self, nal: &[u8]) -> u8 {
		match self {
			Self::H264 => nal[0] & 0x1f,
			Self::H265 => (nal[0] >> 1) & 0x3f,
		}
	}

	/// A coded slice, so the access unit is a picture.
	fn is_vcl(self, nal: &[u8]) -> bool {
		match self {
			Self::H264 => (1..=5).contains(&self.nal_type(nal)),
			Self::H265 => self.nal_type(nal) < 32,
		}
	}
}

/// Length-prefixes Annex-B access units with their parameter sets kept in the
/// sample, for an avc3 or hev1 track whose sample entry lists none.
///
/// A keyframe is the only place a receiver can join, so it always carries every
/// parameter-set type, in dependency order (VPS, SPS, PPS) right after any access
/// unit delimiter. A type the access unit omitted is the last one seen. A type it
/// carried replaces every cached set of that type, so a source that spreads several
/// PPS ids across access units keeps only the last.
pub(crate) struct InBand {
	codec: InBandCodec,
	/// The active sets of each type, in [`InBandCodec::parameter_sets`] order.
	active: [ParameterSets; 3],
}

impl InBand {
	pub fn new(codec: InBandCodec) -> Self {
		Self {
			codec,
			active: Default::default(),
		}
	}

	/// Returns `None` for an access unit with no coded slice, as `Avc1` and `Hvc1`
	/// do; its parameter sets are kept for the next keyframe.
	pub fn transform(&mut self, payload: Bytes, keyframe: bool) -> Result<Option<Bytes>> {
		let codec = self.codec;
		let kinds = codec.parameter_sets();
		let mut buf = payload;
		let mut iter = NalIterator::new(&mut buf);
		let mut nals = Vec::new();
		while let Some(nal) = iter.next().transpose()? {
			nals.push(nal);
		}
		nals.extend(iter.flush()?);
		nals.retain(|nal| !nal.is_empty());

		let kind = |nal: &Bytes| {
			let nal_type = codec.nal_type(nal);
			kinds.iter().position(|(t, _)| *t == nal_type)
		};
		let mut sets: [ParameterSets; 3] = Default::default();
		for nal in &nals {
			if let Some(index) = kind(nal) {
				codec.insert_parameter_set(&mut sets[index], nal)?;
			}
		}
		for (active, set) in self.active.iter_mut().zip(sets) {
			if !set.is_empty() {
				*active = set;
			}
		}
		if !nals.iter().any(|nal| codec.is_vcl(nal)) {
			return Ok(None);
		}

		let mut out = BytesMut::with_capacity(nals.iter().map(|nal| nal.len() + 4).sum());
		if !keyframe {
			for nal in &nals {
				length_prefix(&mut out, nal)?;
			}
			return Ok(Some(out.freeze()));
		}

		let delimiter = codec.delimiter();
		for nal in nals.iter().filter(|nal| codec.nal_type(nal) == delimiter) {
			length_prefix(&mut out, nal)?;
		}
		for ((_, name), active) in kinds.iter().zip(&self.active) {
			if active.is_empty() {
				return Err(Error::MissingParameterSet(name));
			}
			for nal in active.values() {
				length_prefix(&mut out, nal)?;
			}
		}
		for nal in nals
			.iter()
			.filter(|nal| kind(nal).is_none() && codec.nal_type(nal) != delimiter)
		{
			length_prefix(&mut out, nal)?;
		}
		Ok(Some(out.freeze()))
	}
}

fn length_prefix(out: &mut BytesMut, nal: &[u8]) -> Result<()> {
	let len = u32::try_from(nal.len()).map_err(|_| Error::NalTooLarge)?;
	out.extend_from_slice(&len.to_be_bytes());
	out.extend_from_slice(nal);
	Ok(())
}

/// Reconcile the retained parameter sets with what a keyframe access unit carried
/// inline, called when the keyframe slice is reached:
///
/// - If the AU presented its own set (`seen` non-empty), adopt it as the retained
///   set, dropping any the new GOP no longer uses (a mid-stream reinit).
/// - If the AU carried none, re-inject the retained set into `chunks` as Annex-B
///   so a receiver tuning in at this keyframe still gets them.
///
/// `seen` is this AU's inline NALs (already appended to `chunks`); `retained` is
/// the cross-GOP set re-injected on bare keyframes.
pub(crate) fn reconcile_keyframe_params(chunks: &mut BytesMut, retained: &mut ParameterSets, seen: &mut ParameterSets) {
	if seen.is_empty() {
		for nal in retained.values() {
			chunks.extend_from_slice(&START_CODE);
			chunks.extend_from_slice(nal);
		}
		seen.clone_from(retained);
	} else if seen != retained {
		retained.clone_from(seen);
	}
}

/// Iterates complete Annex B NAL units, leaving an unterminated final unit for `flush`.
pub struct NalIterator<'a, T: Buf + AsRef<[u8]> + 'a> {
	buf: &'a mut T,
	start: Option<usize>,
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> NalIterator<'a, T> {
	/// Read NAL units from a buffer that begins with an Annex B start code.
	pub fn new(buf: &'a mut T) -> Self {
		Self { buf, start: None }
	}

	/// Assume the buffer ends with a NAL unit and flush it.
	/// This is more efficient because we cache the last "start" code position.
	pub fn flush(self) -> Result<Option<Bytes>> {
		let start = match self.start {
			Some(start) => start,
			None => {
				let Some(start) = after_start_code(self.buf.as_ref())? else {
					return Ok(None);
				};
				start
			}
		};

		self.buf.advance(start);

		let nal = self.buf.copy_to_bytes(self.buf.remaining());
		Ok(Some(nal))
	}
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> Iterator for NalIterator<'a, T> {
	type Item = Result<Bytes>;

	fn next(&mut self) -> Option<Self::Item> {
		let start = match self.start {
			Some(start) => start,
			None => match after_start_code(self.buf.as_ref()).transpose()? {
				Ok(start) => start,
				Err(err) => return Some(Err(err)),
			},
		};

		let (size, new_start) = find_start_code(&self.buf.as_ref()[start..])?;
		self.buf.advance(start);

		let nal = self.buf.copy_to_bytes(size);
		self.start = Some(new_start);
		Some(Ok(nal))
	}
}

/// Rewrite a length-prefixed NALU buffer (avc1/hvc1 sample, each NAL preceded
/// by a `length_size`-byte big-endian length) into Annex-B by replacing every
/// length prefix with a 4-byte start code. This is the inverse of the
/// length-prefixing done by [`crate::codec::h264::Avc1`] / [`crate::codec::h265::Hvc1`].
pub fn length_prefixed_to_annexb(data: &[u8], length_size: usize, out: &mut Vec<u8>) -> anyhow::Result<()> {
	for nal in length_prefixed_nals(data, length_size)? {
		out.extend_from_slice(&START_CODE);
		out.extend_from_slice(nal?);
	}
	Ok(())
}

/// Return the leading start code size, or `None` if more bytes are needed.
pub fn after_start_code(b: &[u8]) -> Result<Option<usize>> {
	if b.len() < 3 {
		return Ok(None);
	}

	// NOTE: We have to check every byte, so the `find_start_code` optimization doesn't matter.
	if b[0] != 0 || b[1] != 0 {
		return Err(Error::MissingStartCode);
	}

	match b[2] {
		0 if b.len() < 4 => Ok(None),
		0 if b[3] != 1 => Err(Error::MissingStartCode),
		0 => Ok(Some(4)),
		1 => Ok(Some(3)),
		_ => Err(Error::InvalidStartCode),
	}
}

/// Return the byte offset and size of the first start code in the buffer.
//
// Both forms share the `0 0 1` suffix (3-byte is `0 0 1`, 4-byte is `0 0 0 1`), so a single
// SIMD-accelerated substring search for `0 0 1` finds the core. We then peek one byte back to
// decide whether a leading zero promotes it to a 4-byte code.
pub fn find_start_code(b: &[u8]) -> Option<(usize, usize)> {
	let core = memchr::memmem::find(b, &[0, 0, 1])?;
	if core > 0 && b[core - 1] == 0 {
		Some((core - 1, 4))
	} else {
		Some((core, 3))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// Tests for from_length_prefixed - converts avc1/hvc1 to Annex-B,
	// with optional SPS/PPS prefix injection on keyframes.

	#[test]
	fn from_length_prefixed_no_prefix() {
		// One 4-byte length, then 2-byte NAL `0x65 0x88` (an H.264 IDR slice).
		let payload = &[0, 0, 0, 2, 0x65, 0x88];
		let out = from_length_prefixed(payload, 4, None).unwrap();
		assert_eq!(out.as_ref(), &[0, 0, 0, 1, 0x65, 0x88]);
	}

	#[test]
	fn from_length_prefixed_with_prefix_injects_verbatim() {
		// SPS+PPS prefix built by `build_prefix` (start_code + sps_nal + start_code + pps_nal).
		let sps = Bytes::from_static(&[0x67, 0x42, 0xc0, 0x1f]);
		let pps = Bytes::from_static(&[0x68, 0xce, 0x3c, 0x80]);
		let prefix = build_prefix([&sps, &pps]);
		assert_eq!(
			prefix.as_ref(),
			&[
				0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, // start_code + SPS
				0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // start_code + PPS
			]
		);

		// One length-prefixed IDR slice.
		let payload = &[0, 0, 0, 2, 0x65, 0x88];
		let out = from_length_prefixed(payload, 4, Some(&prefix)).unwrap();

		// Output must start with the prefix byte-for-byte, then the slice in Annex-B form.
		assert_eq!(&out[..prefix.len()], prefix.as_ref());
		assert_eq!(&out[prefix.len()..], &[0, 0, 0, 1, 0x65, 0x88]);
	}

	#[test]
	fn from_length_prefixed_keeps_aud_first() {
		for (params, aud, slice) in [
			(
				vec![vec![0x67, 0x42], vec![0x68, 0xce]],
				vec![0x09, 0x10],
				vec![0x65, 0x88],
			),
			(
				vec![vec![0x40, 1], vec![0x42, 1], vec![0x44, 1]],
				vec![0x46, 1, 0x10],
				vec![0x26, 1, 0x88],
			),
		] {
			let params: Vec<Bytes> = params.into_iter().map(Bytes::from).collect();
			let prefix = build_prefix(&params);
			for length_size in 1..=4 {
				for leading in [None, Some(aud.as_slice())] {
					let mut payload = Vec::new();
					for nal in leading.into_iter().chain([slice.as_slice()]) {
						payload.extend_from_slice(&(nal.len() as u32).to_be_bytes()[4 - length_size..]);
						payload.extend_from_slice(nal);
					}
					let mut expected = Vec::new();
					if let Some(aud) = leading {
						expected.extend_from_slice(&START_CODE);
						expected.extend_from_slice(aud);
					}
					expected.extend_from_slice(&prefix);
					expected.extend_from_slice(&START_CODE);
					expected.extend_from_slice(&slice);
					let out = from_length_prefixed(&payload, length_size, Some(&prefix)).unwrap();
					assert_eq!(out.as_ref(), expected.as_slice());
					let mut without_prefix = Vec::new();
					for nal in leading.into_iter().chain([slice.as_slice()]) {
						without_prefix.extend_from_slice(&START_CODE);
						without_prefix.extend_from_slice(nal);
					}
					assert_eq!(
						from_length_prefixed(&payload, length_size, None).unwrap(),
						without_prefix
					);
				}
			}
		}
	}

	#[test]
	fn from_length_prefixed_distinguishes_codec_headers() {
		// H.265 TSA_N with nuh_layer_id 32 has the same first byte as an H.264 AUD.
		let prefix = build_prefix([&Bytes::from_static(&[0x40, 1])]);
		let payload = &[0, 0, 0, 3, 0x09, 1, 0x80];
		let mut expected = prefix.to_vec();
		expected.extend_from_slice(&[0, 0, 0, 1, 0x09, 1, 0x80]);
		assert_eq!(from_length_prefixed(payload, 4, Some(&prefix)).unwrap(), expected);
	}

	#[test]
	fn length_prefixed_to_annexb_rewrites_prefixes() {
		// Two 4-byte-length-prefixed NALs -> two start-code-delimited NALs.
		let input = [0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 3, 0x68, 0xce, 0x3c];
		let mut out = Vec::new();
		length_prefixed_to_annexb(&input, 4, &mut out).unwrap();
		assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0x3c]);
	}

	#[test]
	fn length_prefixed_to_annexb_rejects_truncated() {
		// Declared length (5) overruns the buffer.
		let input = [0, 0, 0, 5, 0x67];
		let mut out = Vec::new();
		assert!(length_prefixed_to_annexb(&input, 4, &mut out).is_err());
	}

	// Tests for after_start_code - validates and measures start code at buffer beginning

	#[test]
	fn test_after_start_code_3_byte() {
		let buf = &[0, 0, 1, 0x67];
		assert_eq!(after_start_code(buf).unwrap(), Some(3));
	}

	#[test]
	fn test_after_start_code_4_byte() {
		let buf = &[0, 0, 0, 1, 0x67];
		assert_eq!(after_start_code(buf).unwrap(), Some(4));
	}

	#[test]
	fn test_after_start_code_too_short() {
		let buf = &[0, 0];
		assert_eq!(after_start_code(buf).unwrap(), None);
	}

	#[test]
	fn test_after_start_code_incomplete_4_byte() {
		let buf = &[0, 0, 0];
		assert_eq!(after_start_code(buf).unwrap(), None);
	}

	#[test]
	fn test_after_start_code_invalid_first_byte() {
		let buf = &[1, 0, 1];
		assert!(after_start_code(buf).is_err());
	}

	#[test]
	fn test_after_start_code_invalid_second_byte() {
		let buf = &[0, 1, 1];
		assert!(after_start_code(buf).is_err());
	}

	#[test]
	fn test_after_start_code_invalid_third_byte() {
		let buf = &[0, 0, 2];
		assert!(after_start_code(buf).is_err());
	}

	#[test]
	fn test_after_start_code_invalid_4_byte_pattern() {
		let buf = &[0, 0, 0, 2];
		assert!(after_start_code(buf).is_err());
	}

	// Tests for find_start_code - finds next start code in NAL data

	#[test]
	fn test_find_start_code_3_byte() {
		let buf = &[0x67, 0x42, 0x00, 0x1f, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((4, 3)));
	}

	#[test]
	fn test_find_start_code_4_byte() {
		// Should detect 4-byte start code at beginning
		let buf = &[0, 0, 0, 1, 0x67];
		assert_eq!(find_start_code(buf), Some((0, 4)));
	}

	#[test]
	fn test_find_start_code_4_byte_after_data() {
		// Should detect 4-byte start code after NAL data
		let buf = &[0x67, 0x42, 0xff, 0x1f, 0, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((4, 4)));
	}

	#[test]
	fn test_find_start_code_at_start_3_byte() {
		let buf = &[0, 0, 1, 0x67];
		assert_eq!(find_start_code(buf), Some((0, 3)));
	}

	#[test]
	fn test_find_start_code_none() {
		let buf = &[0x67, 0x42, 0x00, 0x1f, 0xff];
		assert_eq!(find_start_code(buf), None);
	}

	#[test]
	fn test_find_start_code_trailing_zeros() {
		let buf = &[0x67, 0x42, 0x00, 0x1f, 0, 0];
		assert_eq!(find_start_code(buf), None);
	}

	#[test]
	fn test_find_start_code_edge_case_3_byte() {
		let buf = &[0xff, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((1, 3)));
	}

	#[test]
	fn test_find_start_code_false_positive_avoidance() {
		// Pattern like: x 0 0 y (where y != 1) - should skip ahead
		let buf = &[0xff, 0, 0, 0xff, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((4, 3)));
	}

	#[test]
	fn test_find_start_code_4_byte_after_nonzero() {
		// Critical edge case: x 0 0 0 1 should find 4-byte start code at position 1
		// This tests that we only skip 1 byte when seeing ? ? 0 0
		let buf = &[0xff, 0, 0, 0, 1];
		assert_eq!(find_start_code(buf), Some((1, 4)));
	}

	#[test]
	fn test_find_start_code_consecutive_zeros() {
		// Multiple consecutive zeros before the 1
		let buf = &[0xff, 0, 0, 0, 0, 0, 1];
		// Should skip past leading zeros and find the start code
		let result = find_start_code(buf);
		assert!(result.is_some());
		let (pos, size) = result.unwrap();
		// The exact position depends on the algorithm, but it should find a valid start code
		assert!(size == 3 || size == 4);
		assert!(pos < buf.len());
	}

	// Tests for NalIterator - iterates over NAL units in Annex B format

	#[test]
	fn test_nal_iterator_simple_3_byte() {
		let mut data = Bytes::from(vec![0, 0, 1, 0x67, 0x42, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal = iter.next().unwrap().unwrap();
		assert_eq!(nal.as_ref(), &[0x67, 0x42]);
		assert!(iter.next().is_none());

		// Make sure the trailing 001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_simple_4_byte() {
		let mut data = Bytes::from(vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal = iter.next().unwrap().unwrap();
		assert_eq!(nal.as_ref(), &[0x67, 0x42]);
		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_multiple_nals() {
		let mut data = Bytes::from(vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x68, 0xce]);

		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_realistic_h264() {
		// A realistic H.264 stream with SPS, PPS, and IDR
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, // SPS NAL
			0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // PPS NAL
			0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, // IDR slice
			// Trailing start code (needed to detect the end of the last NAL)
			0, 0, 0, 1,
		]);
		let mut iter = NalIterator::new(&mut data);

		let sps = iter.next().unwrap().unwrap();
		assert_eq!(sps[0] & 0x1f, 7); // SPS type
		assert_eq!(sps.as_ref(), &[0x67, 0x42, 0x00, 0x1f]);

		let pps = iter.next().unwrap().unwrap();
		assert_eq!(pps[0] & 0x1f, 8); // PPS type
		assert_eq!(pps.as_ref(), &[0x68, 0xce, 0x3c, 0x80]);

		let idr = iter.next().unwrap().unwrap();
		assert_eq!(idr[0] & 0x1f, 5); // IDR type
		assert_eq!(idr.as_ref(), &[0x65, 0x88, 0x84, 0x00]);

		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_realistic_h265() {
		// A realistic H.265 stream with VPS, SPS, PPS, and IDR
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01, // VPS NAL
			0, 0, 0, 1, 0x42, 0x01, 0x01, 0x60, // SPS NAL
			0, 0, 0, 1, 0x44, 0x01, 0xc0, 0xf1, // PPS NAL
			0, 0, 0, 1, 0x26, 0x01, 0x9a, 0x20, // IDR_W_RADL slice
			// Trailing start code (needed to detect the end of the last NAL)
			0, 0, 0, 1,
		]);
		let mut iter = NalIterator::new(&mut data);

		let vps = iter.next().unwrap().unwrap();
		assert_eq!((vps[0] >> 1) & 0x3f, 32); // VPS type
		assert_eq!(vps.as_ref(), &[0x40, 0x01, 0x0c, 0x01]);

		let sps = iter.next().unwrap().unwrap();
		assert_eq!((sps[0] >> 1) & 0x3f, 33); // SPS type
		assert_eq!(sps.as_ref(), &[0x42, 0x01, 0x01, 0x60]);

		let pps = iter.next().unwrap().unwrap();
		assert_eq!((pps[0] >> 1) & 0x3f, 34); // PPS type
		assert_eq!(pps.as_ref(), &[0x44, 0x01, 0xc0, 0xf1]);

		let idr = iter.next().unwrap().unwrap();
		assert_eq!((idr[0] >> 1) & 0x3f, 19); // IDR slice type (IDR_W_RADL)
		assert_eq!(idr.as_ref(), &[0x26, 0x01, 0x9a, 0x20]);

		assert!(iter.next().is_none());

		// Make sure the trailing 0001 is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_invalid_start() {
		let mut data = Bytes::from(vec![1, 0, 1, 0x67]);
		let mut iter = NalIterator::new(&mut data);

		assert!(iter.next().unwrap().is_err());

		// Make sure the data is still in the buffer.
		assert_eq!(data.as_ref(), &[1, 0, 1, 0x67]);
	}

	#[test]
	fn test_nal_iterator_empty_nal() {
		// Two consecutive start codes create an empty NAL
		let mut data = Bytes::from(vec![0, 0, 1, 0, 0, 1, 0x67, 0, 0, 1]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.len(), 0);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x67]);

		assert!(iter.next().is_none());

		// Make sure the data is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 1]);
	}

	#[test]
	fn test_nal_iterator_nal_with_embedded_zeros() {
		// NAL data that contains zeros (but not a start code pattern)
		let mut data = Bytes::from(vec![
			0, 0, 1, 0x67, 0x00, 0x00, 0x00, 0xff, // NAL with embedded zeros
			0, 0, 1, 0x68, // Next NAL
			0, 0, 1,
		]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x00, 0x00, 0x00, 0xff]);

		let nal2 = iter.next().unwrap().unwrap();
		assert_eq!(nal2.as_ref(), &[0x68]);

		assert!(iter.next().is_none());

		// Make sure the data is still in the buffer.
		assert_eq!(data.as_ref(), &[0, 0, 1]);
	}

	// Tests for flush - extracts final NAL without trailing start code

	#[test]
	fn test_flush_after_iteration() {
		// Normal case: iterate over NALs, then flush the final one
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, // First NAL
			0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // Second NAL (final, no trailing start code)
		]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		assert!(iter.next().is_none());

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.as_ref(), &[0x68, 0xce, 0x3c, 0x80]);
	}

	#[test]
	fn test_flush_single_nal() {
		// Buffer contains only a single NAL with no trailing start code
		let mut data = Bytes::from(vec![0, 0, 1, 0x67, 0x42, 0x00, 0x1f]);
		let iter = NalIterator::new(&mut data);

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.as_ref(), &[0x67, 0x42, 0x00, 0x1f]);
	}

	#[test]
	fn test_flush_4_byte_start_code() {
		// Test flush with 4-byte start code
		let mut data = Bytes::from(vec![0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0xff]);
		let iter = NalIterator::new(&mut data);

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.as_ref(), &[0x65, 0x88, 0x84, 0x00, 0xff]);
	}

	#[test]
	fn test_flush_no_start_code() {
		// Buffer doesn't start with a start code and has no cached start position
		let mut data = Bytes::from(vec![0x67, 0x42, 0x00, 0x1f]);
		let iter = NalIterator::new(&mut data);

		let result = iter.flush();
		assert!(result.is_err());
	}

	#[test]
	fn test_flush_empty_buffer() {
		// Empty buffer should return None
		let mut data = Bytes::from(vec![]);
		let iter = NalIterator::new(&mut data);

		let result = iter.flush().unwrap();
		assert!(result.is_none());
	}

	#[test]
	fn test_flush_incomplete_start_code() {
		// Buffer has incomplete start code (not enough bytes)
		let mut data = Bytes::from(vec![0, 0]);
		let iter = NalIterator::new(&mut data);

		let result = iter.flush().unwrap();
		assert!(result.is_none());
	}

	#[test]
	fn test_flush_multiple_nals_then_flush() {
		// Iterate over multiple NALs, then flush the final one
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, // SPS
			0, 0, 0, 1, 0x68, 0xce, // PPS
			0, 0, 0, 1, 0x65, 0x88, 0x84, // IDR (final NAL)
		]);
		let mut iter = NalIterator::new(&mut data);

		let sps = iter.next().unwrap().unwrap();
		assert_eq!(sps.as_ref(), &[0x67, 0x42]);

		let pps = iter.next().unwrap().unwrap();
		assert_eq!(pps.as_ref(), &[0x68, 0xce]);

		assert!(iter.next().is_none());

		let idr = iter.flush().unwrap().unwrap();
		assert_eq!(idr.as_ref(), &[0x65, 0x88, 0x84]);
	}

	#[test]
	fn test_flush_empty_final_nal() {
		// Edge case: final NAL is empty (just a start code with no data)
		let mut data = Bytes::from(vec![
			0, 0, 0, 1, 0x67, 0x42, // First NAL
			0, 0, 0, 1, // Second NAL (empty)
		]);
		let mut iter = NalIterator::new(&mut data);

		let nal1 = iter.next().unwrap().unwrap();
		assert_eq!(nal1.as_ref(), &[0x67, 0x42]);

		assert!(iter.next().is_none());

		let final_nal = iter.flush().unwrap().unwrap();
		assert_eq!(final_nal.len(), 0);
	}

	fn annexb(nals: &[&[u8]]) -> Bytes {
		build_prefix(
			nals.iter()
				.map(|nal| Bytes::copy_from_slice(nal))
				.collect::<Vec<_>>()
				.iter(),
		)
	}

	fn length_prefixed(nals: &[&[u8]]) -> Vec<u8> {
		let mut out = Vec::new();
		for nal in nals {
			out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			out.extend_from_slice(nal);
		}
		out
	}

	#[test]
	fn in_band_h264_keyframes_carry_sps_then_pps() {
		let aud = &[0x09, 0xf0][..];
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];
		let idr = &[0x65, 0x88, 0x84, 0x21][..];
		let sei = &[0x06, 0x05, 0xff][..];
		let sps2 = &[0x67, 0x42, 0xc0, 0x1f, 0xaa][..];
		let delta = &[0x61, 0xe0, 0x12][..];

		let mut tx = InBand::new(InBandCodec::H264);
		assert!(tx.transform(annexb(&[aud, sps, pps]), true).unwrap().is_none());
		let full = tx.transform(annexb(&[sps, pps, idr]), true).unwrap().unwrap();
		assert_eq!(full.as_ref(), length_prefixed(&[sps, pps, idr]));

		// A partial keyframe gets the cached sets, never a PPS ahead of its SPS.
		let pps_only = tx.transform(annexb(&[pps, idr]), true).unwrap().unwrap();
		assert_eq!(pps_only.as_ref(), length_prefixed(&[sps, pps, idr]));
		let reordered = tx.transform(annexb(&[pps, sps2, idr]), true).unwrap().unwrap();
		assert_eq!(reordered.as_ref(), length_prefixed(&[sps2, pps, idr]));

		// The delimiter stays first, and SEI follows the sets it may reference.
		let sei_first = tx.transform(annexb(&[aud, sei, idr]), true).unwrap().unwrap();
		assert_eq!(sei_first.as_ref(), length_prefixed(&[aud, sps2, pps, sei, idr]));

		// Delta frames are not given parameter sets.
		let delta_out = tx.transform(annexb(&[delta]), false).unwrap().unwrap();
		assert_eq!(delta_out.as_ref(), length_prefixed(&[delta]));
	}

	#[test]
	fn in_band_h265_keyframes_carry_vps_sps_pps() {
		use crate::codec::h265::fixtures::{PPS, SPS, SPS_PYRAMID, VPS};
		let (vps, sps, pps, sps2) = (VPS, SPS, PPS, SPS_PYRAMID);
		let idr = &[0x26, 0x01, 0xaa][..];
		let sei = &[0x4e, 0x01, 0x05][..];

		// An access unit with no slice is not a sample; its sets wait for the next keyframe.
		let mut tx = InBand::new(InBandCodec::H265);
		assert!(tx.transform(annexb(&[vps, sps, pps]), true).unwrap().is_none());
		assert!(tx.transform(annexb(&[sei]), true).unwrap().is_none());
		let bare = tx.transform(annexb(&[idr]), true).unwrap().unwrap();
		assert_eq!(bare.as_ref(), length_prefixed(&[vps, sps, pps, idr]));

		// A new SPS alone still follows the VPS it references.
		let replaced = tx.transform(annexb(&[sps2, idr]), true).unwrap().unwrap();
		assert_eq!(replaced.as_ref(), length_prefixed(&[vps, sps2, pps, idr]));

		let sei_first = tx.transform(annexb(&[sei, idr]), true).unwrap().unwrap();
		assert_eq!(sei_first.as_ref(), length_prefixed(&[vps, sps2, pps, sei, idr]));
	}

	/// A parameter set keys by the id it defines, so a stream redefining ids holds one entry
	/// per id, the last, however many sets it sends. A set too short for its id is refused.
	#[test]
	fn parameter_sets_key_by_id() {
		use crate::codec::h265::fixtures::{PPS, SPS, SPS_PYRAMID, VPS};

		let mut sets = ParameterSets::new();
		for last in 0..=u8::MAX {
			// pic_parameter_set_id is ue(v): `1` is id 0, `010` is id 1.
			for head in [0xce, 0x4e] {
				let pps = Bytes::copy_from_slice(&[0x68, head, 0x3c, last]);
				InBandCodec::H264.insert_parameter_set(&mut sets, &pps).unwrap();
			}
		}
		assert_eq!(sets.len(), 2);
		assert_eq!(sets[&0].as_ref(), [0x68, 0xce, 0x3c, 0xff]);
		assert_eq!(sets[&1].as_ref(), [0x68, 0x4e, 0x3c, 0xff]);

		for nal in [VPS, SPS, SPS_PYRAMID, PPS] {
			let mut sets = ParameterSets::new();
			InBandCodec::H265
				.insert_parameter_set(&mut sets, &Bytes::from_static(nal))
				.unwrap();
			assert_eq!(sets.keys().collect::<Vec<_>>(), [&0]);
		}

		let truncated = Bytes::from_static(&[0x67, 0x42, 0xc0]);
		let err = InBandCodec::H264.insert_parameter_set(&mut ParameterSets::new(), &truncated);
		assert!(matches!(err, Err(Error::ParameterSetId(7))), "{err:?}");
	}

	#[test]
	fn in_band_keyframe_without_parameter_sets_fails() {
		use crate::codec::h265::fixtures::{PPS, SPS};
		let (sps, pps) = (SPS, PPS);
		let idr = &[0x26, 0x01, 0xaa][..];

		let mut tx = InBand::new(InBandCodec::H265);
		let err = tx.transform(annexb(&[sps, pps, idr]), true).unwrap_err();
		assert!(matches!(err, Error::MissingParameterSet("VPS")), "{err}");

		let mut tx = InBand::new(InBandCodec::H264);
		let err = tx.transform(annexb(&[&[0x65, 0x88][..]]), true).unwrap_err();
		assert!(matches!(err, Error::MissingParameterSet("SPS")), "{err}");
	}
}
