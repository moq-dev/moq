//! Hand-written MPEG-DASH manifest (MPD) generation.
//!
//! Rendered purely from timeline records, exactly like the HLS playlists: each record is one
//! complete aligned segment carrying its own `pts` and `duration` in the timeline's timescale,
//! which map 1:1 onto a `SegmentTimeline`'s `S@t`/`S@d`. Segments are addressed by `$Time$`
//! (the record's `pts`) rather than `$Number$`: with explicit `t` on every `S`, a rendition
//! that skips a span (a gap) or a client that joins mid-window never mis-addresses a segment
//! the way positional numbering would.
//!
//! URIs are relative to the manifest (`/<broadcast>/manifest.mpd`), so a rendition's
//! `<kind>/<name>/...` resources resolve under the broadcast directory, shared byte-for-byte
//! with the HLS routes (only the time-addressed segment alias `seg/t<pts>.m4s` is DASH's own).

use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::{Duration, SystemTime};

use super::Kind;

/// One DASH representation: master-level metadata plus its slice of the shared timeline.
pub(crate) struct Representation {
	/// Rendition name (the `<name>` in its `<kind>/<name>/...` paths).
	pub name: String,
	/// Whether this is a video or audio rendition (also the URL path component).
	pub kind: Kind,
	/// `@bandwidth`, in bits per second.
	pub bandwidth: u64,
	/// RFC 6381 codec string for `@codecs`.
	pub codec: String,
	/// Coded width for `@width` (video only).
	pub width: Option<u32>,
	/// Coded height for `@height` (video only).
	pub height: Option<u32>,
	/// Frames per second for `@frameRate` (video only).
	pub framerate: Option<f64>,
	/// Samples per second for `@audioSamplingRate` (audio only).
	pub sample_rate: Option<u32>,
	/// Channel count for `AudioChannelConfiguration` (audio only).
	pub channel_count: Option<u32>,
	/// The timeline's timescale (units per second), for `SegmentTemplate@timescale`.
	pub timescale: u32,
	/// `(t, d)` per listed segment in `timescale` units, oldest first: the record's `pts` and
	/// `duration` verbatim. Gap segments (no content for this rendition) and zero-duration
	/// segments (unrequestable: they'd sit exactly at the presentation end) are omitted; the
	/// next entry's explicit `t` conveys the hole.
	pub segments: Vec<(u64, u64)>,
	/// Whether this rendition's window has ended (the broadcast finished). Not rendered;
	/// [`Manifest`] callers use it to pick static vs dynamic.
	pub ended: bool,
}

/// Everything a manifest render needs; built by `Broadcaster::manifest`.
pub(crate) struct Manifest {
	/// The wall-clock time of timeline `pts` 0 (`MPD@availabilityStartTime`). Required while
	/// live (`type="dynamic"`); ignored once [`finished`](Self::finished).
	pub availability_start: Option<SystemTime>,
	/// When this render happened (`MPD@publishTime`, dynamic only).
	pub publish: SystemTime,
	/// The playlist window (`MPD@timeShiftBufferDepth`, dynamic only).
	pub window: Duration,
	/// The broadcast ended: render a `static` presentation instead of a `dynamic` one.
	pub finished: bool,
	/// Video representations, in catalog order.
	pub video: Vec<Representation>,
	/// Audio representations, in catalog order.
	pub audio: Vec<Representation>,
}

/// Escape a string for use inside an XML attribute value.
fn escape(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for c in value.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&apos;"),
			_ => out.push(c),
		}
	}
	out
}

/// An `xs:duration` with millisecond precision (`PT2.000S`).
fn xs_duration(duration: Duration) -> String {
	format!("PT{:.3}S", duration.as_secs_f64())
}

/// `@frameRate` must be an integer or a fraction; approximate a fractional rate over 1000
/// (29.97 -> `29970/1000`).
fn frame_rate(rate: f64) -> Option<String> {
	if !rate.is_finite() || rate <= 0.0 {
		return None;
	}
	if rate.fract() == 0.0 {
		Some(format!("{}", rate as u64))
	} else {
		Some(format!("{}/1000", (rate * 1000.0).round() as u64))
	}
}

