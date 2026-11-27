//! Experimental forward error correction for 1-RTT packets
//!
//! A source symbol is one packet's payload, its frames without trailing PADDING, behind a
//! two-byte length and zero-padded to the longest in its block. The length and padding are
//! never sent: they exist only in what a repair symbol is computed over, so that rebuilding a
//! packet also rebuilds where its frames end, which a STREAM frame without a length needs.
//!
//! A REPAIR frame names its block's packets by packet number. The receiver holds the payloads
//! of the packets it last received, and once a block's repairs are at least as many as the
//! packets it lacks, it rebuilds them and processes their frames as if they had arrived.

use std::{
	collections::{BTreeMap, VecDeque},
	sync::LazyLock,
};

use bytes::Bytes;

use crate::{
	TransportError,
	config::transport::fec::{Code, Send},
	frame::Repair,
};

/// Room every packet leaves for the REPAIR frame that will cover it, which carries a symbol as
/// long as the packet's payload: the frame's own fields and the symbol's length prefix
pub(super) const RESERVE: usize = Repair::SIZE_BOUND + PREFIX;

/// The length that opens every source symbol
const PREFIX: usize = 2;

/// How [`Repair::code`] names a code
fn code_id(code: Code) -> u64 {
	match code {
		Code::Xor => 0,
		Code::ReedSolomon => 1,
	}
}

/// GF(2^8) over x^8 + x^4 + x^3 + x^2 + 1, as Reed-Solomon codes commonly use
struct Field {
	exp: [u8; 510],
	log: [u8; 256],
}

static FIELD: LazyLock<Field> = LazyLock::new(|| {
	let mut field = Field {
		exp: [0; 510],
		log: [0; 256],
	};
	let mut x = 1u16;
	for i in 0..255 {
		field.exp[i] = x as u8;
		field.exp[i + 255] = x as u8;
		field.log[x as usize] = i as u8;
		x <<= 1;
		if x & 0x100 != 0 {
			x ^= 0x11d;
		}
	}
	field
});

impl Field {
	fn mul(&self, a: u8, b: u8) -> u8 {
		if a == 0 || b == 0 {
			return 0;
		}
		self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
	}

	fn inv(&self, a: u8) -> u8 {
		debug_assert_ne!(a, 0);
		self.exp[255 - self.log[a as usize] as usize]
	}
}

/// What repair `index` multiplies the block's source `position` by
///
/// Reed-Solomon's is a Cauchy matrix, 1 / (x + y) with the repairs' x counting down from 255
/// and the sources' y up from 0, so every square part of it inverts: any `n` repairs rebuild any
/// `n` sources. Its caller keeps the two ranges apart.
fn coefficient(code: u64, index: u8, position: usize) -> u8 {
	match code {
		0 => 1,
		_ => FIELD.inv((255 - index) ^ position as u8),
	}
}

/// `into[i] += factor * symbol(payload)[i]`, where the symbol is the payload behind its length
fn add_source(into: &mut [u8], factor: u8, payload: &[u8]) {
	let prefix = (payload.len() as u16).to_be_bytes();
	let symbol = prefix.iter().chain(payload);
	match factor {
		1 => into.iter_mut().zip(symbol).for_each(|(out, &byte)| *out ^= byte),
		_ => into
			.iter_mut()
			.zip(symbol)
			.for_each(|(out, &byte)| *out ^= FIELD.mul(factor, byte)),
	}
}

/// Builds repair frames for the packets a connection sends
pub(super) struct Encoder {
	code: Code,
	/// The most packets a block holds, and the widest span of packet numbers it covers, which
	/// keeps every source among the packets the peer still holds
	block: usize,
	overhead: usize,
	/// The open block's packets: each one's number and payload
	sources: Vec<(u64, Vec<u8>)>,
	/// Encoded REPAIR frames not yet sent, oldest first
	pending: VecDeque<Bytes>,
}

impl Encoder {
	/// The most repair frames kept unsent: beyond it the oldest are dropped, as their packets
	/// have long been acknowledged or retransmitted
	const MAX_PENDING: usize = 64;

