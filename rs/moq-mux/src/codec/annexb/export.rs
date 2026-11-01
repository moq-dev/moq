use std::task::{Poll, ready};

use bytes::Bytes;
use hang::Catalog;
use hang::catalog::{VideoCodecKind, VideoConfig};

use super as annexb;
use crate::catalog::Stream;
use crate::container::ExportSource;

/// Shared single-rendition Annex-B lifecycle.
pub(crate) struct Export<S: Stream, C: Codec> {
	codec: std::marker::PhantomData<C>,
	source: crate::Source,
	catalog: Option<S>,
	max_delay: std::time::Duration,
	track: Option<Track>,
	/// The latest presentation time written. A switch to another rendition resumes at its first
	/// keyframe past it: a new subscription starts at the rendition's latest group, which for
	/// one re-enabled is the stale group it stopped on.
	playhead: Option<std::time::Duration>,
}

struct Track {
	name: String,
	/// Rebuild when the rendition changes its source or decoder configuration.
	config: VideoConfig,
	source: ExportSource,
	/// A decoder description supplies length-prefix size and any out-of-band parameter sets.
	/// Without one, the payload is already Annex-B.
	convert: Option<Convert>,
	/// Frames are dropped until a keyframe past the playhead.
	joining: bool,
}

pub(crate) struct Convert {
	pub(crate) length_size: usize,
	pub(crate) keyframe_prefix: Bytes,
}

pub(crate) trait Codec {
	const KIND: VideoCodecKind;
	fn convert(name: &str, config: &VideoConfig) -> crate::Result<Option<Convert>>;
}

