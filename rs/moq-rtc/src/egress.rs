//! Per-broadcast egress source for the RTP-out paths.
//!
//! Counterpart to [`crate::ingest::IngestSink`]. Holds a
//! [`moq_net::BroadcastConsumer`] and a cached catalog snapshot; on each
//! `MediaAdded` event the session loop calls [`EgressSource::on_track`]
//! which picks a matching rendition, subscribes to it, and spawns a pump
//! task that feeds RTP-ready frames back to the session loop via an mpsc
//! channel.
//!
//! Used by `server subscribe` (WHEP server) and `client publish` (WHIP
//! client). SDP negotiation lives in the matching modules; this file is
//! transport-agnostic.

use std::time::{Duration, Instant};

use bytes::Bytes;
use hang::catalog::{AudioCodec, VideoCodec};
use moq_mux::catalog::hang::Catalog;
use str0m::format::Codec;
use str0m::media::{Frequency, MediaTime, Mid, Pt};
use tokio::sync::mpsc;

use crate::{Error, Result, codec};

/// One frame waiting to be written into str0m's [`Writer`](str0m::media::Writer).
///
/// Pump tasks build these and send them down the channel; the session loop
/// receives them and calls `rtc.writer(mid).write(pt, wallclock, time, payload)`.
pub struct WriteRequest {
	/// Negotiated media line to write to.
	pub mid: Mid,
	/// Negotiated RTP payload type.
	pub pt: Pt,
	/// Presentation timestamp in the negotiated RTP clock domain.
	pub time: MediaTime,
	/// Complete encoded media frame.
	pub payload: Bytes,
}

/// Maps the shared MoQ presentation timeline to str0m's wallclock.
///
/// The first observed frame supplies an initial epoch. Later frames can prove
/// that epoch too recent when buffered media arrives faster than real time. In
/// that case the anchor moves earlier so no observed frame maps into the future.
/// Arrival delays never move it later, so dequeue jitter cannot become a
/// permanent difference between audio and video sender reports.
#[derive(Default)]
pub(crate) struct EgressClock {
	anchor: Option<(Duration, Instant)>,
}

impl EgressClock {
	/// Return the production wallclock corresponding to a presentation timestamp.
	pub(crate) fn wallclock(&mut self, time: MediaTime, now: Instant) -> Instant {
		let presentation = Duration::from(time);
		let Some((anchor_presentation, anchor_wallclock)) = self.anchor else {
			self.anchor = Some((presentation, now));
			return now;
		};

		if presentation >= anchor_presentation {
			let delta = presentation - anchor_presentation;
			let Some(mapped) = anchor_wallclock.checked_add(delta) else {
				self.anchor = Some((presentation, now));
				return now;
			};
			if mapped > now {
				// A catch-up burst revealed that the previous anchor was too recent.
				// Tighten it to the newest constraint. This anchor is shared by every
				// track, so equal presentation times map to equal wallclocks.
				self.anchor = Some((presentation, now));
				now
			} else {
				mapped
			}
		} else {
			anchor_wallclock
				.checked_sub(anchor_presentation - presentation)
				.unwrap_or(now)
		}
	}
}

/// Holds the broadcast + catalog and spawns per-rendition pump tasks.
pub struct EgressSource {
	broadcast: moq_net::BroadcastConsumer,
	/// Snapshot of the catalog at session start. Sufficient for v1: SDP
	/// negotiation happens once and the codec list is fixed for the
	/// lifetime of the session.
	catalog: Catalog,
	writes_tx: mpsc::Sender<WriteRequest>,
	writes_rx: Option<mpsc::Receiver<WriteRequest>>,
}

