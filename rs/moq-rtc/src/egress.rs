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

use std::time::Instant;

use bytes::Bytes;
use hang::Catalog;
use hang::catalog::{AudioCodec, VideoCodecKind};
use str0m::format::Codec;
use str0m::media::{Frequency, MediaTime, Mid, Pt};
use tokio::sync::mpsc;

use crate::{Error, Result, codec};

/// One frame waiting to be written into str0m's [`Writer`](str0m::media::Writer).
///
/// Pump tasks build these and send them down the channel; the session loop
/// receives them and calls `rtc.writer(mid).write(pt, wallclock, time, payload)`.
pub struct WriteRequest {
	pub mid: Mid,
	pub pt: Pt,
	pub time: MediaTime,
	pub payload: Bytes,
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
		let catalog_track = broadcast
			.consume_track(hang::Catalog::DEFAULT_NAME)
			.subscribe(hang::Catalog::default_subscription())
			.await?;
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
			let codec = match rendition.codec.kind() {
				VideoCodecKind::H264 => Some(Codec::H264),
				VideoCodecKind::VP8 => Some(Codec::Vp8),
				VideoCodecKind::VP9 => Some(Codec::Vp9),
				_ => None,
			};
			if let Some(c) = codec
				&& !out.contains(&c)
			{
				out.push(c);
			}
		}
		out
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
		Codec::H264 | Codec::Vp8 | Codec::Vp9 => {
			let target = match codec {
				Codec::H264 => VideoCodecKind::H264,
				Codec::Vp8 => VideoCodecKind::VP8,
				Codec::Vp9 => VideoCodecKind::VP9,
				_ => unreachable!(),
			};
			let Some((name, config)) = catalog.video.renditions.iter().find(|(_, c)| c.codec.kind() == target) else {
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