impl<S: Stream, C: Codec> Export<S, C> {
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			codec: std::marker::PhantomData,
			source,
			catalog: Some(catalog),
			max_delay: std::time::Duration::ZERO,
			track: None,
			playhead: None,
		}
	}

	/// Set the max delay for the per-track source.
	///
	/// See [`Consumer`](crate::container::Consumer) for the per-track skip behavior.
	/// Defaults to
	/// [`std::time::Duration::ZERO`](std::time::Duration::ZERO) (skip aggressively).
	pub fn with_max_delay(mut self, max_delay: std::time::Duration) -> Self {
		self.max_delay = max_delay;
		self
	}

	/// Poll for the next Annex B access unit, or `None` once the export ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Bytes>>> {
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot.media())?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		loop {
			let Some(track) = self.track.as_mut() else {
				if self.catalog.is_none() {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			};

			match ready!(track.source.poll_read(waiter))? {
				Some(frame) => {
					let timestamp = std::time::Duration::from(frame.timestamp);
					if track.joining {
						if !frame.keyframe || self.playhead.is_some_and(|playhead| timestamp <= playhead) {
							continue;
						}
						track.joining = false;
					}
					self.playhead = self.playhead.max(Some(timestamp));
					let bytes = match &track.convert {
						None => frame.payload,
						Some(convert) => {
							let prefix = frame.keyframe.then(|| convert.keyframe_prefix.as_ref());
							annexb::from_length_prefixed(&frame.payload, convert.length_size, prefix)?
						}
					};
					if bytes.is_empty() {
						continue;
					}
					return Poll::Ready(Ok(Some(bytes)));
				}
				None => {
					// Keep the resume position while the catalog can still move this rendition.
					return if self.catalog.is_none() {
						Poll::Ready(Ok(None))
					} else {
						Poll::Pending
					};
				}
			}
		}
	}

	fn update_catalog(&mut self, catalog: &Catalog) -> crate::Result<()> {
		let mut catalog = catalog.clone();
		self.source.retain_selectable(&mut catalog);

		let picked = catalog
			.video
			.renditions
			.iter()
			.filter(|(_, c)| c.codec.kind() == C::KIND)
			.collect::<Vec<_>>();

		if picked.len() > 1 {
			tracing::warn!(
				count = picked.len(),
				codec = ?C::KIND,
				"multiple matching renditions in catalog snapshot; using the first by name. \
				 Narrow with catalog::Select to pick one explicitly."
			);
		}

		let Some((name, config)) = picked.into_iter().next() else {
			self.track = None;
			return Ok(());
		};

		if self
			.track
			.as_ref()
			.is_some_and(|t| t.name == *name && t.config == *config)
		{
			return Ok(());
		}

		let Some(source) = ExportSource::for_video_raw(&self.source, name, config, self.max_delay)? else {
			unreachable!("invalid broadcast references were removed above");
		};
		let convert = C::convert(name, config)?;
		let source = match self.track.as_ref().filter(|track| track.name == *name) {
			Some(previous) => source.continuing(&previous.source),
			None => source,
		};

		self.track = Some(Track {
			name: name.clone(),
			config: config.clone(),
			source,
			convert,
			joining: true,
		});

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use crate::catalog::Stream;
	use crate::codec::h264::Export;
	use bytes::Bytes;

	async fn next<S: Stream>(export: &mut Export<S>) -> Bytes {
		moq_clock::timeout(std::time::Duration::from_millis(1), export.next())
			.await
			.expect("a frame is ready")
			.unwrap()
			.expect("the export continues")
	}

	#[tokio::test(start_paused = true)]
	async fn follows_same_id_track_and_broadcast_moves() {
		use crate::container::test_util::{Moving, SPS, marked_frame, marks};
		for (track, broadcast) in [("b", None), ("a", Some("other"))] {
			let mut moving = Moving::new();
			let mut export = Export::new(moving.source().await, moving.catalog_stream().await);
			moving.a.write(marked_frame(0, 0xa1)).unwrap();
			assert_eq!(marks(&next(&mut export).await), [0xa1]);
			moving.point(track, broadcast, SPS);
			moving.a.write(marked_frame(30, 0xa2)).unwrap();
			let target = if broadcast.is_some() {
				&mut moving.other
			} else {
				&mut moving.b
			};
			target.write(marked_frame(30, 0xb1)).unwrap();
			assert_eq!(marks(&next(&mut export).await), [0xb1]);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn moved_rendition_joins_the_live_edge_with_a_delay_budget() {
		use crate::container::test_util::{Moving, SPS, marked_frame, marks};
		for finished in [false, true] {
			let mut moving = Moving::new();
			let mut export = Export::new(moving.source().await, moving.catalog_stream().await)
				.with_max_delay(std::time::Duration::from_secs(10));
			moving.a.write(marked_frame(0, 0xa1)).unwrap();
			assert_eq!(marks(&next(&mut export).await), [0xa1]);
			if finished {
				moving.a.finish().unwrap();
				assert!(export.poll_next(&kio::Waiter::noop()).is_pending());
			}
			moving.b.write(marked_frame(30, 0xb1)).unwrap();
			moving.b.write(marked_frame(60, 0xb2)).unwrap();
			moving.point("b", None, SPS);
			assert_eq!(marks(&next(&mut export).await), [0xb2]);
			moving.b.finish().unwrap();
			moving.catalog.finish().unwrap();
			assert!(export.next().await.unwrap().is_none());
		}
	}

	#[tokio::test(start_paused = true)]
	async fn moved_rendition_refuses_a_rewound_second_group() {
		use crate::container::test_util::{Moving, SPS, marked_frame};
		let mut moving = Moving::new();
		let mut export = Export::new(moving.source().await, moving.catalog_stream().await);
		moving.a.write(marked_frame(60, 0xa1)).unwrap();
		next(&mut export).await;
		moving.point("b", None, SPS);
		moving.b.write(marked_frame(0, 0xb1)).unwrap();
		assert!(
			moq_clock::timeout(std::time::Duration::from_millis(1), export.next())
				.await
				.is_err()
		);
		moving.b.write(marked_frame(30, 0xb2)).unwrap();
		assert!(matches!(
			moq_clock::timeout(std::time::Duration::from_millis(1), export.next())
				.await
				.unwrap(),
			Err(crate::Error::RenditionRewound { .. })
		));
	}
}