impl EgressSource {
	/// Subscribe to the broadcast's catalog and wait for the first snapshot.
	///
	/// The session loop drives the pumps via the returned channel; the
	/// caller hands `EgressSource` to [`Session::egress`](crate::session::Session::egress)
	/// which takes the receiver via [`Self::take_writes`].
	pub async fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self> {
		let catalog_track = broadcast.subscribe_track(&moq_net::Track::new(hang::Catalog::DEFAULT_NAME))?;
		let mut consumer = moq_mux::catalog::hang::Consumer::new(catalog_track);
		let catalog = consumer
			.next()
			.await
			.map_err(|err| Error::Other(anyhow::anyhow!("catalog subscribe: {err}")))?
			.ok_or_else(|| Error::Other(anyhow::anyhow!("catalog closed before first snapshot")))?;

		let (tx, rx) = mpsc::channel(64);
		Ok(Self {
			broadcast,
			catalog,
			writes_tx: tx,
			writes_rx: Some(rx),
		})
	}

	/// One-shot extractor for the write-request receiver. The session loop
	/// awaits on this to forward frames into str0m.
	pub fn take_writes(&mut self) -> mpsc::Receiver<WriteRequest> {
		self.writes_rx.take().expect("EgressSource writes_rx already taken")
	}

	/// Spawn a pump task for a newly added (sendonly) media line.
	///
	/// `mid` and `pt` come from str0m's negotiated state; `clock_rate` is
	/// the codec's negotiated RTP clock. The pump subscribes to a matching
	/// catalog rendition and forwards every frame as a [`WriteRequest`].
	pub fn on_track(&mut self, mid: Mid, codec: Codec, pt: Pt, clock_rate: Frequency) -> Result<()> {
		// the `subscribe` call blocks on SUBSCRIBE_OK, so pick + subscribe inside
		// the pump task to keep this str0m callback non-blocking.
		let tx = self.writes_tx.clone();
		let broadcast = self.broadcast.clone();
		let catalog = self.catalog.clone();
		tokio::spawn(async move {
			let track = match pick_track(&broadcast, &catalog, codec).await {
				Ok(Some(t)) => t,
				Ok(None) => {
					tracing::warn!(?codec, "no matching catalog rendition; egress track ignored");
					return;
				}
				Err(err) => {
					tracing::warn!(?codec, %err, "egress track subscribe failed");
					return;
				}
			};
			pump(mid, pt, clock_rate, track, tx).await;
		});
		Ok(())
	}

	/// Codecs present in the catalog, used by the SDP-offer side
	/// (`client publish`) to declare what we have. For v1: the union of
	/// audio + video codecs across all renditions.
	pub fn catalog_codecs(&self) -> Vec<Codec> {
		let mut out = Vec::new();
		if self
			.catalog
			.audio
			.renditions
			.values()
			.any(|r| matches!(r.codec, AudioCodec::Opus))
		{
			out.push(Codec::Opus);
		}
		for rendition in self.catalog.video.renditions.values() {
			if let Some(c) = video_codec(&rendition.codec)
				&& !out.contains(&c)
			{
				out.push(c);
			}
		}
		out
	}
}

/// Map a hang catalog video codec to the str0m codec we can egress, if any.
fn video_codec(codec: &VideoCodec) -> Option<Codec> {
	match codec {
		VideoCodec::H264(_) => Some(Codec::H264),
		VideoCodec::H265(_) => Some(Codec::H265),
		VideoCodec::VP8 => Some(Codec::Vp8),
		VideoCodec::VP9(_) => Some(Codec::Vp9),
		VideoCodec::AV1(_) => Some(Codec::Av1),
		_ => None,
	}
}

/// Find the first catalog rendition for the given codec and build a
/// [`codec::Track`] subscribed to it. Returns `None` if no rendition matches.
async fn pick_track(
	broadcast: &moq_net::BroadcastConsumer,
	catalog: &Catalog,
	codec: Codec,
) -> Result<Option<codec::Track>> {
	match codec {
		Codec::Opus => {
			let Some((name, _config)) = catalog
				.audio
				.renditions
				.iter()
				.find(|(_, c)| matches!(c.codec, AudioCodec::Opus))
			else {
				return Ok(None);
			};
			Ok(Some(codec::Track::opus(broadcast, name).await?))
		}
		Codec::H264 | Codec::H265 | Codec::Vp8 | Codec::Vp9 | Codec::Av1 => {
			let Some((name, config)) = catalog
				.video
				.renditions
				.iter()
				.find(|(_, c)| video_codec(&c.codec) == Some(codec))
			else {
				return Ok(None);
			};
			Ok(Some(codec::Track::video(broadcast, name, config).await?))
		}
		other => Err(Error::UnsupportedCodec(format!("{other:?}"))),
	}
}

