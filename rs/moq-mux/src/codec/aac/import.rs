use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// AAC importer.
///
/// Initialized from an AudioSpecificConfig blob (variable-length, typically extracted from
/// an MP4 ESDS atom), so its catalog is known up front. By default each packet is published as
/// one hang frame in its own group (every AAC frame is independently decodable), so the relay can
/// forward each frame without waiting for a group boundary. A publisher that drives its own group
/// boundaries (e.g. aligning audio groups to a media-segment cadence) can call
/// [`with_keyframe_grouping(false)`](Self::with_keyframe_grouping) and mark boundaries via
/// [`seek`](Self::seek); frames then accumulate into the current group instead of one per packet.
/// Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Producer`](crate::catalog::Producer) it publishes its rendition into.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
	/// When true, the caller drives group boundaries (via [`seek`](Self::seek)) and
	/// [`decode`](Self::decode) does not close a group per packet. Default false = one group/packet.
	manual_groups: bool,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, registering the rendition in `catalog`.
	pub fn new(
		track: moq_net::TrackProducer,
		catalog: crate::catalog::Producer<E>,
		config: Config,
	) -> crate::Result<Self> {
		let mut audio_config = hang::catalog::AudioConfig::new(
			hang::catalog::AAC {
				profile: config.profile,
			},
			config.sample_rate,
			config.channel_count,
		);
		audio_config.container = hang::catalog::Container::Legacy;
		audio_config.description = Some(config.encode());

		tracing::debug!(name = ?track.name(), config = ?audio_config, "starting track");

		let mut rendition = catalog.audio_track(track.name());
		rendition.set(audio_config);

		Ok(Self {
			track: catalog.media_producer(track, crate::catalog::hang::Container::Legacy),
			rendition,
			manual_groups: false,
		})
	}

	/// Opt into caller-driven group boundaries.
	///
	/// Default (`true`, the parameter passed as `enabled`) keeps one group per packet. Pass
	/// `false` when the publisher marks its own boundaries via [`seek`](Self::seek) (e.g. aligning
	/// audio groups to a media-segment cadence): keyframes no longer roll a new group per packet,
	/// so frames accumulate into the current group until the next explicit boundary.
	pub fn with_keyframe_grouping(mut self, enabled: bool) -> Self {
		self.track.set_keyframe_grouping(enabled);
		self.manual_groups = !enabled;
		self
	}

	/// Post-construction form of [`with_keyframe_grouping`](Self::with_keyframe_grouping), for
	/// callers that build the importer before knowing the policy (e.g. a C-ABI toggle).
	pub fn set_keyframe_grouping(&mut self, enabled: bool) {
		self.track.set_keyframe_grouping(enabled);
		self.manual_groups = !enabled;
	}

	/// The MoQ track name this importer publishes on.
	pub fn name(&self) -> &str {
		self.track.name()
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		self.track.track().demand()
	}

	/// Refine the single audio rendition in place, republishing the catalog.
	///
	/// The TS importer uses this to set the synthesized `description` and an
	/// audio-burst `jitter` once it knows them.
	pub(crate) fn update_rendition(&mut self, f: impl FnOnce(&mut hang::catalog::AudioConfig)) {
		self.rendition.update(f);
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Cut the current group at `end` without finishing the track; publishing resumes on
	/// the next keyframe. See `import::Track::cut` for the full contract.
	pub fn cut(&mut self, end: Option<crate::container::Timestamp>) -> crate::Result<()> {
		self.track.cut(end)?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Publish one AAC packet, stamping `pts` or a wall clock when absent.
	///
	/// By default the packet is published as its own group (closed immediately). If the importer
	/// was built with [`with_keyframe_grouping(false)`](Self::with_keyframe_grouping), the frame
	/// instead accumulates into the current group and the caller closes groups via
	/// [`seek`](Self::seek).
	pub fn decode(&mut self, frame: &[u8], pts: Option<crate::container::Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		self.track.write(Frame {
			timestamp,
			payload: bytes::Bytes::copy_from_slice(frame),
			keyframe: true,
			duration: None,
		})?;
		if !self.manual_groups {
			// One group per packet: close it immediately so the relay forwards it without
			// waiting for the next frame. In manual mode the caller bounds groups via `seek`.
			self.track.cut(None)?;
		}
		Ok(())
	}
}
