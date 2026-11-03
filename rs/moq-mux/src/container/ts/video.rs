//! PES-independent access-unit ownership for the TS importer.
//!
//! This assembler owns PES stamps at the byte where an AU begins. The public
//! elementary splitters keep their caller-supplied complete-frame timestamp mode.

use std::collections::VecDeque;

use bytes::{Buf, Bytes, BytesMut};
use moq_net::Timestamp;

use super::{Pending, Retained, on_clock, reorder_delay, unit_error};
use crate::codec::annexb::{START_CODE, after_start_code, find_start_code};

#[derive(Clone, Copy)]
pub(super) enum Codec {
	Avc,
	Hevc,
}

impl Codec {
	fn end(self, nal: &[u8]) -> bool {
		match self {
			Self::Avc => matches!(nal[0] & 31, 10 | 11),
			Self::Hevc => matches!((nal[0] >> 1) & 63, 36 | 37),
		}
	}

	/// Whether this NAL begins a picture or its leading non-VCL data
	/// (H.264 7.4.1.2.3, H.265 7.4.2.4.4), and whether it is a slice.
	fn boundary(self, nal: &[u8]) -> anyhow::Result<(bool, bool)> {
		match self {
			Self::Avc => {
				let header = *nal
					.first()
					.ok_or(crate::codec::h264::Error::NalTooShort)
					.map_err(crate::Error::from)
					.map_err(unit_error)?;
				if header & 0x80 != 0 {
					return Err(unit_error(crate::codec::h264::Error::ForbiddenZeroBit.into()));
				}
				let kind = header & 31;
				let slice = matches!(kind, 1..=5);
				let first = if slice {
					*nal.get(1)
						.ok_or(crate::codec::h264::Error::NalTooShort)
						.map_err(crate::Error::from)
						.map_err(unit_error)?
						& 0x80 != 0
				} else {
					matches!(kind, 6..=9 | 14..=18)
				};
				Ok((first, slice))
			}
			Self::Hevc => {
				let header = *nal
					.first()
					.ok_or(crate::codec::h265::Error::NalTooShort)
					.map_err(crate::Error::from)
					.map_err(unit_error)?;
				if header & 0x80 != 0 {
					return Err(unit_error(crate::codec::h265::Error::ForbiddenZeroBit.into()));
				}
				let kind = (header >> 1) & 63;
				let slice = kind <= 31;
				let first = if slice {
					*nal.get(2)
						.ok_or(crate::codec::h265::Error::NalTooShort)
						.map_err(crate::Error::from)
						.map_err(unit_error)?
						& 0x80 != 0
				} else {
					matches!(kind, 32..=35 | 39 | 41..=44 | 48..=55)
				};
				Ok((first, slice))
			}
		}
	}
}

struct Span {
	len: usize,
	pts: Option<Timestamp>,
	reorder: Option<Timestamp>,
}

pub(super) struct Unit {
	pub data: Bytes,
	pub pts: Option<Timestamp>,
	pub reorder: Option<Timestamp>,
	end_sequence: bool,
	parameters: bool,
	// Charges remain live while the caller parses and publishes this AU.
	_retained: Retained,
}

#[path = "video_timing.rs"]
mod timing;

pub(super) struct Assembly {
	pub clock: timing::Clock,
	damage: Option<(u64, anyhow::Error)>,
	codec: Codec,
	tail: BytesMut,
	searched: usize,
	spans: VecDeque<Span>,
	current: BytesMut,
	stamp: Option<(Option<Timestamp>, Option<Timestamp>)>,
	has_slice: bool,
	ending: bool,
	parameters: bool,
	retained: Option<Retained>,
}

impl Assembly {
	pub fn new(codec: Codec) -> Self {
		Self {
			clock: timing::Clock::default(),
			damage: None,
			codec,
			tail: BytesMut::new(),
			searched: 0,
			spans: VecDeque::new(),
			current: BytesMut::new(),
			stamp: None,
			has_slice: false,
			ending: false,
			parameters: false,
			retained: None,
		}
	}

