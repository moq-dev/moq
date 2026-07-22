//! Hand-written HLS multivariant (master) playlist generation.
//!
//! URIs are relative to the master playlist (`/<broadcast>/master.m3u8`), so a
//! rendition's `<kind>/<name>/media.m3u8` resolves under the broadcast directory.

use std::collections::BTreeMap;
use std::fmt::Write;

use super::Kind;

const VERSION: u32 = 9;
const AUDIO_GROUP: &str = "aud";

/// A video rendition entry for the master playlist.
pub struct VideoVariant {
	/// Rendition name (the `<name>` in its `<kind>/<name>/media.m3u8` path).
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
	/// Rendition name (the `<name>` in its `<kind>/<name>/media.m3u8` path).
	pub name: String,
	/// `BANDWIDTH` attribute, in bits per second.
	pub bandwidth: u64,
	/// RFC 6381 codec string (e.g. `mp4a.40.2`).
	pub codec: String,
}

struct AudioGroup<'a> {
	id: String,
	bandwidth: u64,
	codec: &'a str,
	variants: Vec<&'a AudioVariant>,
}

fn group_audio(audio: &[AudioVariant]) -> Vec<AudioGroup<'_>> {
	let mut codecs = BTreeMap::<&str, Vec<&AudioVariant>>::new();
	for variant in audio {
		codecs.entry(&variant.codec).or_default().push(variant);
	}

	let multiple = codecs.len() > 1;
	codecs
		.into_iter()
		.enumerate()
		.map(|(index, (codec, variants))| AudioGroup {
			id: if multiple {
				format!("{AUDIO_GROUP}-{index}")
			} else {
				AUDIO_GROUP.to_string()
			},
			bandwidth: variants
				.iter()
				.map(|variant| variant.bandwidth)
				.max()
				.unwrap_or_default(),
			codec,
			variants,
		})
		.collect()
}

fn render_video(out: &mut String, variant: &VideoVariant, audio: Option<&AudioGroup<'_>>, suffix: &str) {
	let bandwidth = variant
		.bandwidth
		.saturating_add(audio.map_or(0, |group| group.bandwidth));
	let codecs = audio.map_or_else(
		|| variant.codec.clone(),
		|group| format!("{},{}", variant.codec, group.codec),
	);
	let mut line = format!("#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}");
	if let (Some(width), Some(height)) = (variant.width, variant.height) {
		let _ = write!(line, ",RESOLUTION={width}x{height}");
	}
	let _ = write!(line, ",CODECS=\"{codecs}\"");
	if let Some(group) = audio {
		let _ = write!(line, ",AUDIO=\"{}\"", group.id);
	}
	let _ = writeln!(out, "{line}");
	let _ = writeln!(out, "{}/{}/media.m3u8{suffix}", Kind::Video.as_str(), variant.name);
}

/// Render the multivariant playlist. The first rendition in each audio codec group is default.
///
/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
/// appended to every child media-playlist URL, so a credential the master was fetched
/// with propagates to the rendition playlists a stock player loads next.
pub fn render_master(video: &[VideoVariant], audio: &[AudioVariant], query: Option<&str>) -> String {
	let suffix = query.map(|q| format!("?{q}")).unwrap_or_default();

	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");

	let audio_groups = group_audio(audio);
	for group in &audio_groups {
		for (index, variant) in group.variants.iter().enumerate() {
			let default = if index == 0 { "YES" } else { "NO" };
			let _ = writeln!(
				out,
				"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\",DEFAULT={default},AUTOSELECT=YES,URI=\"{}/{}/media.m3u8{suffix}\"",
				group.id,
				variant.name,
				Kind::Audio.as_str(),
				variant.name
			);
		}
	}

	for variant in video {
		if audio_groups.is_empty() {
			render_video(&mut out, variant, None, &suffix);
		} else {
			for group in &audio_groups {
				render_video(&mut out, variant, Some(group), &suffix);
			}
		}
	}

	// Audio-only broadcast: still expose a playable variant per audio rendition.
	if video.is_empty() {
		for variant in audio {
			let _ = writeln!(
				out,
				"#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{}\"",
				variant.bandwidth, variant.codec
			);
			let _ = writeln!(out, "{}/{}/media.m3u8{suffix}", Kind::Audio.as_str(), variant.name);
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

		let out = render_master(&video, &audio, None);
		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:9\n"));
		assert!(out.contains(
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"audio\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio/audio/media.m3u8\"\n"
		));
		assert!(out.contains(
			"#EXT-X-STREAM-INF:BANDWIDTH=2628000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud\"\n"
		));
		assert!(out.contains("\nvideo/video/media.m3u8\n"));

		// A credential rides every child media-playlist URL, audio and video alike.
		let signed = render_master(&video, &audio, Some("jwt=abc.def"));
		assert!(signed.contains("URI=\"audio/audio/media.m3u8?jwt=abc.def\"\n"));
		assert!(signed.contains("\nvideo/video/media.m3u8?jwt=abc.def\n"));
	}

	#[test]
	fn separates_audio_codecs_into_accurate_variants() {
		let video = vec![VideoVariant {
			name: "video".into(),
			bandwidth: 2_500_000,
			width: Some(1280),
			height: Some(720),
			codec: "avc1.42c01f".into(),
		}];
		let audio = vec![
			AudioVariant {
				name: "aac-low".into(),
				bandwidth: 96_000,
				codec: "mp4a.40.2".into(),
			},
			AudioVariant {
				name: "aac-high".into(),
				bandwidth: 128_000,
				codec: "mp4a.40.2".into(),
			},
			AudioVariant {
				name: "opus".into(),
				bandwidth: 160_000,
				codec: "opus".into(),
			},
		];

		let out = render_master(&video, &audio, None);
		assert!(out.contains("GROUP-ID=\"aud-0\",NAME=\"aac-low\",DEFAULT=YES"));
		assert!(out.contains("GROUP-ID=\"aud-0\",NAME=\"aac-high\",DEFAULT=NO"));
		assert!(out.contains("GROUP-ID=\"aud-1\",NAME=\"opus\",DEFAULT=YES"));
		assert!(out.contains("BANDWIDTH=2628000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud-0\""));
		assert!(out.contains("BANDWIDTH=2660000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,opus\",AUDIO=\"aud-1\""));
		assert_eq!(out.matches("\nvideo/video/media.m3u8\n").count(), 2);
	}

	#[test]
	fn audio_only_is_playable() {
		let audio = vec![AudioVariant {
			name: "audio".into(),
			bandwidth: 128_000,
			codec: "opus".into(),
		}];
		let out = render_master(&[], &audio, None);
		assert!(out.contains("#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"opus\"\n"));
		assert!(out.contains("\naudio/audio/media.m3u8\n"));
	}
}
