//! Codec compatibility helpers shared by the egress muxers and the RTP egress.
//!
//! Each egress format (MPEG-TS, FLV/RTMP, WebRTC) can only carry a subset of the
//! codecs a hang catalog might contain. When a broadcast carries a codec a given
//! format can't, that rendition is *dropped*: the rest still egresses, and the
//! gateway reports the drop so the dashboard can flag the incompatibility (e.g.
//! "AAC audio can't go over WebRTC, use Opus").

use std::time::Duration;

use anyhow::Context;
use hang::catalog::{AudioCodec, VideoCodec};

use crate::catalog::hang::{Catalog, CatalogExt};

/// An egress protocol, used to pick the codec set it can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
	/// WebRTC / WHEP.
	Webrtc,
	/// FLV / RTMP (enhanced-RTMP FourCCs included).
	Flv,
	/// MPEG-TS (SRT).
	Ts,
}

/// Whether `protocol` can carry this audio codec on egress.
pub fn carries_audio(protocol: Protocol, codec: &AudioCodec) -> bool {
	match protocol {
		Protocol::Webrtc => matches!(codec, AudioCodec::Opus),
		Protocol::Flv => matches!(
			codec,
			AudioCodec::AAC(_) | AudioCodec::Opus | AudioCodec::Ac3 | AudioCodec::Ec3
		),
		Protocol::Ts => matches!(
			codec,
			AudioCodec::AAC(_) | AudioCodec::Mp2 | AudioCodec::Ac3 | AudioCodec::Ec3
		),
	}
}

/// Whether `protocol` can carry this video codec on egress.
pub fn carries_video(protocol: Protocol, codec: &VideoCodec) -> bool {
	match protocol {
		Protocol::Webrtc => matches!(
			codec,
			VideoCodec::H264(_) | VideoCodec::H265(_) | VideoCodec::VP8 | VideoCodec::VP9(_) | VideoCodec::AV1(_)
		),
		Protocol::Flv => matches!(
			codec,
			VideoCodec::H264(_) | VideoCodec::H265(_) | VideoCodec::AV1(_) | VideoCodec::VP9(_)
		),
		Protocol::Ts => matches!(codec, VideoCodec::H264(_) | VideoCodec::H265(_)),
	}
}

/// Renditions in `catalog` that `protocol` can't carry.
pub fn classify<E: CatalogExt>(catalog: &Catalog<E>, protocol: Protocol) -> Vec<DroppedTrack> {
	let mut out = Vec::new();
	for r in catalog.audio.renditions.values() {
		if !carries_audio(protocol, &r.codec) {
			out.push(DroppedTrack::audio(&r.codec));
		}
	}
	for r in catalog.video.renditions.values() {
		if !carries_video(protocol, &r.codec) {
			out.push(DroppedTrack::video(&r.codec));
		}
	}
	out
}

/// Resolve the broadcast at `path`, read its catalog, and report the renditions
/// `protocol` can't carry. Best-effort and bounded: if the broadcast or its
/// catalog doesn't resolve quickly, returns empty rather than blocking egress.
///
/// For egress paths that hand the muxer/peer back the dropped set directly (WHEP's
/// `Response::dropped`, the TS exporter's `Export::dropped`), prefer that. This is
/// for the RTMP path, whose serve owns the exporter and can't surface it mid-session.
pub async fn dropped_for(origin: &moq_net::OriginConsumer, path: &str, protocol: Protocol) -> Vec<DroppedTrack> {
	let fetch = async {
		let broadcast = origin
			.request_broadcast(path)
			.await
			.context("resolve broadcast for compat check")?;
		let track = broadcast.subscribe_track(&moq_net::Track::new(hang::Catalog::DEFAULT_NAME))?;
		let mut consumer = crate::catalog::hang::Consumer::<()>::new(track);
		let catalog = consumer.next().await?.context("catalog closed before snapshot")?;
		anyhow::Ok(classify(&catalog, protocol))
	};
	match tokio::time::timeout(Duration::from_secs(3), fetch).await {
		Ok(Ok(dropped)) => dropped,
		Ok(Err(err)) => {
			tracing::debug!(%err, "compat check skipped");
			Vec::new()
		}
		Err(_) => {
			tracing::debug!("compat check timed out");
			Vec::new()
		}
	}
}