	pub fn push(&mut self, mut pending: Pending) -> anyhow::Result<()> {
		let pts = on_clock(pending.pts, pending.offset)?;
		let reorder = reorder_delay(pending.pts, pending.dts);
		if pending.data.is_empty() {
			return Ok(());
		}
		if let Some(retained) = &mut self.retained {
			debug_assert!(std::sync::Arc::ptr_eq(&retained.staging, &pending.retained.staging));
			retained.bytes += pending.retained.bytes;
			retained.units += pending.retained.units;
			pending.retained.bytes = 0;
			pending.retained.units = 0;
		} else {
			self.retained = Some(pending.retained);
		}
		self.spans.push_back(Span {
			len: pending.data.len(),
			pts,
			reorder,
		});
		self.tail.extend_from_slice(&pending.data);
		self.account()
	}

	fn account(&mut self) -> anyhow::Result<()> {
		if let Some(retained) = &mut self.retained {
			retained.resize(
				self.tail.len() + self.current.len(),
				self.spans.len() + usize::from(self.stamp.is_some()),
			)?;
		}
		Ok(())
	}

	/// Return one AU at a time so later parameter sets cannot precede its validation.
	pub fn next(&mut self, eof: bool) -> anyhow::Result<Option<Unit>> {
		loop {
			if self.tail.is_empty() {
				return if eof { self.emit() } else { Ok(None) };
			}
			let start = after_start_code(&self.tail)
				.map_err(crate::Error::from)
				.map_err(unit_error)?;
			let Some(start) = start else {
				if eof {
					return Err(unit_error(crate::codec::annexb::Error::MissingStartCode.into()));
				}
				return Ok(None);
			};
			// A new picture's header closes the preceding AU even while its final
			// NAL is still arriving. Never treat a PES boundary as this evidence.
			if self.has_slice {
				let header = match self.codec {
					Codec::Avc => 2,
					Codec::Hevc => 3,
				};
				if self.tail.len() < start + header {
					// Until the boundary is decided, this prefix may own the next AU's
					// stamp, so it must not be scanned into the spent spans below.
					if !eof {
						return Ok(None);
					}
				} else if self.codec.boundary(&self.tail[start..])?.0 {
					return self.emit();
				}
			}
			if self.stamp.is_none() {
				// The optional zero_byte may be a trailing zero from the preceding
				// PES. Ownership follows the required three-byte start_code_prefix.
				let mut skip = start - 3;
				let span = self
					.spans
					.iter_mut()
					.find(|span| {
						if skip < span.len {
							true
						} else {
							skip -= span.len;
							false
						}
					})
					.expect("every input byte has an owner");
				self.stamp = Some((span.pts.take(), span.reorder.take()));
			}
			self.account()?;
			let from = self.searched.max(start);
			let end = match find_start_code(&self.tail[from..]) {
				Some((offset, _)) => from + offset,
				None if eof => self.tail.len(),
				None => {
					// A hostile stream can split one large NAL across tiny PES.
					// Scan each byte once, retaining just the possible split prefix.
					self.searched = self.tail.len().saturating_sub(3).max(start);
					// Only the possible split prefix can start another AU. Earlier
					// PES stamps are spent; retain byte coverage without one entry
					// per fragment of an arbitrarily large NAL.
					let mut scanned = 0;
					while self
						.spans
						.front()
						.is_some_and(|span| scanned + span.len <= self.searched)
					{
						scanned += self.spans.pop_front().unwrap().len;
					}
					if scanned > 0 {
						self.spans.push_front(Span {
							len: scanned,
							pts: None,
							reorder: None,
						});
					}
					self.account()?;
					return Ok(None);
				}
			};
			self.tail.advance(start);
			let nal = self.tail.copy_to_bytes(end - start);
			let consumed = end;
			self.searched = 0;
			let (_, slice) = self.codec.boundary(&nal)?;
			self.has_slice |= slice;
			self.ending = self.codec.end(&nal);
			self.parameters |= match self.codec {
				Codec::Avc => matches!(nal[0] & 31, 7 | 8),
				Codec::Hevc => matches!((nal[0] >> 1) & 63, 32..=34),
			};
			self.current.extend_from_slice(&START_CODE);
			self.current.extend_from_slice(&nal);
			let mut remaining = consumed;
			while remaining > 0 {
				let span = self.spans.front_mut().expect("consumed bytes have an owner");
				let take = remaining.min(span.len);
				span.len -= take;
				remaining -= take;
				if span.len == 0 {
					self.spans.pop_front();
				}
			}
			self.account()?;
			if self.ending {
				if self.has_slice {
					return self.emit();
				}
				self.current.clear();
				self.stamp = None;
				self.ending = false;
				self.clock.reset();
				self.account()?;
			}
		}
	}

