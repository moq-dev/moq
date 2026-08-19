//! Container importers.
//!
//! [`Container`] decodes a container from whole chunks; [`ContainerStream`]
//! decodes it from a raw byte stream. A container may publish more than one MoQ
//! track, so neither exposes a single-track demand/name handle. Today every
//! container supports both; both wrap the same [`ContainerImpl`] dispatch.

use super::{ContainerFormat, ContainerInit};
use crate::Result;

/// The concrete container importers, shared by [`Container`] and
/// [`ContainerStream`]. Containers parse their own internal framing, so a whole
/// chunk and a stream chunk decode identically.
enum ContainerImpl<E: crate::container::ts::Catalog = ()> {
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import<E>>),
	Mkv(Box<crate::container::mkv::Import<E>>),
	Ts(Box<crate::container::ts::Import<E>>),
	Flv(Box<crate::container::flv::Import<E>>),
}

impl<E: crate::container::ts::Catalog> ContainerImpl<E> {
	fn fmp4(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		ContainerImpl::Fmp4(Box::new(crate::container::fmp4::Import::new(broadcast, reserved)))
	}

	fn mkv(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		ContainerImpl::Mkv(Box::new(crate::container::mkv::Import::new(broadcast, reserved)))
	}

	fn ts(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		ContainerImpl::Ts(Box::new(crate::container::ts::Import::new(broadcast, reserved)))
	}

	fn flv(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		ContainerImpl::Flv(Box::new(crate::container::flv::Import::new(broadcast, reserved)))
	}

	fn decode(&mut self, data: &[u8]) -> Result<()> {
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.decode(data),
			ContainerImpl::Mkv(decoder) => decoder.decode(data),
			ContainerImpl::Ts(decoder) => decoder.decode(data).map_err(Into::into),
			ContainerImpl::Flv(decoder) => decoder.decode(data),
		}
	}

	fn finish(&mut self) -> Result<()> {
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.finish(),
			ContainerImpl::Mkv(decoder) => decoder.finish(),
			ContainerImpl::Ts(decoder) => decoder.finish().map_err(Into::into),
			ContainerImpl::Flv(decoder) => decoder.finish(),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.abort(err),
			ContainerImpl::Mkv(decoder) => decoder.abort(err),
			ContainerImpl::Ts(decoder) => decoder.abort(err),
			ContainerImpl::Flv(decoder) => decoder.abort(err),
		}
	}

	fn cut(&mut self) {
		// Only fMP4 has a segment concept to declare. The others recover their own framing and
		// group on what they find in it, so there is nothing for a caller to draw.
		if let ContainerImpl::Fmp4(decoder) = self {
			decoder.cut();
		}
	}

	fn seek(&mut self, sequence: u64) -> Result<()> {
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.seek(sequence),
			ContainerImpl::Mkv(decoder) => decoder.seek(sequence),
			ContainerImpl::Ts(decoder) => decoder.seek(sequence).map_err(Into::into),
			ContainerImpl::Flv(decoder) => decoder.seek(sequence),
		}
	}
}

/// A container importer for whole chunks.
///
/// Use this when the caller hands over discrete buffers (the typical case for
/// files and reassembled network input). May publish more than one track.
pub struct Container<E: crate::container::ts::Catalog = ()> {
	inner: ContainerImpl<E>,
}