/// Which half of the catalog a dropped track came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
	Audio,
	Video,
}

/// A catalog rendition an egress format can't carry, surfaced for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedTrack {
	pub kind: TrackKind,
	/// Friendly codec label, e.g. "AAC", "Opus", "VP8".
	pub codec: String,
}

impl DroppedTrack {
	pub fn audio(codec: &AudioCodec) -> Self {
		Self {
			kind: TrackKind::Audio,
			codec: audio_codec_name(codec),
		}
	}

	pub fn video(codec: &VideoCodec) -> Self {
		Self {
			kind: TrackKind::Video,
			codec: video_codec_name(codec),
		}
	}
}

/// Short human label for an audio codec (for incompatibility messages).
pub fn audio_codec_name(codec: &AudioCodec) -> String {
	match codec {
		AudioCodec::AAC(_) => "AAC".into(),
		AudioCodec::Opus => "Opus".into(),
		AudioCodec::Mp2 => "MP2".into(),
		AudioCodec::Ac3 => "AC-3".into(),
		AudioCodec::Ec3 => "E-AC-3".into(),
		AudioCodec::Unknown(s) => s.clone(),
		_ => "unknown audio".into(),
	}
}

/// Short human label for a video codec (for incompatibility messages).
pub fn video_codec_name(codec: &VideoCodec) -> String {
	match codec {
		VideoCodec::H264(_) => "H.264".into(),
		VideoCodec::H265(_) => "H.265".into(),
		VideoCodec::VP8 => "VP8".into(),
		VideoCodec::VP9(_) => "VP9".into(),
		VideoCodec::AV1(_) => "AV1".into(),
		VideoCodec::Unknown(s) => s.clone(),
		_ => "unknown video".into(),
	}
}

#[cfg(test)]
mod tests {
	use std::str::FromStr;

	use super::*;

	fn aac() -> AudioCodec {
		AudioCodec::from_str("mp4a.40.2").unwrap()
	}
	fn h264() -> VideoCodec {
		VideoCodec::from_str("avc1.42001f").unwrap()
	}
	fn h265() -> VideoCodec {
		VideoCodec::from_str("hvc1.1.6.L93.B0").unwrap()
	}
	fn av1() -> VideoCodec {
		VideoCodec::from_str("av01.0.04M.08").unwrap()
	}

	#[test]
	fn protocol_codec_support_matrix() {
		// The case that motivated this: AAC can't go over WebRTC, but can over RTMP/SRT.
		assert!(!carries_audio(Protocol::Webrtc, &aac()));
		assert!(carries_audio(Protocol::Flv, &aac()));
		assert!(carries_audio(Protocol::Ts, &aac()));

		// Opus: WebRTC + RTMP yes, MPEG-TS no.
		assert!(carries_audio(Protocol::Webrtc, &AudioCodec::Opus));
		assert!(carries_audio(Protocol::Flv, &AudioCodec::Opus));
		assert!(!carries_audio(Protocol::Ts, &AudioCodec::Opus));

		// MP2: only MPEG-TS.
		assert!(!carries_audio(Protocol::Flv, &AudioCodec::Mp2));
		assert!(carries_audio(Protocol::Ts, &AudioCodec::Mp2));

		// VP8: only WebRTC. AV1: WebRTC + RTMP, not TS.
		assert!(carries_video(Protocol::Webrtc, &VideoCodec::VP8));
		assert!(!carries_video(Protocol::Flv, &VideoCodec::VP8));
		assert!(!carries_video(Protocol::Ts, &VideoCodec::VP8));
		assert!(carries_video(Protocol::Flv, &av1()));
		assert!(!carries_video(Protocol::Ts, &av1()));

		// H.264/H.265: everyone.
		for p in [Protocol::Webrtc, Protocol::Flv, Protocol::Ts] {
			assert!(carries_video(p, &h264()));
			assert!(carries_video(p, &h265()));
		}
	}

	#[test]
	fn dropped_track_labels() {
		assert_eq!(DroppedTrack::audio(&aac()).codec, "AAC");
		assert_eq!(DroppedTrack::video(&VideoCodec::VP8).codec, "VP8");
		assert_eq!(DroppedTrack::audio(&AudioCodec::Opus).kind, TrackKind::Audio);
	}
}
