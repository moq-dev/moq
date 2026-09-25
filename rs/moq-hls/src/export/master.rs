//! Hand-written HLS multivariant (master) playlist generation.
//!
//! Each variant carries its own URI, so a caller with a different layout (a VOD
//! recorder writing `<name>/media.m3u8`, say) reuses the grouping and attribute
//! rules. [`Broadcaster::master_playlist`](super::Broadcaster::master_playlist)
//! renders the live layout, relative to `/<broadcast>/master.m3u8`.

use std::collections::BTreeMap;
use std::fmt::Write;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use super::Kind;

const VERSION: u32 = 9;
const AUDIO_GROUP: &str = "aud";

/// RFC 3986 unreserved characters, which are safe in one URL path segment.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// The live layout's media-playlist URI for a rendition, relative to the master, with an
/// optional query (without the leading `?`) appended.
pub(crate) fn rendition_uri(kind: Kind, name: &str, query: Option<&str>) -> String {
	let mut uri = format!(
		"{}/{}/media.m3u8",
		kind.as_str(),
		utf8_percent_encode(name, PATH_SEGMENT)
	);
	if let Some(query) = query {
		let _ = write!(uri, "?{query}");
	}
	uri
}

fn quoted_string(value: &str) -> String {
	let mut quoted = String::with_capacity(value.len());
	for character in value.chars() {
		// `is_control` spans C0, DEL, and C1 (U+0080..=U+009F), all forbidden in a playlist.
		if character == '"' || character.is_control() {
			for byte in character.encode_utf8(&mut [0; 4]).bytes() {
				let _ = write!(quoted, "%{byte:02X}");
			}
		} else {
			quoted.push(character);
		}
	}
	quoted
}

/// A URI on its own playlist line, where a leading `#` would read as a tag or comment.
fn uri_line(uri: &str) -> String {
	let quoted = quoted_string(uri);
	match quoted.strip_prefix('#') {
		Some(rest) => format!("%23{rest}"),
		None => quoted,
	}
}

/// A video rendition entry for the master playlist.
#[derive(Clone, Debug)]
pub struct VideoVariant {
	/// Media-playlist URI, relative to the master or absolute, including any query.
	pub uri: String,
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
#[derive(Clone, Debug)]
pub struct AudioVariant {
	/// Rendition name, rendered as the `NAME` attribute.
	pub name: String,
	/// Media-playlist URI, relative to the master or absolute, including any query.
	pub uri: String,
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
	if let (Some(width), Some(height)) = (variant.width, variant.height) {
		let _ = write!(line, ",RESOLUTION={width}x{height}");
	}
	let _ = write!(line, ",CODECS=\"{}\"", quoted_string(&codecs));
	if let Some(group) = audio {
		let _ = write!(line, ",AUDIO=\"{}\"", group.id);
	}
	let _ = writeln!(out, "{line}");
	let _ = writeln!(out, "{}", uri_line(&variant.uri));
}

/// Render the multivariant playlist. The first rendition in each audio codec group is default.
///
/// A `"` or control character in a name, codec, or URI is percent-encoded, as is a leading
/// `#` on a variant's URI line, so none can break out of its attribute or line.
pub fn render(video: &[VideoVariant], audio: &[AudioVariant]) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");

	let audio_groups = group_audio(audio);
	for group in &audio_groups {
		for (index, variant) in group.variants.iter().enumerate() {
			let default = if index == 0 { "YES" } else { "NO" };
			let name = quoted_string(&variant.name);
			let _ = writeln!(
				out,
				"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\",DEFAULT={default},AUTOSELECT=YES,URI=\"{}\"",
				group.id,
				name,
				quoted_string(&variant.uri)
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
				variant.bandwidth,
				quoted_string(&variant.codec)
			);
			let _ = writeln!(out, "{}", uri_line(&variant.uri));
		}
	}

	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn video(name: &str, width: Option<u32>, height: Option<u32>) -> VideoVariant {
		VideoVariant {
			uri: rendition_uri(Kind::Video, name, None),
			bandwidth: 2_500_000,
			width,
			height,
			codec: "avc1.42c01f".into(),
		}
	}

	fn audio(name: &str, bandwidth: u64, codec: &str) -> AudioVariant {
		AudioVariant {
			name: name.into(),
			uri: rendition_uri(Kind::Audio, name, None),
			bandwidth,
			codec: codec.into(),
		}
	}

