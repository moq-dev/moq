//! Hand-written HLS media playlist generation.
//!
//! Rendered purely from timeline records: each record is one complete aligned segment
//! (numbered by the broadcast's shared segment sequence, so renditions cut at the same
//! boundaries) carrying its own duration. URIs are relative to the media playlist
//! (`/<broadcast>/<kind>/<rendition>/media.m3u8`), so they resolve against the rendition
//! directory.

use std::fmt::Write;
use std::time::SystemTime;

/// fMP4 segments via `EXT-X-MAP` require protocol version 6.
const VERSION: u32 = 6;

/// `EXT-X-GAP` requires protocol version 8.
const VERSION_GAP: u32 = 8;

/// Everything a media playlist render needs; built by the crate-internal
/// `Rendition::playlist`.
pub(crate) struct Snapshot {
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
pub(crate) struct Segment {
	/// The aligned segment number; the URI is `seg/{segment}.m4s`.
	pub segment: u64,
	/// `EXTINF` duration in seconds.
	pub duration: f64,
	/// The rendition has no content for this span (`EXT-X-GAP`): the segment keeps its slot in
	/// the aligned numbering, but a player should not request it.
	pub gap: bool,
	/// Content time jumped before this segment (`EXT-X-DISCONTINUITY`).
	pub discontinuity: bool,
}

/// Render a media playlist for one rendition from a [`Snapshot`].
///
/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
/// appended to every child URL (the init map and each segment), so a stock player that
/// does not replay request headers still carries a credential on its follow-up requests.
pub(crate) fn render_media(snapshot: &Snapshot, query: Option<&str>) -> String {
	let suffix = query.map(|q| format!("?{q}")).unwrap_or_default();

	// EXT-X-GAP needs a newer protocol version; only require it when actually used.
	let version = if snapshot.segments.iter().any(|s| s.gap) {
		VERSION_GAP
	} else {
		VERSION
	};

	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{version}");
	let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", snapshot.target_duration);
	let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", snapshot.media_sequence);
	let _ = writeln!(out, "#EXT-X-MAP:URI=\"init.mp4{suffix}\"");

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
		if segment.discontinuity {
			let _ = writeln!(out, "#EXT-X-DISCONTINUITY");
		}
		if segment.gap {
			let _ = writeln!(out, "#EXT-X-GAP");
		}
		let _ = writeln!(out, "#EXTINF:{:.5},", segment.duration);
		let _ = writeln!(out, "seg/{}.m4s{suffix}", segment.segment);
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
					segment: 10,
					duration: 2.0,
					gap: false,
					discontinuity: false,
				},
				Segment {
					segment: 11,
					duration: 1.96,
					gap: false,
					discontinuity: false,
				},
			],
			finished: false,
			program_date_time: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_751_846_400_123)),
		};

		let out = render_media(&snapshot, None);
		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:6\n"));
		assert!(out.contains("#EXT-X-TARGETDURATION:2\n"));
		assert!(out.contains("#EXT-X-MEDIA-SEQUENCE:10\n"));
		assert!(out.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
		assert!(out.contains("#EXT-X-PROGRAM-DATE-TIME:2025-07-07T00:00:00.123Z\n"));
		assert!(out.contains("#EXTINF:2.00000,\nseg/10.m4s\n"));
		assert!(out.contains("#EXTINF:1.96000,\nseg/11.m4s\n"));
		assert!(!out.contains("#EXT-X-ENDLIST"));

		// A credential rides every child URL so a header-less player keeps sending it.
		let signed = render_media(&snapshot, Some("jwt=abc.def"));
		assert!(signed.contains("#EXT-X-MAP:URI=\"init.mp4?jwt=abc.def\"\n"));
		assert!(signed.contains("\nseg/10.m4s?jwt=abc.def\n"));
		assert!(signed.contains("\nseg/11.m4s?jwt=abc.def\n"));
	}

	#[test]
	fn finished_playlist_has_endlist() {
		let snapshot = Snapshot {
			target_duration: 4,
			media_sequence: 0,
			segments: vec![Segment {
				segment: 0,
				duration: 4.0,
				gap: false,
				discontinuity: false,
			}],
			finished: true,
			program_date_time: None,
		};

		let out = render_media(&snapshot, None);
		assert!(out.contains("#EXT-X-ENDLIST\n"));
		assert!(!out.contains("PROGRAM-DATE-TIME"));
	}
}