	fn emit(&mut self) -> anyhow::Result<Option<Unit>> {
		if !self.has_slice {
			return Ok(None);
		}
		let (pts, reorder) = self.stamp.take().expect("a picture has a first byte");
		let data = std::mem::take(&mut self.current).freeze();
		self.has_slice = false;
		let retained = self
			.retained
			.as_mut()
			.expect("buffered bytes are charged")
			.take(data.len(), 1);
		Ok(Some(Unit {
			data,
			pts,
			reorder,
			end_sequence: std::mem::take(&mut self.ending),
			parameters: std::mem::take(&mut self.parameters),
			_retained: retained,
		}))
	}

	pub fn take_damage(&mut self) -> Option<(u64, anyhow::Error)> {
		self.damage.take()
	}

	/// Release everything buffered at end of input, keeping damage the final drain refused.
	pub fn finish(&mut self) {
		let damage = self.damage.take();
		*self = Self::new(self.codec);
		self.damage = damage;
	}

	pub fn reset(&mut self) {
		self.clock.reset();
		self.tail = BytesMut::new();
		self.searched = 0;
		self.spans.clear();
		self.current = BytesMut::new();
		self.stamp = None;
		self.has_slice = false;
		self.ending = false;
		self.parameters = false;
		self.retained = None;
	}
}

/// One proportional AVC clock must be declared explicitly by the TS descriptor;
/// an SPS fixed frame rate alone says nothing about numeric POC distance.
pub(super) fn temporal_poc(descriptors: &[super::catalog::Descriptor]) -> anyhow::Result<bool> {
	let mut proportional = false;
	for descriptor in descriptors.iter().filter(|d| d.tag == 0x2a) {
		let data = &descriptor.data;
		let first = *data
			.first()
			.ok_or_else(|| anyhow::anyhow!("truncated AVC timing descriptor"))?;
		let len = if first & 1 == 0 {
			2
		} else {
			let clock = *data
				.get(1)
				.ok_or_else(|| anyhow::anyhow!("truncated AVC timing descriptor"))?;
			if clock & 0x80 != 0 { 7 } else { 15 }
		};
		anyhow::ensure!(data.len() == len, "invalid AVC timing descriptor length");
		proportional |= data[len - 1] & 0xc0 == 0xc0;
	}
	Ok(proportional)
}

