//! H.265 single-rendition Annex-B exporter.
//!
//! HEVC analogue of [`crate::codec::h264::Export`]. Accepts either a hev1
//! (Annex-B, parameter sets inline) or hvc1 (length-prefixed + out-of-band
//! hvcC) source and emits a raw Annex-B elementary stream. Timestamps are
//! dropped.

use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::Catalog;
use hang::catalog::{VideoCodecKind, VideoConfig};

use crate::catalog::Stream;
use crate::codec::annexb;
use crate::container::ExportSource;

/// Single-rendition H.265 Annex-B exporter.
pub struct Export<S: Stream> {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<S>,
	latency: Duration,
	track: Option<H265Track>,
}

struct H265Track {
	name: String,
	/// Snapshot of the catalog config we built `source` from. Cached so that
	/// a catalog update which keeps the same rendition name but changes the
	/// codec config (e.g. a new hvcC) triggers a full rebuild instead of
	/// silently reusing a stale `convert`.
	config: VideoConfig,
	source: ExportSource,
	/// `Some` for an hvc1 source: VPS/SPS/PPS prefix prebuilt from the hvcC,
	/// and the hvcC length-prefix size. `None` for a hev1 source: Annex-B
	/// passes through without conversion.
	convert: Option<Hvc1Convert>,
}

struct Hvc1Convert {
	length_size: usize,
	keyframe_prefix: Bytes,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `broadcast` and emit an Annex-B H.265 byte stream.
	///
	/// `catalog` is expected to be narrowed to a single H.265 rendition. If
	/// multiple H.265 renditions appear in a snapshot, the first by BTreeMap
	/// order wins and a warning is logged.
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: S) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			track: None,
		}
	}

	/// Set the maximum buffering latency for the per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	pub async fn next(&mut self) -> crate::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

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

			match track.source.poll_read(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
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
				Poll::Ready(Ok(None)) => {
					self.track = None;
					continue;
				}
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			}
		}
	}

	fn update_catalog(&mut self, catalog: &Catalog) -> crate::Result<()> {
		let picked = catalog
			.video
			.renditions
			.iter()
			.filter(|(_, c)| c.codec.kind() == VideoCodecKind::H265)
			.collect::<Vec<_>>();

		if picked.len() > 1 {
			tracing::warn!(
				count = picked.len(),
				"multiple H.265 renditions in catalog snapshot; using the first by name. \
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

		let source = ExportSource::for_video_raw(&self.broadcast, name, config, self.latency)?;
		let convert = match config.description.as_ref().filter(|d| !d.is_empty()) {
			None => None,
			Some(hvcc) => {
				let params = super::Hvcc::parse(hvcc)?;
				if params.vps.is_empty() || params.sps.is_empty() || params.pps.is_empty() {
					return Err(super::Error::MissingParamSets {
						name: name.clone(),
						vps: params.vps.len(),
						sps: params.sps.len(),
						pps: params.pps.len(),
					}
					.into());
				}
				let prefix = annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
				Some(Hvc1Convert {
					length_size: params.length_size,
					keyframe_prefix: prefix,
				})
			}
		};

		self.track = Some(H265Track {
			name: name.clone(),
			config: config.clone(),
			source,
			convert,
		});

		Ok(())
	}
}