/// The largest listed segment duration in whole seconds, for `MPD@maxSegmentDuration` (and the
/// update cadence). Like HLS's target duration, derived from the segments when the publisher
/// declared no bound.
fn max_segment_duration<'a>(representations: impl Iterator<Item = &'a Representation>) -> u64 {
	representations
		.flat_map(|rep| {
			let timescale = rep.timescale.max(1) as u64;
			rep.segments.iter().map(move |(_, d)| d.div_ceil(timescale))
		})
		.max()
		.unwrap_or(0)
		.max(1)
}

fn render_representation(out: &mut String, rep: &Representation, suffix: &str) {
	let kind = rep.kind.as_str();
	let name = escape(&rep.name);

	let mut attrs = format!(
		"id=\"{kind}/{name}\" bandwidth=\"{}\" codecs=\"{}\"",
		rep.bandwidth,
		escape(&rep.codec)
	);
	if let (Some(width), Some(height)) = (rep.width, rep.height) {
		let _ = write!(attrs, " width=\"{width}\" height=\"{height}\"");
	}
	if let Some(rate) = rep.framerate.and_then(frame_rate) {
		let _ = write!(attrs, " frameRate=\"{rate}\"");
	}
	if let Some(sample_rate) = rep.sample_rate {
		let _ = write!(attrs, " audioSamplingRate=\"{sample_rate}\"");
	}
	let _ = writeln!(out, "      <Representation {attrs}>");

	if let Some(channels) = rep.channel_count {
		let _ = writeln!(
			out,
			"        <AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{channels}\"/>"
		);
	}

	let _ = writeln!(
		out,
		"        <SegmentTemplate timescale=\"{}\" initialization=\"{kind}/{name}/init.mp4{suffix}\" media=\"{kind}/{name}/seg/t$Time$.m4s{suffix}\">",
		rep.timescale.max(1)
	);
	let _ = writeln!(out, "          <SegmentTimeline>");
	for (t, d) in &rep.segments {
		// Explicit `t` on every S: durations never accumulate into the address, so a gap (an
		// omitted segment) or timescale rounding can't shift what `$Time$` resolves to.
		let _ = writeln!(out, "            <S t=\"{t}\" d=\"{d}\"/>");
	}
	let _ = writeln!(out, "          </SegmentTimeline>");
	let _ = writeln!(out, "        </SegmentTemplate>");
	let _ = writeln!(out, "      </Representation>");
}

fn render_adaptation_sets(out: &mut String, kind: Kind, representations: &[Representation], suffix: &str) {
	// Representations in one AdaptationSet must be seamlessly switchable, so group by codec
	// (mirroring the HLS master's audio groups).
	let mut codecs = BTreeMap::<&str, Vec<&Representation>>::new();
	for rep in representations {
		if rep.segments.is_empty() {
			// Nothing listable (yet, or a gap-only window): a Representation with an empty
			// SegmentTimeline is unplayable, so leave it out of this render.
			continue;
		}
		codecs.entry(&rep.codec).or_default().push(rep);
	}

	let (content, mime) = match kind {
		Kind::Video => ("video", "video/mp4"),
		Kind::Audio => ("audio", "audio/mp4"),
	};
	for representations in codecs.values() {
		// Segment boundaries come from the broadcast's single timeline, so alignment across
		// renditions is guaranteed by construction; groups open on keyframes.
		let _ = writeln!(
			out,
			"    <AdaptationSet contentType=\"{content}\" mimeType=\"{mime}\" segmentAlignment=\"true\" startWithSAP=\"1\">"
		);
		for rep in representations {
			render_representation(out, rep, suffix);
		}
		let _ = writeln!(out, "    </AdaptationSet>");
	}
}