	pub(super) fn new(config: &Send, peer_window: u64) -> Self {
		let block = usize::from(config.block)
			.min(usize::try_from(peer_window).unwrap_or(usize::MAX))
			.clamp(1, Repair::MAX_SOURCES);
		Self {
			code: config.code,
			block,
			overhead: config.overhead.into(),
			sources: Vec::new(),
			pending: VecDeque::new(),
		}
	}

	/// A packet carrying stream data was sent with these frames
	pub(super) fn source(&mut self, number: u64, payload: &[u8]) {
		// The frame lists its sources as ranges of consecutive packet numbers, so close the
		// block before one that would need a range, or a gap, the frame's size bound leaves out
		if let (Some(&(first, _)), Some(&(last, _))) = (self.sources.first(), self.sources.last()) {
			let ranges = 1 + self
				.sources
				.windows(2)
				.filter(|pair| pair[0].0 + 1 != pair[1].0)
				.count();
			let adjacent = last + 1 == number;
			if number - first >= self.block as u64
				|| (!adjacent && (ranges == Repair::MAX_RANGES || number - last > 1 << 14))
			{
				self.close();
			}
		}
		self.sources.push((number, payload.to_vec()));
		if self.sources.len() == self.block {
			self.close();
		}
	}

	/// Closes the open block, queueing its repairs
	pub(super) fn close(&mut self) {
		if self.sources.is_empty() {
			return;
		}
		let sources = std::mem::take(&mut self.sources);
		let code = code_id(self.code);
		let repairs = match self.code {
			Code::Xor => 1,
			// The matrix keeps the repairs' elements apart from the sources'
			Code::ReedSolomon => ((sources.len() * self.overhead + 50) / 100).clamp(1, 255 - sources.len()),
		};
		let longest = sources.iter().map(|(_, payload)| payload.len()).max().unwrap_or(0);
		for index in 0..repairs as u8 {
			let mut symbol = vec![0; PREFIX + longest];
			for (position, (_, payload)) in sources.iter().enumerate() {
				add_source(&mut symbol, coefficient(code, index, position), payload);
			}
			let repair = Repair {
				code,
				index,
				sources: sources.iter().map(|&(number, _)| number).collect(),
				symbol: symbol.into(),
			};
			let mut frame = Vec::with_capacity(Repair::SIZE_BOUND + repair.symbol.len());
			repair.encode(&mut frame);
			if self.pending.len() == Self::MAX_PENDING {
				self.pending.pop_front();
			}
			self.pending.push_back(frame.into());
		}
	}

	/// Drops the unsent frames a packet with `space` bytes for frames cannot carry, as after
	/// the path's MTU shrank
	pub(super) fn fit(&mut self, space: usize) {
		self.pending.retain(|frame| frame.len() <= space);
	}

	/// The next frame to send
	pub(super) fn next(&self) -> Option<&Bytes> {
		self.pending.front()
	}

	pub(super) fn pop(&mut self) -> Option<Bytes> {
		self.pending.pop_front()
	}
}

/// Rebuilds lost packets from the repairs a connection receives
pub(super) struct Decoder {
	/// The most packets one repair may cover, as the peer was told
	window: usize,
	/// The payloads of the packets last received or rebuilt, by packet number
	held: BTreeMap<u64, Bytes>,
	/// The repairs of blocks that still lack more packets than they have repairs, oldest first
	repairs: VecDeque<Repair>,
}

impl Decoder {
	pub(super) fn new(window: u8) -> Self {
		Self {
			window: usize::from(window).min(Repair::MAX_SOURCES),
			held: BTreeMap::new(),
			repairs: VecDeque::new(),
		}
	}

	/// Holds a received or rebuilt packet's frames to rebuild others with
	pub(super) fn hold(&mut self, number: u64, payload: Bytes) {
		self.held.insert(number, payload);
		// A sender's block spans at most `window` packet numbers, and reordering is far
		// narrower, so twice the window covers every block still arriving
		while self.held.len() > 2 * self.window {
			self.held.pop_first();
		}
	}