	#[test]
	fn renders_video_and_audio() {
		let out = render(
			&[video("video", Some(1280), Some(720))],
			&[audio("audio", 128_000, "mp4a.40.2")],
		);
		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:9\n"));
		assert!(out.contains(
			"#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"audio\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio/audio/media.m3u8\"\n"
		));
		assert!(out.contains(
			"#EXT-X-STREAM-INF:BANDWIDTH=2628000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud\"\n"
		));
		assert!(out.contains("\nvideo/video/media.m3u8\n"));
	}

	#[test]
	fn renders_each_variant_at_its_own_uri() {
		let mut hd = video("hd", None, None);
		hd.uri = "hd/media.m3u8".into();
		let mut main = audio("main", 128_000, "opus");
		main.uri = "https://cdn.example/main/media.m3u8?jwt=abc".into();

		let out = render(&[hd], &[main]);
		assert!(out.contains("\nhd/media.m3u8\n"));
		assert!(out.contains("URI=\"https://cdn.example/main/media.m3u8?jwt=abc\""));
	}

	#[test]
	fn separates_audio_codecs_into_accurate_variants() {
		let out = render(
			&[video("video", Some(1280), Some(720))],
			&[
				audio("aac-low", 96_000, "mp4a.40.2"),
				audio("aac-high", 128_000, "mp4a.40.2"),
				audio("opus", 160_000, "opus"),
			],
		);
		assert!(out.contains("GROUP-ID=\"aud-0\",NAME=\"aac-low\",DEFAULT=YES"));
		assert!(out.contains("GROUP-ID=\"aud-0\",NAME=\"aac-high\",DEFAULT=NO"));
		assert!(out.contains("GROUP-ID=\"aud-1\",NAME=\"opus\",DEFAULT=YES"));
		assert!(out.contains("BANDWIDTH=2628000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,mp4a.40.2\",AUDIO=\"aud-0\""));
		assert!(out.contains("BANDWIDTH=2660000,RESOLUTION=1280x720,CODECS=\"avc1.42c01f,opus\",AUDIO=\"aud-1\""));
		assert_eq!(out.matches("\nvideo/video/media.m3u8\n").count(), 2);
	}

	#[test]
	fn audio_only_is_playable() {
		let out = render(&[], &[audio("audio", 128_000, "opus")]);
		assert!(out.contains("#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"opus\"\n"));
		assert!(out.contains("\naudio/audio/media.m3u8\n"));
	}

	#[test]
	fn rendition_names_are_percent_encoded_in_uris() {
		assert_eq!(
			rendition_uri(Kind::Video, "cam#1/main?alt", Some("jwt=abc.def")),
			"video/cam%231%2Fmain%3Falt/media.m3u8?jwt=abc.def"
		);
		assert_eq!(
			rendition_uri(Kind::Audio, "audio #1", None),
			"audio/audio%20%231/media.m3u8"
		);
	}

	#[test]
	fn names_and_uris_cannot_inject() {
		let mut variant = audio("音声\"\nINJECT\u{7f}\u{85}\u{1f3b5}", 128_000, "opus");
		variant.uri = "a\"\n#EXT-X-INJECT".into();

		let out = render(&[], &[variant]);

		assert!(out.contains("NAME=\"音声%22%0AINJECT%7F%C2%85🎵\""), "{out}");
		assert!(out.contains("URI=\"a%22%0A#EXT-X-INJECT\""));
		assert!(!out.contains("\nINJECT"));
		assert!(!out.contains("\n#EXT-X-INJECT"));
	}

	#[test]
	fn codecs_and_uri_lines_cannot_inject() {
		let mut hd = video("hd", None, None);
		hd.codec = "avc1\"\n#EXT-X-ENDLIST".into();
		hd.uri = "#EXT-X-ENDLIST".into();
		let out = render(&[hd], &[]);
		assert!(
			out.contains("CODECS=\"avc1%22%0A#EXT-X-ENDLIST\"\n%23EXT-X-ENDLIST\n"),
			"{out}"
		);
		assert!(!out.contains("\n#EXT-X-ENDLIST"), "{out}");

		let mut main = audio("main", 128_000, "opus\"\n#EXT-X-ENDLIST");
		main.uri = "#frag".into();
		let out = render(&[], &[main]);
		assert!(out.contains("CODECS=\"opus%22%0A#EXT-X-ENDLIST\"\n%23frag\n"), "{out}");
		assert!(!out.contains("\n#EXT-X-ENDLIST"), "{out}");
	}
}
