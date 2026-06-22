//! Legacy broadcast audio (MP2, AC-3, E-AC-3) carried verbatim.
//!
//! These codecs share one model: every frame is whole and self-describing
//! (framing header included), published as one hang frame in its own group,
//! never decoded. Verbatim is byte-exact for complete, well-formed frames;
//! malformed or out-of-scope input is rejected, never mis-described. Each
//! codec contributes only a header parser and a [`Descriptor`]; this module
//! owns the track lifecycle.

use bytes::{Buf, BytesMut};

use crate::catalog::hang::CatalogExt;

/// A parsed legacy-audio frame header.
#[derive(Debug)]
pub(crate) struct Header {
	/// Whole-frame size in bytes (header included).
	pub len: usize,
	pub sample_rate: u32,
	pub channel_count: u32,
	/// Samples in this frame. Per-frame, not per-codec: E-AC-3 varies it
	/// (256 x numblks) while MP2/AC-3 keep it constant.
	pub samples: u64,
}

/// What distinguishes one legacy codec from another.
pub(crate) struct Descriptor {
	/// Track name suffix, e.g. ".mp2".
	pub track_suffix: &'static str,
	/// Catalog codec for the rendition.
	pub codec: hang::catalog::AudioCodec,
	/// Bytes needed to attempt a header parse.
	pub min_header_len: usize,
	/// Parse one frame header at the start of the slice.
	pub parse: fn(&[u8]) -> anyhow::Result<Header>,
}

/// Catalog config for a legacy audio track. Both fields come from the frame
/// header, never the TS stream_type.
pub(crate) struct Config {
	pub sample_rate: u32,
	pub channel_count: u32,
}

/// Legacy audio importer.
///
/// Publishes each whole frame as one hang frame in its own group, so the relay
/// forwards it immediately. The audio is never decoded; the catalog carries the
/// codec, sample rate and channel count read from the frame header.
pub(crate) struct Import<E: CatalogExt = ()> {
	catalog: crate::catalog::Producer<E>,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	zero: Option<tokio::time::Instant>,
}

impl<E: CatalogExt> Import<E> {
	pub fn new(
		descriptor: &'static Descriptor,
		mut broadcast: moq_net::BroadcastProducer,
		mut catalog: crate::catalog::Producer<E>,
		config: Config,
	) -> anyhow::Result<Self> {
		let track = broadcast.unique_track(descriptor.track_suffix)?;

		let mut audio_config =
			hang::catalog::AudioConfig::new(descriptor.codec.clone(), config.sample_rate, config.channel_count);
		audio_config.container = hang::catalog::Container::Legacy;
		// description stays None: legacy frames are self-describing and no in-repo
		// consumer needs out-of-band config (TS export self-describes; WebCodecs
		// cannot decode these codecs). Fill it only if a real consumer ever needs it.

		tracing::debug!(name = ?track.name, config = ?audio_config, "starting track");
		catalog.lock().audio.renditions.insert(track.name.clone(), audio_config);

		Ok(Self {
			catalog,
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			zero: None,
		})
	}

	/// The MoQ track name.
	pub fn name(&self) -> &str {
		&self.track.name
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Publish one whole frame as a hang frame in its own group.
	pub fn decode<T: Buf>(&mut self, buf: &mut T, pts: Option<crate::container::Timestamp>) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;

		let mut payload = BytesMut::with_capacity(buf.remaining());
		while buf.has_remaining() {
			let chunk = buf.chunk();
			payload.extend_from_slice(chunk);
			let len = chunk.len();
			buf.advance(len);
		}

		let frame = crate::container::Frame {
			timestamp: pts,
			payload: payload.freeze(),
			keyframe: true,
		};

		self.track.write(frame)?;
		self.track.finish_group()?;

		Ok(())
	}

	fn pts(&mut self, hint: Option<crate::container::Timestamp>) -> anyhow::Result<crate::container::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(crate::container::Timestamp::from_micros(
			zero.elapsed().as_micros() as u64
		)?)
	}
}

impl<E: CatalogExt> Drop for Import<E> {
	fn drop(&mut self) {
		tracing::debug!(name = ?self.track.name, "ending track");
		self.catalog.lock().audio.renditions.remove(&self.track.name);
	}
}