	/// Takes in a repair, returning the packets it lets the block rebuild: each one's number
	/// and frames
	pub(super) fn repair(&mut self, repair: Repair) -> Result<Vec<(u64, Bytes)>, TransportError> {
		if repair.sources.len() > self.window {
			return Err(TransportError::PROTOCOL_VIOLATION(
				"REPAIR covers more packets than the window",
			));
		}
		if repair.code > 1 || repair.sources.len() + usize::from(repair.index) > 255 {
			return Err(TransportError::FRAME_ENCODING_ERROR("REPAIR names no known symbol"));
		}
		if repair.symbol.len() < PREFIX {
			return Err(TransportError::FRAME_ENCODING_ERROR("REPAIR symbol too short"));
		}
		let missing: Vec<usize> = (0..repair.sources.len())
			.filter(|&position| !self.held.contains_key(&repair.sources[position]))
			.collect();
		// The block's other repairs, taken out until it is known whether they suffice
		let (mut repairs, others): (Vec<Repair>, Vec<Repair>) = std::mem::take(&mut self.repairs)
			.into_iter()
			.partition(|other| other.sources == repair.sources && other.code == repair.code);
		self.repairs = others.into();
		if missing.is_empty() {
			return Ok(Vec::new());
		}
		repairs.retain(|other| other.index != repair.index);
		repairs.push(repair);
		// XOR's one symbol rebuilds one packet
		let most = if repairs[0].code == 0 { 1 } else { repairs.len() };
		if missing.len() > most {
			// Kept until the block has enough; the oldest make room, their blocks long past
			self.repairs.extend(repairs);
			while self.repairs.len() > self.window {
				self.repairs.pop_front();
			}
			return Ok(Vec::new());
		}
		repairs.truncate(missing.len());
		self.solve(&repairs, &missing)
	}

