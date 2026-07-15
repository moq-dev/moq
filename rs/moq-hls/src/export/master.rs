//! Hand-written HLS multivariant (master) playlist generation.
//!
//! URIs are relative to the master playlist (`/<broadcast>/master.m3u8`), so a
//! rendition's `<name>/media.m3u8` resolves under the broadcast directory.

use std::collections::BTreeMap;
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

fn render_video(out: &mut String, variant: &VideoVariant, audio: Option<&AudioGroup<'_>>) {
	let bandwidth = variant
		.bandwidth
		.saturating_add(audio.map_or(0, |group| group.bandwidth));
	let codecs = audio.map_or_else(
		|| variant.codec.clone(),
		|group| format!("{},{}", variant.codec, group.codec),
	);
	let mut line = format!("#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}");
	if let (Some(w), Some(h)) = (variant.width, variant.height) {
		let _ = write!(line, ",RESOLUTION={w}x{h}");
	}
	let _ = write!(line, ",CODECS=\"{codecs}\"");
	if let Some(group) = audio {
		let _ = write!(line, ",AUDIO=\"{}\"", group.id);
	}
	let _ = writeln!(out, "{line}");
	let _ = writeln!(out, "{}/media.m3u8", variant.name);
}

/// Render the multivariant playlist. The first rendition in each audio codec group is default.
pub fn render_master(video: &[VideoVariant], audio: &[AudioVariant]) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");

	let audio_groups = group_audio(audio);
	for group in &audio_groups {
		for (index, variant) in group.variants.iter().enumerate() {
			let default = if index == 0 { "YES" } else { "NO" };
			let _ = writeln!(
				out,
				"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\",DEFAULT={default},AUTOSELECT=YES,URI=\"{}/media.m3u8\"",
				group.id, variant.name, variant.name
			);
		}
	}

	for variant in video {
		if audio_groups.is_empty() {
			render_video(&mut out, variant, None);
		} else {
			for group in &audio_groups {
				render_video(&mut out, variant, Some(group));
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
			"#EXT-X-STREAM-INF:BANDWIDTH=2628000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud\"\n"
		));
		assert!(out.contains("\nvideo/media.m3u8\n"));
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

		let out = render_master(&video, &audio);
		assert!(out.contains(
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud-0\",NAME=\"aac-low\",DEFAULT=YES,AUTOSELECT=YES,URI=\"aac-low/media.m3u8\"\n"
		));
		assert!(out.contains(
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud-0\",NAME=\"aac-high\",DEFAULT=NO,AUTOSELECT=YES,URI=\"aac-high/media.m3u8\"\n"
		));
		assert!(out.contains(
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud-1\",NAME=\"opus\",DEFAULT=YES,AUTOSELECT=YES,URI=\"opus/media.m3u8\"\n"
		));
		assert!(out.contains(
			"#EXT-X-STREAM-INF:BANDWIDTH=2628000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud-0\"\n"
		));
		assert!(out.contains(
			"#EXT-X-STREAM-INF:BANDWIDTH=2660000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,opus\",AUDIO=\"aud-1\"\n"
		));
		assert_eq!(out.matches("\nvideo/media.m3u8\n").count(), 2);
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
