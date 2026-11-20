//! Grades a raw TS release against the source it was imported from.
//!
//! The admitted interval is the source from the first packet of the first group the release
//! writes to the end. Every packet the release writes must sit at its exact position in that
//! interval: nothing rewritten, duplicated or reordered, null packets, tables, unknown PIDs and
//! adaptation bytes included. Each object must go out on the source's PCR schedule, so a missing
//! object leaves its time in the output instead of pulling the rest forward. The bytes the
//! importer withheld while acquiring, before its first group, are [`Admitted::prefix`], not part
//! of the claim.
//!
//! Identity is not decodability: a missing object can hold part of a PES or the next table, so a
//! gap is reported as [`Loss`] and never graded as decodable here.

use std::time::Duration;

use bytes::Bytes;
use kio::clock::Instant;

use crate::container::Frame;

const PACKET: usize = 188;

/// One object as published: its group, the PCR time of its first byte, and its packets. A break
/// marker, an object with no bytes, has an empty payload and the time of the object before it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Object {
	pub group: u64,
	pub micros: u64,
	pub payload: Bytes,
}

/// The published objects, placed on the source they came from.
pub(super) struct Admitted<'a> {
	source: &'a [u8],
	objects: &'a [Object],
	/// Where each object's first packet sits in `source`.
	offsets: Vec<usize>,
}

/// What a release left out of its admitted interval.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Loss {
	/// The published object the release opened on.
	pub first: usize,
	/// The published objects from `first` on that it never wrote.
	pub missing: Vec<usize>,
}

impl<'a> Admitted<'a> {
	/// Place `objects` on `source`, checking they are its packets verbatim, in order, from the
	/// first published group to the end.
	pub fn new(source: &'a [u8], objects: &'a [Object]) -> Self {
		assert!(!objects.is_empty(), "nothing was published");
		let published: usize = objects.iter().map(|object| object.payload.len()).sum();
		assert!(published <= source.len(), "more was published than the source holds");
		let start = source.len() - published;
		assert_eq!(start % PACKET, 0, "the first group starts on the packet grid");
		let mut offsets = Vec::with_capacity(objects.len());
		let mut offset = start;
		for (i, object) in objects.iter().enumerate() {
			assert!(
				!object.payload.is_empty() && object.payload.len() % PACKET == 0,
				"object {i} is whole packets"
			);
			assert!(
				source[offset..offset + object.payload.len()] == object.payload[..],
				"object {i} is the source's packets {}.. verbatim",
				offset / PACKET
			);
			offsets.push(offset);
			offset += object.payload.len();
		}
		Self {
			source,
			objects,
			offsets,
		}
	}

	/// Bytes of the source before the first published group, which acquisition withheld.
	pub fn prefix(&self) -> usize {
		self.offsets[0]
	}

	/// Grade what a release wrote, and when, against the admitted interval it opened.
	///
	/// The release must open on the first object of a group, write every object at its exact
	/// position after that in order, whole, its first run stamped as published. Each run must be
	/// stamped, and written on the clock its first object set, within `tolerance` of its first
	/// byte's time: the object's PCR time, and for a later run the share of the way to the next
	/// object written that its bytes before it are of the object's.
	pub fn grade(&self, released: &[(Instant, Frame)], tolerance: Duration) -> Loss {
		let Some((start, opened)) = released.first() else {
			panic!("the release wrote nothing");
		};
		let mut matched: Vec<usize> = Vec::with_capacity(released.len());
		// Each run's object, and its first byte's offset in it.
		let mut runs = Vec::with_capacity(released.len());
		let mut next = 0;
		let mut written = 0;
		for (i, (_, frame)) in released.iter().enumerate() {
			if let Some(&found) = matched.last()
				&& written < self.objects[found].payload.len()
			{
				let at = self.offsets[found] + written;
				assert!(
					written + frame.payload.len() <= self.objects[found].payload.len()
						&& self.source[at..].starts_with(&frame.payload),
					"released run {i} does not continue object {found} at packet {}",
					at / PACKET
				);
				runs.push((matched.len() - 1, written));
				written += frame.payload.len();
				continue;
			}
			let micros = frame.timestamp.as_micros().unwrap() as u64;
			let Some(found) = (next..self.objects.len()).find(|&j| self.objects[j].micros == micros) else {
				panic!("released run {i} at {micros} us opens no published object after the one before it");
			};
			let offset = self.offsets[found];
			assert!(
				frame.payload.len() <= self.objects[found].payload.len()
					&& self.source[offset..].starts_with(&frame.payload),
				"released run {i} is not the source's packets {}.. verbatim",
				offset / PACKET
			);
			matched.push(found);
			runs.push((matched.len() - 1, 0));
			written = frame.payload.len();
			next = found + 1;
		}
		if let Some(&last) = matched.last() {
			assert_eq!(
				written,
				self.objects[last].payload.len(),
				"object {last} goes out whole"
			);
		}

		let first = matched[0];
		assert!(
			first == 0 || self.objects[first - 1].group != self.objects[first].group,
			"the release opens on object {first}, inside group {}",
			self.objects[first].group
		);

		let origin = opened.timestamp.as_micros().unwrap() as i128;
		for (i, ((at, frame), &(m, offset))) in released.iter().zip(&runs).enumerate() {
			let j = matched[m];
			let object = &self.objects[j];
			let mut due = object.micros as i128;
			if offset > 0 {
				let Some(&next) = matched.get(m + 1) else {
					panic!("released run {i} spreads object {j}, which no object follows");
				};
				let span = self.objects[next].micros as i128 - due;
				due += span * offset as i128 / object.payload.len() as i128;
			}
			let stamp = frame.timestamp.as_micros().unwrap() as i128 - due;
			assert!(
				stamp.unsigned_abs() <= tolerance.as_micros(),
				"released run {i} of object {j} is stamped {stamp} us off its first byte's time"
			);
			let lag = at.duration_since(*start).as_micros() as i128 - (due - origin);
			assert!(
				lag.unsigned_abs() <= tolerance.as_micros(),
				"released run {i} of object {j} goes out {lag} us off its first byte's time"
			);
		}

		let mut written = matched.into_iter().peekable();
		let missing = (first..self.objects.len())
			.filter(|j| written.next_if_eq(j).is_none())
			.collect();
		Loss { first, missing }
	}

	/// The admitted interval from `loss.first`, less the objects it is missing: what the release
	/// must have written.
	pub fn expected(&self, loss: &Loss) -> Vec<u8> {
		let mut missing = loss.missing.iter().peekable();
		(loss.first..self.objects.len())
			.filter(|j| missing.next_if_eq(&j).is_none())
			.flat_map(|j| self.objects[j].payload.iter().copied())
			.collect()
	}
}
