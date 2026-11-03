//! An HEVCDecoderConfigurationRecord canonicalized as its bytes arrive.

use scuffle_h265::NALUnitType;

use super::Error;
use crate::Result;

/// The record's fields before its arrays, through `numOfArrays` (ISO/IEC 14496-15 §8.3.2.1.2).
const FIXED_LEN: usize = 23;

/// The parameter set types a record keeps, and how many each can hold: one per id its
/// `*_parameter_set_id` can name (H.265 §7.4.3.1 to §7.4.3.3).
const SETS: [(NALUnitType, usize); 3] = [
	(NALUnitType::VpsNut, 16),
	(NALUnitType::SpsNut, 16),
	(NALUnitType::PpsNut, 64),
];

/// Canonicalizes an hvcC as it arrives: the fixed fields and the parameter set arrays are kept,
/// and every other array, such as the SEI holding x265's encoder settings, is discarded unread.
///
/// SEI arrays have no finite bound, while the parameter sets are bounded by their id spaces, each
/// at most 64 KiB by its `u16` length. A record past those, truncated, or followed by bytes past
/// its last array is refused.
pub(super) struct HvccFilter {
	/// The canonical record so far.
	out: Vec<u8>,
	/// A fixed-size field split across chunks.
	field: Vec<u8>,
	state: State,
	/// Arrays yet to start.
	arrays: u8,
	/// Arrays kept in `out`.
	kept: u8,
	/// Parameter sets kept, per [`SETS`] entry.
	sets: [usize; SETS.len()],
}

#[derive(Clone, Copy)]
enum State {
	/// The fixed fields, gathered in `out`.
	Fixed,
	/// An array's NAL unit type and count.
	Array,
	/// The length of a NAL unit, of `nalus` left in its array, counting it. Kept when `keep`.
	Length { keep: bool, nalus: u16 },
	/// A NAL unit's body, `remaining` bytes from its end.
	Nalu { keep: bool, nalus: u16, remaining: u16 },
	/// Every array arrived.
	Done,
}

impl HvccFilter {
	pub fn new() -> Self {
		Self {
			out: Vec::with_capacity(FIXED_LEN),
			field: Vec::new(),
			state: State::Fixed,
			arrays: 0,
			kept: 0,
			sets: [0; SETS.len()],
		}
	}

	/// Bytes held: the canonical record so far and a split field.
	#[cfg(test)]
	pub fn retained(&self) -> usize {
		self.out.capacity() + self.field.capacity()
	}

	/// Take the record's next bytes.
	pub fn feed(&mut self, mut chunk: &[u8]) -> Result<()> {
		while !chunk.is_empty() {
			match self.state {
				State::Fixed => {
					let taken = take(&mut chunk, FIXED_LEN - self.out.len());
					self.out.extend_from_slice(taken);
					if self.out.len() == FIXED_LEN {
						self.arrays = self.out[FIXED_LEN - 1];
						self.next_array();
					}
				}
				State::Array => {
					let Some([kind, high, low]) = self.field(&mut chunk) else {
						continue;
					};
					let nalus = u16::from_be_bytes([high, low]);
					let set = SETS.iter().position(|(set, _)| set.0 == kind & 0x3f);
					if let Some(set) = set {
						self.sets[set] += usize::from(nalus);
						if self.sets[set] > SETS[set].1 {
							return Err(Error::UnsupportedCodecPrivate("V_MPEGH/ISO/HEVC".to_string()).into());
						}
						self.out.extend_from_slice(&[kind, high, low]);
						self.kept += 1;
					}
					self.next_nalu(set.is_some(), nalus);
				}
				State::Length { keep, nalus } => {
					let Some(length) = self.field(&mut chunk) else {
						continue;
					};
					if keep {
						self.out.extend_from_slice(&length);
					}
					match u16::from_be_bytes(length) {
						0 => self.next_nalu(keep, nalus - 1),
						remaining => self.state = State::Nalu { keep, nalus, remaining },
					}
				}
				State::Nalu { keep, nalus, remaining } => {
					let taken = take(&mut chunk, remaining.into());
					if keep {
						self.out.extend_from_slice(taken);
					}
					let remaining = remaining - taken.len() as u16;
					self.state = State::Nalu { keep, nalus, remaining };
					if remaining == 0 {
						self.next_nalu(keep, nalus - 1);
					}
				}
				State::Done => return Err(Error::InvalidHvcc.into()),
			}
		}
		Ok(())
	}

	/// The canonical record, once every array arrived.
	pub fn finish(mut self) -> Result<Vec<u8>> {
		if !matches!(self.state, State::Done) {
			return Err(Error::InvalidHvcc.into());
		}
		self.out[FIXED_LEN - 1] = self.kept;
		Ok(self.out)
	}

	/// Start the next array, or end the record after its last.
	fn next_array(&mut self) {
		self.state = match self.arrays.checked_sub(1) {
			Some(arrays) => {
				self.arrays = arrays;
				State::Array
			}
			None => State::Done,
		};
	}

	/// Start the next of `nalus` NAL units left in an array, or the next array after its last.
	fn next_nalu(&mut self, keep: bool, nalus: u16) {
		match nalus {
			0 => self.next_array(),
			_ => self.state = State::Length { keep, nalus },
		}
	}

	/// An `N`-byte field, once its last byte arrives.
	fn field<const N: usize>(&mut self, chunk: &mut &[u8]) -> Option<[u8; N]> {
		let taken = take(chunk, N - self.field.len());
		self.field.extend_from_slice(taken);
		let field = self.field.as_slice().try_into().ok()?;
		self.field.clear();
		Some(field)
	}
}

/// Split off up to `max` bytes from the front of `chunk`.
fn take<'a>(chunk: &mut &'a [u8], max: usize) -> &'a [u8] {
	let (taken, rest) = chunk.split_at(max.min(chunk.len()));
	*chunk = rest;
	taken
}
