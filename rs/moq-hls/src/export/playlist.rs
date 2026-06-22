//! Hand-written HLS / LL-HLS media playlist generation.
//!
//! `m3u8-rs` can parse classic playlists but cannot emit the LL-HLS tags
//! (`EXT-X-PART`, `EXT-X-PART-INF`, `EXT-X-SERVER-CONTROL`,
//! `EXT-X-PRELOAD-HINT`), so the export playlists are written by hand. URIs are
//! relative to the media playlist (`/<broadcast>/<rendition>/media.m3u8`), so
//! they resolve against the rendition directory.

use std::fmt::Write;

use super::store::Snapshot;

/// LL-HLS compatibility version: required for `EXT-X-PART` and friends.
const VERSION: u32 = 9;

/// Render a media playlist for one rendition from a [`Snapshot`].
///
/// `query` is appended to every URI (init, parts, segments, preload hint) so a
/// token-gated player carries the token onto each sub-request; pass `None` for
/// public broadcasts. See [`super::query_suffix`].
pub fn render_media(snapshot: &Snapshot, query: Option<&str>) -> String {
	let q = super::query_suffix(query);

	// TARGETDURATION must be >= the longest *complete* segment (rounded up), and
	// at least the part target so a part-only edge still produces a sane value.
	let max_segment = snapshot
		.segments
		.iter()
		.filter(|s| s.complete)
		.map(|s| s.duration)
		.fold(0.0_f64, f64::max)
		.max(snapshot.part_target);
	let target_duration = max_segment.ceil().max(1.0) as u64;

	// PART-HOLD-BACK must be at least 3x the part target (HLS spec).
	let part_hold_back = snapshot.part_target * 3.0;

	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");
	let _ = writeln!(out, "#EXT-X-TARGETDURATION:{target_duration}");
	let _ = writeln!(
		out,
		"#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={part_hold_back:.3}"
	);
	let _ = writeln!(out, "#EXT-X-PART-INF:PART-TARGET={:.3}", snapshot.part_target);
	let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", snapshot.media_sequence);
	let _ = writeln!(out, "#EXT-X-MAP:URI=\"init.mp4{q}\"");

	for segment in &snapshot.segments {
		for (index, part) in segment.parts.iter().enumerate() {
			let independent = if part.independent { ",INDEPENDENT=YES" } else { "" };
			let _ = writeln!(
				out,
				"#EXT-X-PART:DURATION={:.5},URI=\"part/{}/{}.m4s{}\"{}",
				part.duration, segment.sequence, index, q, independent
			);
		}
		if segment.complete {
			let _ = writeln!(out, "#EXTINF:{:.5},", segment.duration);
			let _ = writeln!(out, "seg/{}.m4s{}", segment.sequence, q);
		}
	}

	if snapshot.finished {
		let _ = writeln!(out, "#EXT-X-ENDLIST");
	} else {
		// Hint the next part at the live edge so the player can pre-request it.
		let (sequence, index) = match snapshot.segments.last() {
			Some(last) if !last.complete => (last.sequence, last.parts.len()),
			_ => (snapshot.next_sequence, 0),
		};
		let _ = writeln!(
			out,
			"#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part/{sequence}/{index}.m4s{q}\""
		);
	}

	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::export::store::{PartMeta, SegmentMeta};

	fn part(duration: f64, independent: bool) -> PartMeta {
		PartMeta { duration, independent }
	}

	#[test]
	fn renders_ll_hls_tags() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 0.5,
			media_sequence: 10,
			next_sequence: 12,
			segments: vec![
				SegmentMeta {
					sequence: 10,
					parts: vec![part(0.5, true), part(0.5, false)],
					duration: 1.0,
					complete: true,
				},
				SegmentMeta {
					sequence: 11,
					parts: vec![part(0.5, true)],
					duration: 0.5,
					complete: false,
				},
			],
			finished: false,
		};

		let out = render_media(&snapshot, None);

		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:9\n"));
		assert!(out.contains("#EXT-X-TARGETDURATION:1\n"));
		// PART-HOLD-BACK must be >= 3x PART-TARGET.
		assert!(out.contains("PART-HOLD-BACK=1.500"));
		assert!(out.contains("CAN-BLOCK-RELOAD=YES"));
		assert!(out.contains("#EXT-X-PART-INF:PART-TARGET=0.500\n"));
		assert!(out.contains("#EXT-X-MEDIA-SEQUENCE:10\n"));
		assert!(out.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
		// First part of the complete segment is independent; the second is not.
		assert!(out.contains("#EXT-X-PART:DURATION=0.50000,URI=\"part/10/0.m4s\",INDEPENDENT=YES\n"));
		assert!(out.contains("#EXT-X-PART:DURATION=0.50000,URI=\"part/10/1.m4s\"\n"));
		assert!(!out.contains("part/10/1.m4s\",INDEPENDENT"));
		// Completed segment gets an EXTINF + segment URI.
		assert!(out.contains("#EXTINF:1.00000,\nseg/10.m4s\n"));
		// Live edge: preload hint points at the next (not-yet-present) part.
		assert!(out.contains("#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part/11/1.m4s\"\n"));
		assert!(!out.contains("#EXT-X-ENDLIST"));
	}

	#[test]
	fn finished_playlist_has_endlist_and_no_preload() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 1.0,
			media_sequence: 0,
			next_sequence: 1,
			segments: vec![SegmentMeta {
				sequence: 0,
				parts: vec![part(1.0, true)],
				duration: 1.0,
				complete: true,
			}],
			finished: true,
		};

		let out = render_media(&snapshot, None);
		assert!(out.contains("#EXT-X-ENDLIST\n"));
		assert!(!out.contains("#EXT-X-PRELOAD-HINT"));
	}

	#[test]
	fn appends_token_to_uris() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 0.5,
			media_sequence: 0,
			next_sequence: 1,
			segments: vec![SegmentMeta {
				sequence: 0,
				parts: vec![part(0.5, true)],
				duration: 0.5,
				complete: false,
			}],
			finished: false,
		};

		let out = render_media(&snapshot, Some("jwt=abc"));
		// init, parts and the live-edge preload hint all carry the token.
		assert!(out.contains("#EXT-X-MAP:URI=\"init.mp4?jwt=abc\""));
		assert!(out.contains("URI=\"part/0/0.m4s?jwt=abc\""));
		assert!(out.contains("#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part/0/1.m4s?jwt=abc\""));

		// A completed segment carries it on the bare segment line too.
		let finished = Snapshot {
			init_ready: true,
			part_target: 0.5,
			media_sequence: 0,
			next_sequence: 1,
			segments: vec![SegmentMeta {
				sequence: 0,
				parts: vec![part(0.5, true)],
				duration: 0.5,
				complete: true,
			}],
			finished: true,
		};
		let out = render_media(&finished, Some("jwt=abc"));
		assert!(out.contains("\nseg/0.m4s?jwt=abc\n"));
	}
}