// Both codecs publish one AU before inspecting the next, committing parameter
// sets only after that AU is accepted. EOF and PMT retirement use the same path.
macro_rules! publish {
	($name:ident, $codec:ident) => {
		pub(super) fn $name(
			split: &mut crate::codec::$codec::Split,
			import: &mut crate::codec::$codec::Import,
			assembly: &mut Assembly,
			eof: bool,
		) -> anyhow::Result<u64> {
			let mut published = 0;
			let result = (|| {
				while let Some(unit) = assembly.next(eof)? {
					let params = unit.parameters.then(|| split.params());
					let result = (|| {
						// Structural damage is local even when it prevents timing
						// inference. No provisional timestamp reaches the producer.
						let provisional = unit.pts.unwrap_or(Timestamp::ZERO);
						let mut frames = split.decode(&unit.data, provisional).map_err(unit_error)?;
						frames.extend(split.flush(provisional).map_err(unit_error)?);
						anyhow::ensure!(frames.len() == 1, "unsupported TS video access-unit framing");
						for frame in &frames {
							if frame.keyframe {
								crate::codec::$codec::config(&frame.payload)
									.map_err(crate::Error::from)
									.map_err(unit_error)?;
							}
						}
						let pts = assembly.clock.$name(
							&unit,
							params.as_ref().into_iter().flat_map(|params| params.nals()),
						)?;
						let mut published = 0;
						for mut frame in frames {
							frame.timestamp = pts;
							published += u64::from(super::skip_missing_keyframe(import.decode([frame]))?);
						}
						anyhow::Ok(published)
					})();
					match result {
						Ok(units) => {
							published += units;
							assembly.clock.commit();
						}
						Err(err) if err.is::<super::Damaged>() => {
							// This AU has a known end. Keep the next AU's bytes,
							// which may already be here in the same or following PES.
							if let Some(params) = params {
								split.restore(params);
							}
							split.reset();
							assembly.clock.reset();
							import.cut(None)?;
							let count = assembly.damage.take().map_or(1, |(count, _)| count + 1);
							assembly.damage = Some((count, err));
							continue;
						}
						Err(err) => return Err(err),
					}
					if let Some(reorder) = unit.reorder {
						import.observe_reorder(reorder)?;
					}
					if unit.end_sequence {
						assembly.clock.reset();
					}
				}
				anyhow::Ok(())
			})();
			match result {
				Ok(()) => Ok(published),
				Err(err) if err.is::<super::Damaged>() => Err(err.context(Published(published))),
				Err(err) => Err(err),
			}
		}
	};
}
publish!(avc, h264);
publish!(hevc, h265);

#[derive(Debug, thiserror::Error)]
#[error("damaged TS video after {0} published access units")]
struct Published(u64);

