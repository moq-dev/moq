//! Hand-written HLS media playlist generation.
//!
//! Rendered purely from timeline records: each segment is a starting group plus the gap to
//! the next record. URIs are relative to the media playlist
//! (`/<broadcast>/<kind>/<rendition>/media.m3u8`), so they resolve against the rendition
//! directory.

use std::fmt::Write;
use std::time::SystemTime;

/// fMP4 segments via `EXT-X-MAP` require protocol version 6.
const VERSION: u32 = 6;

/// Everything a media playlist render needs; built by
/// [`Rendition::playlist`](super::Rendition::playlist).
pub struct Snapshot {
	/// `EXT-X-TARGETDURATION`, in whole seconds.
	pub target_duration: u64,
	/// `EXT-X-MEDIA-SEQUENCE` of the first listed segment.
	pub media_sequence: u64,
	/// Listed segments, oldest first.
	pub segments: Vec<Segment>,
	/// Whether the broadcast ended (`EXT-X-ENDLIST`).
	pub finished: bool,
	/// Wall-clock time of the first listed segment (`EXT-X-PROGRAM-DATE-TIME`), when the
	/// timeline advertises a wall-clock anchor.
	pub program_date_time: Option<SystemTime>,
}

/// One listed segment.
pub struct Segment {
	/// The starting group sequence; the URI is `seg/{group}.m4s`.
	pub group: u64,
	/// `EXTINF` duration in seconds.
	pub duration: f64,
}

/// Render a media playlist for one rendition from a [`Snapshot`].
pub fn render_media(snapshot: &Snapshot) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");
	let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", snapshot.target_duration);
	let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", snapshot.media_sequence);
	let _ = writeln!(out, "#EXT-X-MAP:URI=\"init.mp4\"");

	for (index, segment) in snapshot.segments.iter().enumerate() {
		if index == 0
			&& let Some(pdt) = snapshot.program_date_time
		{
			let _ = writeln!(
				out,
				"#EXT-X-PROGRAM-DATE-TIME:{}",
				humantime::format_rfc3339_millis(pdt)
			);
		}
		let _ = writeln!(out, "#EXTINF:{:.5},", segment.duration);
		let _ = writeln!(out, "seg/{}.m4s", segment.group);
	}

	if snapshot.finished {
		let _ = writeln!(out, "#EXT-X-ENDLIST");
	}

	out
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	#[test]
	fn renders_live_playlist() {
		let snapshot = Snapshot {
			target_duration: 2,
			media_sequence: 10,
			segments: vec![
				Segment {
					group: 10,
					duration: 2.0,
				},
				Segment {
					group: 11,
					duration: 1.96,
				},
			],
			finished: false,
			program_date_time: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_751_846_400_123)),
		};

		let out = render_media(&snapshot);
		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:6\n"));
		assert!(out.contains("#EXT-X-TARGETDURATION:2\n"));
		assert!(out.contains("#EXT-X-MEDIA-SEQUENCE:10\n"));
		assert!(out.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
		assert!(out.contains("#EXT-X-PROGRAM-DATE-TIME:2025-07-07T00:00:00.123Z\n"));
		assert!(out.contains("#EXTINF:2.00000,\nseg/10.m4s\n"));
		assert!(out.contains("#EXTINF:1.96000,\nseg/11.m4s\n"));
		assert!(!out.contains("#EXT-X-ENDLIST"));
	}

	#[test]
	fn finished_playlist_has_endlist() {
		let snapshot = Snapshot {
			target_duration: 4,
			media_sequence: 0,
			segments: vec![Segment {
				group: 0,
				duration: 4.0,
			}],
			finished: true,
			program_date_time: None,
		};

		let out = render_media(&snapshot);
		assert!(out.contains("#EXT-X-ENDLIST\n"));
		assert!(!out.contains("PROGRAM-DATE-TIME"));
	}
}