/// Per-rendition pump task. Reads frames, converts the timestamp into the
/// codec's clock domain, and forwards as a [`WriteRequest`].
async fn pump(mid: Mid, pt: Pt, clock_rate: Frequency, mut track: codec::Track, tx: mpsc::Sender<WriteRequest>) {
	loop {
		let frame = match track.next().await {
			Ok(Some(f)) => f,
			Ok(None) => {
				tracing::debug!(?mid, "egress track ended");
				return;
			}
			Err(err) => {
				tracing::warn!(?mid, %err, "egress track error");
				return;
			}
		};
		let ticks = us_to_ticks(frame.timestamp_us, clock_rate);
		let time = MediaTime::new(ticks, clock_rate);
		let req = WriteRequest {
			mid,
			pt,
			time,
			payload: frame.payload,
		};
		if tx.send(req).await.is_err() {
			// Session closed; drop the pump.
			return;
		}
	}
}

/// Convert a microsecond timestamp to a tick count at the given clock rate.
/// Uses u128 internally to avoid overflow at high tick rates.
fn us_to_ticks(timestamp_us: u64, clock_rate: Frequency) -> u64 {
	let rate = clock_rate.get() as u128;
	((timestamp_us as u128 * rate) / 1_000_000) as u64
}

/// Write one `WriteRequest` into str0m.
///
/// Lives here (not in session.rs) so the egress data shape is colocated
/// with the channel definition. Logs and swallows non-fatal errors; an
/// `UnknownPt` error after renegotiation isn't worth tearing down the
/// session over.
pub fn dispatch(rtc: &mut str0m::Rtc, request: WriteRequest, wallclock: Instant) {
	let Some(writer) = rtc.writer(request.mid) else {
		tracing::debug!(?request.mid, "egress write before media available");
		return;
	};
	let WriteRequest {
		pt,
		time,
		payload,
		mid: _,
	} = request;
	if let Err(err) = writer.write(pt, wallclock, time, payload.to_vec()) {
		tracing::warn!(%err, "egress write rejected by str0m");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn egress_clock_ignores_cross_track_dequeue_jitter() {
		let mut clock = EgressClock::default();
		let t0 = Instant::now();

		assert_eq!(clock.wallclock(MediaTime::from_millis(1_000), t0), t0);
		assert_eq!(
			clock.wallclock(MediaTime::from_millis(1_100), t0 + Duration::from_millis(100)),
			t0 + Duration::from_millis(100)
		);

		// Two tracks dequeue the same presentation time 50 ms apart. Their
		// sender-report wallclocks must still agree.
		let audio = clock.wallclock(MediaTime::from_millis(1_200), t0 + Duration::from_millis(250));
		let video = clock.wallclock(MediaTime::from_millis(1_200), t0 + Duration::from_millis(300));
		assert_eq!(audio, t0 + Duration::from_millis(200));
		assert_eq!(video, audio);
	}

	#[test]
	fn egress_clock_moves_epoch_earlier_for_catch_up_bursts() {
		let mut clock = EgressClock::default();
		let t0 = Instant::now();

		assert_eq!(clock.wallclock(MediaTime::from_millis(1_000), t0), t0);
		// The next 100 ms of media was already buffered and arrives immediately.
		// Re-anchor it at now instead of handing str0m a future wallclock.
		assert_eq!(clock.wallclock(MediaTime::from_millis(1_100), t0), t0);

		// Once the live edge is known, another track uses the same mapping even
		// when its frames dequeue later.
		assert_eq!(
			clock.wallclock(MediaTime::from_millis(1_100), t0 + Duration::from_millis(50)),
			t0
		);
	}
}