pub(super) fn published(err: &anyhow::Error) -> u64 {
	err.downcast_ref::<Published>().map_or(0, |published| published.0)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Arc, atomic::Ordering};

	fn pending(staging: &Arc<super::super::Staging>, data: &[u8], pts: Option<u64>) -> Pending {
		let mut retained = Retained::new(staging.clone());
		retained.grow(data.len(), 0).unwrap();
		Pending {
			retained,
			data: data.to_vec(),
			pts,
			..Pending::empty()
		}
	}

	fn drain(assembly: &mut Assembly, eof: bool, units: &mut Vec<Unit>) {
		while let Some(unit) = assembly.next(eof).unwrap() {
			units.push(unit);
		}
	}

	#[test]
	fn prefix_ownership_and_staging_follow_the_bytes() {
		// Every cut of the required prefix and slice, for both 3/4-byte prefixes,
		// with and without a preceding AU awaiting its boundary. A continuation's
		// unrelated PTS never restamps this AU.
		for lead in [false, true] {
			for prefix in [&[0, 0, 1][..], &[0, 0, 0, 1][..]] {
				let data = [prefix, &[0x65, 0x80, 0xab, 0xcd]].concat();
				for cut in 1..data.len() {
					let staging = Arc::default();
					let mut assembly = Assembly::new(Codec::Avc);
					let mut units = Vec::new();
					if lead {
						assembly
							.push(pending(&staging, &[0, 0, 1, 0x65, 0x80, 0x11], None))
							.unwrap();
					}
					assembly.push(pending(&staging, &data[..cut], Some(90_000))).unwrap();
					drain(&mut assembly, false, &mut units);
					if !lead {
						assert_eq!(staging.bytes.load(Ordering::Relaxed), cut);
					}
					assembly.push(pending(&staging, &data[cut..], Some(180_000))).unwrap();
					drain(&mut assembly, false, &mut units);
					drain(&mut assembly, true, &mut units);
					assert_eq!(units.len(), 1 + usize::from(lead));
					let unit = units.last().unwrap();
					let owner = if prefix.len() == 4 && cut == 1 { 2 } else { 1 };
					assert_eq!(unit.pts.unwrap().as_secs(), owner, "lead {lead} cut {cut}");
					assert_eq!(
						unit.data.as_ref(),
						[START_CODE.as_ref(), &[0x65, 0x80, 0xab, 0xcd]].concat()
					);
					if !lead {
						assert_eq!(staging.bytes.load(Ordering::Relaxed), unit.data.len());
					}
					drop(units);
					assembly.reset();
					assert_eq!(staging.bytes.load(Ordering::Relaxed), 0);
					assert_eq!(staging.units.load(Ordering::Relaxed), 0);
				}
			}
		}
	}

	#[test]
	fn every_standard_opener_starts_the_next_au() {
		let avc = [6, 7, 8, 9, 14, 15, 16, 17, 18].map(|kind| vec![kind]);
		let hevc = [32, 33, 34, 35, 39, 41, 44, 48, 55].map(|kind| vec![kind << 1, 0x01]);
		let cases = [
			(Codec::Avc, vec![0x65, 0x80], vec![0x61, 0x80], avc),
			(Codec::Hevc, vec![0x26, 0x01, 0x80], vec![0x02, 0x01, 0x80], hevc),
		];
		for (codec, first, next, openers) in cases {
			for opener in openers {
				let staging = Arc::default();
				let mut assembly = Assembly::new(codec);
				let data = [&[0, 0, 1][..], &first, &[0, 0, 1], &opener, &[0, 0, 1], &next].concat();
				assembly.push(pending(&staging, &data, Some(90_000))).unwrap();
				let mut units = Vec::new();
				drain(&mut assembly, true, &mut units);
				assert_eq!(units.len(), 2, "opener {opener:?}");
				assert_eq!(
					&units[1].data[..START_CODE.len() + opener.len()],
					[START_CODE.as_ref(), &opener].concat()
				);
			}
		}
	}

	#[test]
	fn tiny_pes_fragments_release_spent_stamp_metadata() {
		let staging = Arc::default();
		let mut assembly = Assembly::new(Codec::Avc);
		let data = [&[0, 0, 1, 0x65, 0x80][..], &vec![0xff; 4096]].concat();
		for (i, byte) in data.iter().enumerate() {
			assembly
				.push(pending(&staging, &[*byte], Some(if i == 0 { 90_000 } else { 180_000 })))
				.unwrap();
			assert!(assembly.next(false).unwrap().is_none());
			// At most three possible prefix bytes, one scanned byte range,
			// and the current AU's stamp remain independently owned.
			assert!(staging.units.load(Ordering::Relaxed) <= 5);
		}
		assembly
			.push(pending(&staging, &[0, 0, 1, 0x65, 0x80], Some(270_000)))
			.unwrap();
		let first = assembly.next(false).unwrap().unwrap();
		assert_eq!(first.pts.unwrap().as_secs(), 1);
		assert_eq!(&first.data[4..], &data[3..]);
		let second = assembly.next(true).unwrap().unwrap();
		assert_eq!(second.pts.unwrap().as_secs(), 3);
		drop((first, second));
		assembly.reset();
		assert_eq!(staging.bytes.load(Ordering::Relaxed), 0);
		assert_eq!(staging.units.load(Ordering::Relaxed), 0);
	}

	#[test]
	fn lost_tail_releases_bytes_stamps_and_clock() {
		let staging = Arc::default();
		let mut assembly = Assembly::new(Codec::Avc);
		assembly
			.push(pending(&staging, &[0, 0, 1, 0x65, 0x80], Some(90_000)))
			.unwrap();
		assert!(assembly.next(false).unwrap().is_none());
		assembly.reset();
		assert_eq!(staging.bytes.load(Ordering::Relaxed), 0);
		assert_eq!(staging.units.load(Ordering::Relaxed), 0);
		assembly.push(pending(&staging, &[0, 0, 1, 0x65, 0x80], None)).unwrap();
		let unit = assembly.next(true).unwrap().unwrap();
		assert!(unit.pts.is_none());
	}
}