/// Render the manifest.
///
/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`) appended
/// to every child URL (each representation's init and media templates), so a stock player that
/// does not replay request headers still carries a credential on its follow-up requests.
pub(crate) fn render_manifest(manifest: &Manifest, query: Option<&str>) -> String {
	let suffix = query.map(|q| format!("?{}", escape(q))).unwrap_or_default();
	let representations = || manifest.video.iter().chain(&manifest.audio);
	let target = max_segment_duration(representations());

	let mut out = String::new();
	let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"utf-8\"?>");
	let _ = write!(
		out,
		"<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\""
	);
	if manifest.finished {
		// Presentation time stays anchored at pts 0 in both states (no
		// presentationTimeOffset), so a player that followed the live presentation keeps the
		// same segment-to-time mapping when the manifest turns static; DASH forbids shifting
		// representation timing across MPD updates. The duration therefore spans from pts 0,
		// and a fresh static viewer starts at the earliest listed S@t (players seek to the
		// first timeline entry, not to 0).
		let duration = representations()
			.filter_map(|rep| {
				let (t, d) = rep.segments.last()?;
				let timescale = rep.timescale.max(1) as u64;
				Some(Duration::from_nanos(
					((t + d) as u128 * 1_000_000_000 / timescale as u128) as u64,
				))
			})
			.max()
			.unwrap_or_default();
		let _ = write!(
			out,
			" type=\"static\" mediaPresentationDuration=\"{}\"",
			xs_duration(duration)
		);
	} else {
		let availability = manifest.availability_start.unwrap_or(SystemTime::UNIX_EPOCH);
		// Reload cadence and live delay follow HLS conventions: players refresh about once
		// per segment and sit a few segments behind the live edge (bounded by the window).
		let update = Duration::from_secs(target);
		let delay = Duration::from_secs(3 * target).min(manifest.window.max(update));
		let _ = write!(
			out,
			" type=\"dynamic\" availabilityStartTime=\"{}\" publishTime=\"{}\" minimumUpdatePeriod=\"{}\" timeShiftBufferDepth=\"{}\" suggestedPresentationDelay=\"{}\"",
			humantime::format_rfc3339_millis(availability),
			humantime::format_rfc3339_millis(manifest.publish),
			xs_duration(update),
			xs_duration(manifest.window),
			xs_duration(delay),
		);
	}
	let _ = writeln!(
		out,
		" minBufferTime=\"{}\" maxSegmentDuration=\"{}\">",
		xs_duration(Duration::from_secs(target)),
		xs_duration(Duration::from_secs(target)),
	);

	let _ = writeln!(out, "  <Period id=\"0\" start=\"PT0.000S\">");
	render_adaptation_sets(&mut out, Kind::Video, &manifest.video, &suffix);
	render_adaptation_sets(&mut out, Kind::Audio, &manifest.audio, &suffix);
	let _ = writeln!(out, "  </Period>");
	let _ = writeln!(out, "</MPD>");
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn video(segments: Vec<(u64, u64)>, ended: bool) -> Representation {
		Representation {
			name: "video0".into(),
			kind: Kind::Video,
			bandwidth: 2_500_000,
			codec: "avc1.42c01f".into(),
			width: Some(1280),
			height: Some(720),
			framerate: Some(30.0),
			sample_rate: None,
			channel_count: None,
			timescale: 1000,
			segments,
			ended,
		}
	}

	fn audio(segments: Vec<(u64, u64)>, ended: bool) -> Representation {
		Representation {
			name: "audio0".into(),
			kind: Kind::Audio,
			bandwidth: 128_000,
			codec: "mp4a.40.2".into(),
			width: None,
			height: None,
			framerate: None,
			sample_rate: Some(48_000),
			channel_count: Some(2),
			timescale: 1000,
			segments,
			ended,
		}
	}

	#[test]
	fn renders_dynamic_manifest() {
		let manifest = Manifest {
			availability_start: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_751_846_400_123)),
			publish: SystemTime::UNIX_EPOCH + Duration::from_millis(1_751_846_410_000),
			window: Duration::from_secs(16),
			finished: false,
			video: vec![video(vec![(0, 2_000), (2_000, 2_000)], false)],
			audio: vec![audio(vec![(0, 2_000), (2_000, 2_000)], false)],
		};

		let out = render_manifest(&manifest, None);
		assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n"));
		assert!(out.contains(" type=\"dynamic\""));
		assert!(out.contains(" availabilityStartTime=\"2025-07-07T00:00:00.123Z\""));
		assert!(out.contains(" publishTime=\"2025-07-07T00:00:10.000Z\""));
		assert!(out.contains(" timeShiftBufferDepth=\"PT16.000S\""));
		assert!(out.contains(" minimumUpdatePeriod=\"PT2.000S\""));
		assert!(out.contains(" suggestedPresentationDelay=\"PT6.000S\""));
		assert!(out.contains(" maxSegmentDuration=\"PT2.000S\""));
		assert!(out.contains(
			"<AdaptationSet contentType=\"video\" mimeType=\"video/mp4\" segmentAlignment=\"true\" startWithSAP=\"1\">"
		));
		assert!(out.contains(
			"<Representation id=\"video/video0\" bandwidth=\"2500000\" codecs=\"avc1.42c01f\" width=\"1280\" height=\"720\" frameRate=\"30\">"
		));
		assert!(out.contains(
			"<Representation id=\"audio/audio0\" bandwidth=\"128000\" codecs=\"mp4a.40.2\" audioSamplingRate=\"48000\">"
		));
		assert!(out.contains(
			"<AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"2\"/>"
		));
		assert!(out.contains(
			"<SegmentTemplate timescale=\"1000\" initialization=\"video/video0/init.mp4\" media=\"video/video0/seg/t$Time$.m4s\">"
		));
		assert!(out.contains("<S t=\"0\" d=\"2000\"/>"));
		assert!(out.contains("<S t=\"2000\" d=\"2000\"/>"));
		assert!(!out.contains("mediaPresentationDuration"));
	}

	#[test]
	fn renders_static_manifest_when_finished() {
		let manifest = Manifest {
			availability_start: None,
			publish: SystemTime::UNIX_EPOCH,
			window: Duration::from_secs(16),
			finished: true,
			// The window starts mid-broadcast: presentation time stays anchored at pts 0 (no
			// presentationTimeOffset), so the duration spans the lead-in and a live session
			// keeps its segment-to-time mapping across the live-to-static reload.
			video: vec![video(vec![(10_000, 2_000), (12_000, 2_500)], true)],
			audio: Vec::new(),
		};

		let out = render_manifest(&manifest, None);
		assert!(out.contains(" type=\"static\""));
		assert!(out.contains(" mediaPresentationDuration=\"PT14.500S\""));
		assert!(out.contains(" maxSegmentDuration=\"PT3.000S\""));
		assert!(!out.contains("presentationTimeOffset"));
		assert!(!out.contains("availabilityStartTime"));
		assert!(!out.contains("minimumUpdatePeriod"));
		assert!(!out.contains("timeShiftBufferDepth"));
	}

	#[test]
	fn query_rides_child_urls_escaped() {
		let manifest = Manifest {
			availability_start: Some(SystemTime::UNIX_EPOCH),
			publish: SystemTime::UNIX_EPOCH,
			window: Duration::from_secs(16),
			finished: false,
			video: vec![video(vec![(0, 2_000)], false)],
			audio: Vec::new(),
		};

		let out = render_manifest(&manifest, Some("jwt=abc.def&x=1"));
		assert!(out.contains("initialization=\"video/video0/init.mp4?jwt=abc.def&amp;x=1\""));
		assert!(out.contains("media=\"video/video0/seg/t$Time$.m4s?jwt=abc.def&amp;x=1\""));
	}

	#[test]
	fn fractional_frame_rate_renders_as_fraction() {
		assert_eq!(frame_rate(30.0).as_deref(), Some("30"));
		assert_eq!(frame_rate(29.97).as_deref(), Some("29970/1000"));
		assert_eq!(frame_rate(0.0), None);
	}

	#[test]
	fn empty_and_gap_only_representations_are_omitted() {
		let manifest = Manifest {
			availability_start: Some(SystemTime::UNIX_EPOCH),
			publish: SystemTime::UNIX_EPOCH,
			window: Duration::from_secs(16),
			finished: false,
			video: vec![video(vec![(0, 2_000)], false)],
			audio: vec![audio(Vec::new(), false)],
		};

		let out = render_manifest(&manifest, None);
		assert!(out.contains("id=\"video/video0\""));
		assert!(!out.contains("id=\"audio/audio0\""), "an empty timeline is unplayable");
	}
}
