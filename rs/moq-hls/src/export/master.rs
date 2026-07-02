//! Hand-written HLS multivariant (master) playlist generation.
//!
//! URIs are relative to the master playlist (`/<broadcast>/master.m3u8`), so a
//! rendition's `<name>/media.m3u8` resolves under the broadcast directory.

use std::fmt::Write;

const VERSION: u32 = 9;
const AUDIO_GROUP: &str = "aud";

/// A video rendition entry for the master playlist.
pub struct VideoVariant {
	/// Rendition name (also its `<name>/media.m3u8` path component).
	pub name: String,
	/// `BANDWIDTH` attribute, in bits per second.
	pub bandwidth: u64,
	/// Coded width for the `RESOLUTION` attribute, if known.
	pub width: Option<u32>,
	/// Coded height for the `RESOLUTION` attribute, if known.
	pub height: Option<u32>,
	/// RFC 6381 codec string (e.g. `avc1.42c01f`).
	pub codec: String,
}

/// An audio rendition entry for the master playlist.
pub struct AudioVariant {
	/// Rendition name (also its `<name>/media.m3u8` path component).
	pub name: String,
	/// `BANDWIDTH` attribute, in bits per second.
	pub bandwidth: u64,
	/// RFC 6381 codec string (e.g. `mp4a.40.2`).
	pub codec: String,
}

/// Render the multivariant playlist. The first audio rendition is marked default.
pub fn render_master(video: &[VideoVariant], audio: &[AudioVariant]) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");

	let has_audio = !audio.is_empty();
	for (index, variant) in audio.iter().enumerate() {
		let default = if index == 0 { "YES" } else { "NO" };
		let _ = writeln!(
			out,
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{AUDIO_GROUP}\",NAME=\"{}\",DEFAULT={default},AUTOSELECT=YES,URI=\"{}/media.m3u8\"",
			variant.name, variant.name
		);
	}

	// One audio codec is enough for the combined CODECS attribute.
	let audio_codec = audio.first().map(|a| a.codec.as_str());

	for variant in video {
		let codecs = match audio_codec {
			Some(audio) => format!("{},{}", variant.codec, audio),
			None => variant.codec.clone(),
		};

		let mut line = format!("#EXT-X-STREAM-INF:BANDWIDTH={}", variant.bandwidth);
		if let (Some(w), Some(h)) = (variant.width, variant.height) {
			let _ = write!(line, ",RESOLUTION={w}x{h}");
		}
		let _ = write!(line, ",CODECS=\"{codecs}\"");
		if has_audio {
			let _ = write!(line, ",AUDIO=\"{AUDIO_GROUP}\"");
		}
		let _ = writeln!(out, "{line}");
		let _ = writeln!(out, "{}/media.m3u8", variant.name);
	}

	// Audio-only broadcast: still expose a playable variant per audio rendition.
	if video.is_empty() {
		for variant in audio {
			let _ = writeln!(
				out,
				"#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{}\"",
				variant.bandwidth, variant.codec
			);
			let _ = writeln!(out, "{}/media.m3u8", variant.name);
		}
	}

	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn renders_video_and_audio() {
		let video = vec![VideoVariant {
			name: "video".into(),
			bandwidth: 2_500_000,
			width: Some(1280),
			height: Some(720),
			codec: "avc1.42c01f".into(),
		}];
		let audio = vec![AudioVariant {
			name: "audio".into(),
			bandwidth: 128_000,
			codec: "mp4a.40.2".into(),
		}];

		let out = render_master(&video, &audio);
		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:9\n"));
		assert!(out.contains(
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"audio\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio/media.m3u8\"\n"
		));
		assert!(out.contains(
			"#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud\"\n"
		));
		assert!(out.contains("\nvideo/media.m3u8\n"));
	}

	#[test]
	fn audio_only_is_playable() {
		let audio = vec![AudioVariant {
			name: "audio".into(),
			bandwidth: 128_000,
			codec: "opus".into(),
		}];
		let out = render_master(&[], &audio);
		assert!(out.contains("#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"opus\"\n"));
		assert!(out.contains("\naudio/media.m3u8\n"));
	}
}
