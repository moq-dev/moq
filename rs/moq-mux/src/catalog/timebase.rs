use std::sync::{Arc, OnceLock};

use super::hang::CatalogExt;
use super::{Producer, Reserved};

/// One source's timestamp base on a catalog, shared by every importer reading it.
///
/// Made via [`Producer::timebase`]. An importer publishes its source's own timestamps, shifted onto
/// the catalog's [`Clock`](crate::Clock) by one offset fixed on its first frame: zero when that
/// frame places the clock (the catalog was not yet published, nor its clock taken), so the
/// timestamps stay verbatim, and the gap between the clock's reading and the frame's timestamp
/// otherwise. Every track of every importer minted from one timebase shares that offset, so
/// renditions on one timestamp base (an HLS import's variants, or the importer replacing one on a
/// new init segment) stay in sync, while separate timebases each get their own.
///
/// [`Producer::reserve`] mints a fresh timebase per call, which is what a lone importer wants.
/// Clones share the offset. Holding one never withholds the catalog: only a live [`Reserved`]
/// does.
pub struct Timebase<E: CatalogExt = ()> {
	pub(super) catalog: Producer<E>,
	offset: Arc<OnceLock<Offset>>,
}

impl<E: CatalogExt> Timebase<E> {
	pub(super) fn new(catalog: Producer<E>) -> Self {
		Self {
			catalog,
			offset: Default::default(),
		}
	}

	/// Begin reserving an importer's tracks, sharing this timebase's offset.
	///
	/// See [`Producer::reserve`] for how the reservation gates the catalog.
	pub fn reserve(&self) -> Reserved<E> {
		Reserved::new(self.clone())
	}

	/// Place this timebase's `pts` at the wall-clock instant `wall`, instead of at the arrival of its
	/// first frame.
	///
	/// For a source whose timestamps say when they happened (a program clock, a shared epoch), so
	/// two importers of one stream publish identical timestamps whenever each starts. Like a first
	/// frame, this places the catalog's clock if it is not yet fixed and offsets onto it otherwise.
	/// Refused once this timebase has an offset.
	pub fn place(&self, pts: moq_net::Timestamp, wall: std::time::SystemTime) -> crate::Result<()> {
		self.catalog.clone().anchor(&self.offset, pts, Some(wall))?;
		Ok(())
	}

	/// Anchor on the first frame's timestamp `pts`, returning the timebase's offset.
	///
	/// The first call from any clone fixes the offset; every later one returns it unchanged.
	pub(crate) fn anchor(&self, pts: moq_net::Timestamp) -> crate::Result<Offset> {
		match self.offset.get() {
			Some(offset) => Ok(*offset),
			None => self.catalog.clone().anchor(&self.offset, pts, None),
		}
	}

	/// The offset, once the timebase anchored.
	pub(crate) fn offset(&self) -> Option<Offset> {
		self.offset.get().copied()
	}

	/// Shift `pts` onto the catalog clock, anchoring on it if it is this timebase's first.
	pub(crate) fn shift(&self, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		self.anchor(pts)?.apply(pts)
	}
}

// Manual so a timebase is clonable regardless of whether `E` is.
impl<E: CatalogExt> Clone for Timebase<E> {
	fn clone(&self) -> Self {
		Self {
			catalog: self.catalog.clone(),
			offset: self.offset.clone(),
		}
	}
}

/// The signed shift from one timebase's timestamps onto the catalog clock, in microseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Offset(i64);

impl Offset {
	pub(super) fn from_micros(micros: i128) -> crate::Result<Self> {
		let micros = i64::try_from(micros).map_err(|_| moq_net::TimeOverflow)?;
		Ok(Self(micros))
	}

	/// The offset in units of `units_per_second`, rounded down so every track at one scale shifts
	/// by the same whole number of units.
	pub(crate) fn ticks(self, units_per_second: u64) -> i128 {
		(self.0 as i128 * units_per_second as i128).div_euclid(1_000_000)
	}

	/// Shift `pts`, keeping its scale. Refuses a result below zero rather than clamping it.
	pub(crate) fn apply(self, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		if self.0 == 0 {
			return Ok(pts);
		}
		let scale = pts.scale();
		let value = pts.value() as i128 + self.ticks(scale.as_u64());
		let value = u64::try_from(value).map_err(|_| {
			crate::Error::UnmappableTimestamp(format!(
				"{} µs shifted by {} µs lands before the broadcast clock's zero",
				pts.as_micros(),
				self.0
			))
		})?;
		Ok(moq_net::Timestamp::new(value, scale)?)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn us(micros: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(micros).unwrap()
	}

	#[test]
	fn an_offset_keeps_the_scale_and_refuses_below_zero() {
		let offset = Offset(-1_500_000);
		let pts = moq_net::Timestamp::from_scale(270_000, 90_000).unwrap();
		assert_eq!(
			offset.apply(pts).unwrap(),
			moq_net::Timestamp::from_scale(135_000, 90_000).unwrap()
		);
		assert_eq!(offset.apply(us(2_000_000)).unwrap(), us(500_000));
		assert!(matches!(
			offset.apply(us(1_000_000)),
			Err(crate::Error::UnmappableTimestamp(_))
		));
		assert_eq!(Offset::default().apply(us(7)).unwrap(), us(7));
	}

	/// A fractional unit rounds down at every scale, so tracks at one scale shift alike.
	#[test]
	fn ticks_round_down() {
		assert_eq!(Offset(15).ticks(90_000), 1);
		assert_eq!(Offset(-15).ticks(90_000), -2);
		assert_eq!(Offset(1_000_000).ticks(48_000), 48_000);
	}
}
