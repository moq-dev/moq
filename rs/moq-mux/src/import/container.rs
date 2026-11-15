//! The container importer.
//!
//! [`Container`] decodes a container from chunks split anywhere: every container
//! recovers its own framing. A container may publish more than one MoQ track, so
//! it exposes no single-track demand/name handle.

use super::{ContainerFormat, ContainerInit};
use crate::Result;

/// The concrete container importers [`Container`] dispatches to.
enum ContainerImpl<E: crate::container::ts::Catalog = ()> {
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import<E>>),
	Mkv(Box<crate::container::mkv::Import<E>>),
	Ts(Box<crate::container::ts::Import<E>>),
	Flv(Box<crate::container::flv::Import<E>>),
}

impl<E: crate::container::ts::Catalog> ContainerImpl<E> {
	fn new(
		broadcast: moq_net::broadcast::Producer,
		reserved: crate::catalog::Reserved<E>,
		format: ContainerFormat,
	) -> Self {
		match format {
			ContainerFormat::Fmp4 => Self::fmp4(broadcast, reserved),
			ContainerFormat::Mkv => Self::mkv(broadcast, reserved),
			ContainerFormat::Ts => Self::ts(broadcast, reserved),
			ContainerFormat::Flv => Self::flv(broadcast, reserved),
		}
	}

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

	fn finish(self) -> Result<FinishedImpl<E>> {
		Ok(match self {
			ContainerImpl::Fmp4(decoder) => FinishedImpl::Fmp4(Box::new(decoder.finish()?)),
			ContainerImpl::Mkv(decoder) => FinishedImpl::Mkv(Box::new(decoder.finish()?)),
			ContainerImpl::Ts(decoder) => FinishedImpl::Ts(Box::new(decoder.finish()?)),
			ContainerImpl::Flv(decoder) => FinishedImpl::Flv(Box::new(decoder.finish()?)),
		})
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.abort(err),
			ContainerImpl::Mkv(decoder) => decoder.abort(err),
			ContainerImpl::Ts(decoder) => decoder.abort(err),
			ContainerImpl::Flv(decoder) => decoder.abort(err),
		}
	}

	fn cut(&mut self) -> Result<()> {
		// Only fMP4 has a segment concept to declare. The others recover their own framing and
		// group on what they find in it, so there is nothing for a caller to draw.
		match self {
			ContainerImpl::Fmp4(decoder) => decoder.cut(),
			_ => Ok(()),
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

/// A container importer, fed chunks split anywhere. May publish more than one track.
pub struct Container<E: crate::container::ts::Catalog = ()> {
	inner: ContainerImpl<E>,
}

impl<E: crate::container::ts::Catalog> Container<E> {
	/// Create a new container importer, decoding [`ContainerInit::data`] as the initial chunk.
	///
	/// The data may be empty, leaving every byte to [`Self::decode`].
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		reserved: crate::catalog::Reserved<E>,
		init: &ContainerInit,
	) -> Result<Self> {
		let mut inner = ContainerImpl::new(broadcast, reserved, init.format);
		inner.decode(&init.data)?;
		Ok(Self { inner })
	}

	/// Decode a chunk of container bytes.
	///
	/// An fMP4 or MKV decode error is terminal: replace the importer before feeding more input.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.inner.decode(data)
	}

	/// Finish the importer, flushing any buffered data, and return the [`Finished`]
	/// importer.
	///
	/// Every track ends either way, so a failed finish can't be retried.
	pub fn finish(self) -> Result<Finished<E>> {
		Ok(Finished {
			inner: self.inner.finish()?,
		})
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
	pub fn cut(&mut self) -> Result<()> {
		self.inner.cut()
	}

	/// Close the current group and open the next one at `start`.
	pub fn seek(&mut self, start: u64) -> Result<()> {
		self.inner.seek(start)
	}
}

/// The finished importers [`Finished`] dispatches to.
enum FinishedImpl<E: crate::container::ts::Catalog = ()> {
	// Boxed like the importers they finished.
	Fmp4(Box<crate::container::fmp4::Finished<E>>),
	Mkv(Box<crate::container::mkv::Finished<E>>),
	Ts(Box<crate::container::ts::Finished<E>>),
	Flv(Box<crate::container::flv::Finished<E>>),
}

/// A container importer that [finished](Container::finish): every track has ended, and they can
/// still be read or aborted, but it takes no more input.
///
/// Dropping it retires the tracks and their catalog renditions, as dropping the [`Container`]
/// does.
///
/// ```compile_fail,E0599
/// # fn cannot_decode(mut container: moq_mux::import::container::Finished) {
/// container.decode(b"").unwrap();
/// # }
/// ```
#[must_use = "dropping it retires the tracks and their catalog renditions"]
pub struct Finished<E: crate::container::ts::Catalog = ()> {
	inner: FinishedImpl<E>,
}

impl<E: crate::container::ts::Catalog> Finished<E> {
	/// Release every track's cache, reporting `err` to readers that have not drained it.
	pub fn abort(self, err: moq_net::Error) {
		match self.inner {
			FinishedImpl::Fmp4(finished) => finished.abort(err),
			FinishedImpl::Mkv(finished) => finished.abort(err),
			FinishedImpl::Ts(finished) => finished.abort(err),
			FinishedImpl::Flv(finished) => finished.abort(err),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A container recovers its own framing, so it needs no leading bytes to seed it. Prove it:
	/// start empty, hand the whole file to `decode` in two chunks split mid-header, and the tracks
	/// still land.
	#[test]
	fn a_split_stream_publishes_its_tracks() {
		let data = include_bytes!("../container/fmp4/test_data/bbb.mp4");
		let (head, tail) = data.split_at(100);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

		let init = ContainerInit::new(ContainerFormat::Fmp4, bytes::Bytes::new());
		let mut stream: Container = Container::new(broadcast, catalog.reserve(), &init).unwrap();

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
