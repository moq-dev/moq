//! Container importers.
//!
//! [`Container`] decodes a container from whole chunks; [`ContainerStream`]
//! decodes it from a raw byte stream. A container may publish more than one MoQ
//! track, so neither exposes a single-track demand/name handle. Today every
//! container supports both; both wrap the same [`ContainerImpl`] dispatch.

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

	fn abort(&mut self, err: moq_net::Error) {
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.abort(err),
			ContainerImpl::Mkv(decoder) => decoder.abort(err),
			ContainerImpl::Ts(decoder) => decoder.abort(err),
			ContainerImpl::Flv(decoder) => decoder.abort(err),
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
	/// Create a new container importer, decoding the initial chunk.
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		reserved: crate::catalog::Reserved<E>,
		format: &str,
		init: &[u8],
	) -> Result<Self> {
		let mut inner = match format {
			"fmp4" | "cmaf" => ContainerImpl::fmp4(broadcast, reserved),
			"mkv" | "webm" | "matroska" => ContainerImpl::mkv(broadcast, reserved),
			"ts" | "mpegts" | "mpeg2ts" | "m2ts" => ContainerImpl::ts(broadcast, reserved),
			"flv" => ContainerImpl::flv(broadcast, reserved),
			_ => return Err(crate::Error::UnknownFormat(format.to_string())),
		};
		inner.decode(init)?;
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
	/// rather than [`moq_net::Error::Dropped`].
	pub fn abort(&mut self, err: moq_net::Error) {
		self.inner.abort(err)
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
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		reserved: crate::catalog::Reserved<E>,
		format: &str,
	) -> Result<Self> {
		// A separate list from [`Container::new`]: only containers that can be
		// recovered from a raw byte stream belong here. Today that's all of them,
		// but a non-streamable container (e.g. RTP) would be added to `Container`
		// alone.
		let inner = match format {
			"fmp4" | "cmaf" => ContainerImpl::fmp4(broadcast, reserved),
			"mkv" | "webm" | "matroska" => ContainerImpl::mkv(broadcast, reserved),
			"ts" | "mpegts" | "mpeg2ts" | "m2ts" => ContainerImpl::ts(broadcast, reserved),
			"flv" => ContainerImpl::flv(broadcast, reserved),
			_ => return Err(crate::Error::UnknownFormat(format.to_string())),
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
	/// rather than [`moq_net::Error::Dropped`].
	pub fn abort(&mut self, err: moq_net::Error) {
		self.inner.abort(err)
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.inner.seek(sequence)
	}
}