impl<E: crate::container::ts::Catalog> Container<E> {
	/// Create a new container importer, decoding [`ContainerInit::data`] as the initial chunk.
	///
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		reserved: crate::catalog::Reserved<E>,
		init: &ContainerInit,
	) -> Result<Self> {
		let mut inner = match init.format {
			ContainerFormat::Fmp4 => ContainerImpl::fmp4(broadcast, reserved),
			ContainerFormat::Mkv => ContainerImpl::mkv(broadcast, reserved),
			ContainerFormat::Ts => ContainerImpl::ts(broadcast, reserved),
			ContainerFormat::Flv => ContainerImpl::flv(broadcast, reserved),
		};
		inner.decode(&init.data)?;
		Ok(Self { inner })
	}

	/// Decode a chunk of container bytes.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.inner.decode(data)
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.finish()
	}

	/// Abort every published track with `err`, so subscribers see the real cause
	/// rather than [`moq_net::Error::Dropped`]. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		self.inner.abort(err)
	}

	/// Declare that the next fragment starts a new segment, rolling a group on every track.
	///
	/// For a caller that knows the source's segmentation out of band (an HLS import following its
	/// playlist). An fMP4 source that carries `styp` atoms declares its own, so this is only
	/// needed when it doesn't. Ignored by container formats with no segment concept (MKV, TS, FLV).
	pub fn cut(&mut self) {
		self.inner.cut()
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.inner.seek(sequence)
	}
}

/// A container importer for a raw byte stream.
///
/// Use this when the caller pushes arbitrary byte chunks and the container
/// recovers its own framing. May publish more than one track.
pub struct ContainerStream<E: crate::container::ts::Catalog = ()> {
	inner: ContainerImpl<E>,
}

impl<E: crate::container::ts::Catalog> ContainerStream<E> {
	/// Create a new container stream importer.
	///
	/// Takes a bare format rather than a [`ContainerInit`]: a stream recovers its own framing, so
	/// there are no leading bytes to seed it with. Push everything through [`Self::decode`].
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		reserved: crate::catalog::Reserved<E>,
		format: ContainerFormat,
	) -> Result<Self> {
		// A separate list from [`Container::new`]: only containers that can be
		// recovered from a raw byte stream belong here. Today that's all of them,
		// but a non-streamable container (e.g. RTP) would be added to `Container`
		// alone.
		let inner = match format {
			ContainerFormat::Fmp4 => ContainerImpl::fmp4(broadcast, reserved),
			ContainerFormat::Mkv => ContainerImpl::mkv(broadcast, reserved),
			ContainerFormat::Ts => ContainerImpl::ts(broadcast, reserved),
			ContainerFormat::Flv => ContainerImpl::flv(broadcast, reserved),
		};
		Ok(Self { inner })
	}

	/// Decode a chunk of the byte stream.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.inner.decode(data)
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.finish()
	}

	/// Abort every published track with `err`, so subscribers see the real cause
	/// rather than [`moq_net::Error::Dropped`]. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		self.inner.abort(err)
	}

	/// Declare that the next fragment starts a new segment, rolling a group on every track.
	///
	/// For a caller that knows the source's segmentation out of band (an HLS import following its
	/// playlist). An fMP4 source that carries `styp` atoms declares its own, so this is only
	/// needed when it doesn't. Ignored by container formats with no segment concept (MKV, TS, FLV).
	pub fn cut(&mut self) {
		self.inner.cut()
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.inner.seek(sequence)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A stream recovers its own framing, which is why [`ContainerStream::new`] takes a bare format
	/// with no leading bytes to seed it. Prove it: hand the whole file to `decode` in two chunks
	/// split mid-header, and the tracks still land.
	#[test]
	fn a_split_stream_publishes_its_tracks() {
		let data = include_bytes!("../container/fmp4/test_data/bbb.mp4");
		let (head, tail) = data.split_at(100);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

		let mut stream: ContainerStream =
			ContainerStream::new(broadcast, catalog.reserve(), ContainerFormat::Fmp4).unwrap();

		stream.decode(head).unwrap();
		// The test file ends on a malformed fragment, so a trailing decode error is expected.
		let _ = stream.decode(tail);

		let snapshot = catalog.snapshot();
		assert_eq!(
			snapshot.video.renditions.len(),
			1,
			"video rendition missing from a split stream"
		);
		assert_eq!(
			snapshot.audio.renditions.len(),
			1,
			"audio rendition missing from a split stream"
		);
	}
}
