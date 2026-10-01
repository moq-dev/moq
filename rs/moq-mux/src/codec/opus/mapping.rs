//! The OpusHead channel mapping table (RFC 7845 §5.1.1).

use bytes::Buf;

use super::{Error, Result};

/// An OpusHead channel mapping table (RFC 7845 §5.1.1), present for every family but 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
	family: u8,
	streams: u8,
	coupled: u8,
	channels: u8,
	// Sized for the most channels any family allows, so `opus::Config` stays `Copy`.
	table: [u8; 255],
}

/// The fields of a channel mapping table, checked into a [`Mapping`] by [`Mapping::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config<'a> {
	/// The channel mapping family; 0 has no table and is refused.
	pub family: u8,
	/// Number of Opus streams in each packet.
	pub streams: u8,
	/// How many of those streams are coupled (stereo); they come first.
	pub coupled: u8,
	/// The decoded channel feeding each output channel, 255 for silence.
	pub table: &'a [u8],
}

/// The family 1 stream counts and table for 1 to 8 channels, as libopus and
/// RFC 7845 §5.1.1.2 lay out the Vorbis channel orders.
const VORBIS: [(u8, u8, &[u8]); 8] = [
	(1, 0, &[0]),
	(1, 1, &[0, 1]),
	(2, 1, &[0, 2, 1]),
	(2, 2, &[0, 1, 2, 3]),
	(3, 2, &[0, 4, 1, 2, 3]),
	(4, 2, &[0, 4, 1, 2, 3, 5]),
	(4, 3, &[0, 4, 1, 2, 3, 5, 6]),
	(5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]),
];

impl Mapping {
	/// The family 1 mapping a surround encoder uses for `channels` in Vorbis
	/// order, for sources that name only a channel count.
	pub(crate) fn vorbis(channels: u8) -> Result<Self> {
		let (streams, coupled, table) = *channels
			.checked_sub(1)
			.and_then(|index| VORBIS.get(index as usize))
			.ok_or(Error::UnsupportedChannelCount(channels as u32))?;
		Self::new(Config {
			family: 1,
			streams,
			coupled,
			table,
		})
	}

	/// Check `config` into a mapping with one output channel per table entry.
	///
	/// Refuses family 0, a channel count the family does not allow, and a table
	/// that names no streams, more coupled streams than streams, or a channel
	/// past the decoded ones.
	pub fn new(config: Config<'_>) -> Result<Self> {
		let Config {
			family,
			streams,
			coupled,
			table,
		} = config;
		let max_channels = match family {
			0 => return Err(Error::UnsupportedMappingFamily(0)),
			1 => 8,
			_ => 255,
		};
		if table.is_empty() || table.len() > max_channels {
			return Err(Error::UnsupportedChannelCount(table.len() as u32));
		}

		let decoded = streams as u32 + coupled as u32;
		if streams == 0 || coupled > streams || decoded > 255 {
			return Err(Error::InvalidMappingTable);
		}
		if table.iter().any(|&entry| entry != 255 && entry as u32 >= decoded) {
			return Err(Error::InvalidMappingTable);
		}

		let mut padded = [0u8; 255];
		padded[..table.len()].copy_from_slice(table);
		Ok(Self {
			family,
			streams,
			coupled,
			channels: table.len() as u8,
			table: padded,
		})
	}

	pub(super) fn parse<T: Buf>(buf: &mut T, family: u8, channels: u8) -> Result<Self> {
		if buf.remaining() < 2 + channels as usize {
			return Err(Error::MappingTableTooShort);
		}
		let streams = buf.get_u8();
		let coupled = buf.get_u8();
		let mut table = [0u8; 255];
		buf.copy_to_slice(&mut table[..channels as usize]);
		Self::new(Config {
			family,
			streams,
			coupled,
			table: &table[..channels as usize],
		})
	}

	/// The channel mapping family: 1 is the Vorbis speaker order, 2 and 3 are
	/// ambisonics, and 255 is channels with no declared position.
	pub fn family(&self) -> u8 {
		self.family
	}

	/// Number of Opus streams in each packet.
	pub fn streams(&self) -> u8 {
		self.streams
	}

	/// How many of those streams are coupled (stereo); they come first.
	pub fn coupled(&self) -> u8 {
		self.coupled
	}

	/// The decoded channel feeding each output channel, 255 for silence.
	pub fn table(&self) -> &[u8] {
		&self.table[..self.channels as usize]
	}
}

impl std::fmt::Debug for Mapping {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Mapping")
			.field("family", &self.family)
			.field("streams", &self.streams)
			.field("coupled", &self.coupled)
			.field("table", &self.table())
			.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn new_validates_the_table() {
		fn new(family: u8, streams: u8, coupled: u8, table: &[u8]) -> Result<Mapping> {
			Mapping::new(Config {
				family,
				streams,
				coupled,
				table,
			})
		}

		let mapping = new(255, 2, 0, &[0, 1, 255]).unwrap();
		assert_eq!(mapping.family(), 255);
		assert_eq!((mapping.streams(), mapping.coupled()), (2, 0));
		assert_eq!(mapping.table(), &[0, 1, 255]);

		assert!(matches!(new(0, 1, 1, &[0, 1]), Err(Error::UnsupportedMappingFamily(0))));
		assert!(matches!(new(1, 1, 0, &[]), Err(Error::UnsupportedChannelCount(0))));
		assert!(matches!(new(1, 9, 0, &[0; 9]), Err(Error::UnsupportedChannelCount(9))));
		for (streams, coupled, table) in [(0, 0, &[0][..]), (1, 2, &[0]), (1, 1, &[2]), (200, 100, &[0])] {
			assert!(
				matches!(new(1, streams, coupled, table), Err(Error::InvalidMappingTable)),
				"{streams} streams, {coupled} coupled, {table:?}"
			);
		}
	}
}