	/// Rebuilds the `missing` positions of a block from as many of its `repairs`
	fn solve(&self, repairs: &[Repair], missing: &[usize]) -> Result<Vec<(u64, Bytes)>, TransportError> {
		let mismatch = || TransportError::PROTOCOL_VIOLATION("REPAIR symbols disagree with their packets");
		let (code, sources) = (repairs[0].code, &repairs[0].sources);
		let len = repairs[0].symbol.len();
		// Each row starts as its repair symbol less the packets held, leaving a combination of
		// the missing ones
		let mut rows = Vec::with_capacity(repairs.len());
		for repair in repairs {
			if repair.symbol.len() != len {
				return Err(mismatch());
			}
			let mut row = repair.symbol.to_vec();
			for (position, number) in sources.iter().enumerate() {
				let Some(payload) = self.held.get(number) else { continue };
				if PREFIX + payload.len() > len {
					return Err(mismatch());
				}
				add_source(&mut row, coefficient(code, repair.index, position), payload);
			}
			rows.push(row);
		}
		// Gauss-Jordan elimination over the field, on a matrix whose every square part inverts
		let mut matrix: Vec<Vec<u8>> = repairs
			.iter()
			.map(|repair| {
				missing
					.iter()
					.map(|&position| coefficient(code, repair.index, position))
					.collect()
			})
			.collect();
		for column in 0..missing.len() {
			let pivot = (column..missing.len())
				.find(|&row| matrix[row][column] != 0)
				.ok_or_else(mismatch)?;
			matrix.swap(column, pivot);
			rows.swap(column, pivot);
			let inverse = FIELD.inv(matrix[column][column]);
			matrix[column].iter_mut().for_each(|x| *x = FIELD.mul(*x, inverse));
			rows[column].iter_mut().for_each(|x| *x = FIELD.mul(*x, inverse));
			for row in 0..missing.len() {
				let factor = matrix[row][column];
				if row == column || factor == 0 {
					continue;
				}
				let (pivot_matrix, pivot_row) = (matrix[column].clone(), rows[column].clone());
				matrix[row]
					.iter_mut()
					.zip(&pivot_matrix)
					.for_each(|(x, &p)| *x ^= FIELD.mul(factor, p));
				rows[row]
					.iter_mut()
					.zip(&pivot_row)
					.for_each(|(x, &p)| *x ^= FIELD.mul(factor, p));
			}
		}
		missing
			.iter()
			.zip(rows)
			.map(|(&position, symbol)| {
				let payload = usize::from(u16::from_be_bytes([symbol[0], symbol[1]]));
				// A packet has at least one frame, and what follows its frames is padding
				if payload == 0 || PREFIX + payload > len || symbol[PREFIX + payload..].iter().any(|&b| b != 0) {
					return Err(mismatch());
				}
				let frames = Bytes::from(symbol).slice(PREFIX..PREFIX + payload);
				Ok((sources[position], frames))
			})
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::frame::{Frame, Iter};

	fn payloads(count: usize) -> Vec<(u64, Vec<u8>)> {
		// Uneven lengths, and packet numbers with a gap
		(0..count)
			.map(|i| {
				let number = 100 + i as u64 + if i > 2 { 3 } else { 0 };
				(number, vec![i as u8 + 1; 20 + (i * 37) % 90])
			})
			.collect()
	}

	fn repairs(code: Code, overhead: u8, sources: &[(u64, Vec<u8>)]) -> Vec<Repair> {
		let config = Send {
			code,
			block: 32,
			overhead,
		};
		let mut encoder = Encoder::new(&config, 64);
		for (number, payload) in sources {
			encoder.source(*number, payload);
		}
		encoder.close();
		std::iter::from_fn(|| encoder.pop())
			.map(|frame| {
				let mut frames = Iter::new(frame).unwrap();
				let Some(Ok(Frame::Repair(repair))) = frames.next() else {
					panic!("not a REPAIR frame");
				};
				assert!(frames.next().is_none());
				repair
			})
			.collect()
	}

	/// Every way of losing as many packets as there are repairs is rebuilt exactly
	#[test]
	fn rebuilds_any_losses() {
		for code in [Code::Xor, Code::ReedSolomon] {
			let sources = payloads(8);
			let repairs = repairs(code, 40, &sources);
			let lose = if code == Code::Xor { 1 } else { 3 };
			assert_eq!(repairs.len(), lose);
			for lost in 0u32..1 << sources.len() {
				if lost.count_ones() as usize != lose {
					continue;
				}
				let mut decoder = Decoder::new(64);
				for (i, (number, payload)) in sources.iter().enumerate() {
					if lost & (1 << i) == 0 {
						decoder.hold(*number, payload.clone().into());
					}
				}
				let mut rebuilt = Vec::new();
				for repair in &repairs {
					assert!(rebuilt.is_empty(), "rebuilt before the last repair");
					rebuilt = decoder.repair(repair.clone()).unwrap();
				}
				let expected: Vec<_> = sources
					.iter()
					.enumerate()
					.filter(|(i, _)| lost & (1 << i) != 0)
					.map(|(_, (number, payload))| (*number, Bytes::from(payload.clone())))
					.collect();
				assert_eq!(rebuilt, expected, "{code:?} lost {lost:#b}");
			}
		}
	}

	/// A block lacking more packets than it has repairs rebuilds nothing, and one lacking none
	/// keeps nothing
	#[test]
	fn too_many_losses() {
		let sources = payloads(8);
		let repairs = repairs(Code::ReedSolomon, 25, &sources);
		assert_eq!(repairs.len(), 2);
		let mut decoder = Decoder::new(64);
		for (number, payload) in &sources[3..] {
			decoder.hold(*number, payload.clone().into());
		}
		for repair in &repairs {
			assert!(decoder.repair(repair.clone()).unwrap().is_empty());
		}
		for (number, payload) in &sources[..3] {
			decoder.hold(*number, payload.clone().into());
		}
		assert!(decoder.repair(repairs[0].clone()).unwrap().is_empty());
		assert!(decoder.repairs.is_empty());
	}

	/// A repair over more packets than the window is refused
	#[test]
	fn beyond_the_window() {
		let sources = payloads(8);
		let repair = repairs(Code::Xor, 20, &sources).remove(0);
		assert!(Decoder::new(7).repair(repair).is_err());
	}

	/// A symbol that disagrees with the packets it covers closes the connection instead of
	/// handing over frames nobody sent
	#[test]
	fn corrupt_symbol() {
		let sources = payloads(4);
		let mut repair = repairs(Code::Xor, 20, &sources).remove(0);
		let mut symbol = repair.symbol.to_vec();
		symbol[0] ^= 0x40;
		repair.symbol = symbol.into();
		let mut decoder = Decoder::new(64);
		for (number, payload) in &sources[1..] {
			decoder.hold(*number, payload.clone().into());
		}
		assert!(decoder.repair(repair).is_err());
	}
}
