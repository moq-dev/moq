use std::time::Duration;

use hang::catalog::{AudioCodecKind, VideoCodecKind};
use moq_mux::catalog::{self, CatalogFormat, Stream};
use moq_mux::select;
use tokio::io::AsyncWriteExt;

/// Container format written to stdout on the export (sink) side.
#[derive(Clone, Copy)]
pub enum SubscribeFormat {
	/// Fragmented MP4 (CMAF).
	Fmp4,
	/// Matroska / WebM.
	Mkv,
	/// H.264 Annex-B elementary stream (no container).
	H264,
	/// H.265 Annex-B elementary stream (no container).
	H265,
	/// MPEG-TS (transport stream).
	Ts,
	/// FLV (Flash Video / RTMP).
	Flv,
}

/// `Usage` adapter for [`CatalogFormat`] (which is `#[non_exhaustive]` and so
/// can't derive `ValueEnum` itself).
#[derive(usage::ValueEnum, Clone, Copy)]
pub enum CatalogFormatArg {
	Hang,
	#[usage(name = "hangz")]
	HangZ,
	Msf,
}

impl From<CatalogFormatArg> for CatalogFormat {
	fn from(format: CatalogFormatArg) -> Self {
		match format {
			CatalogFormatArg::Hang => Self::Hang,
			CatalogFormatArg::HangZ => Self::HangZ,
			CatalogFormatArg::Msf => Self::Msf,
		}
	}
}

/// `Usage` adapter for [`VideoCodecKind`].
#[derive(usage::ValueEnum, Clone, Copy)]
pub enum VideoCodecArg {
	H264,
	H265,
	Vp8,
	Vp9,
	Av1,
}

impl From<VideoCodecArg> for VideoCodecKind {
	fn from(value: VideoCodecArg) -> Self {
		match value {
			VideoCodecArg::H264 => Self::H264,
			VideoCodecArg::H265 => Self::H265,
			VideoCodecArg::Vp8 => Self::VP8,
			VideoCodecArg::Vp9 => Self::VP9,
			VideoCodecArg::Av1 => Self::AV1,
		}
	}
}

/// `Usage` adapter for [`AudioCodecKind`].
#[derive(usage::ValueEnum, Clone, Copy)]
pub enum AudioCodecArg {
	Aac,
	Opus,
	Pcm,
}

impl From<AudioCodecArg> for AudioCodecKind {
	fn from(value: AudioCodecArg) -> Self {
		match value {
			AudioCodecArg::Aac => Self::AAC,
			AudioCodecArg::Opus => Self::Opus,
			AudioCodecArg::Pcm => Self::Pcm,
		}
	}
}

/// Rendition selection flags for stdout container sinks and native playback.
/// With no flags set, every rendition is kept.
#[derive(usage::Args, Clone, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct SelectArgs {
	/// Pick the video rendition with this exact name.
	#[usage(long)]
	pub video_name: Option<String>,

	/// Keep only video renditions whose codec family matches.
	#[usage(long, value_enum)]
	pub video_codec: Option<VideoCodecArg>,

	/// Pick the audio rendition with this exact name.
	#[usage(long)]
	pub audio_name: Option<String>,

	/// Keep only audio renditions whose codec family matches.
	#[usage(long, value_enum)]
	pub audio_codec: Option<AudioCodecArg>,
}

impl SelectArgs {
	/// Build the rendition selection shared by stdout exports and native playback.
	///
	/// `force` takes the place of `--video-codec`, for a sink whose format implies
	/// one. Pass `None` to use the flag as given.
	pub(crate) fn selection(&self, force: Option<VideoCodecKind>) -> select::Broadcast {
		let mut video = select::Video::default();
		if let Some(name) = &self.video_name {
			video = video.name(name);
		}
		if let Some(codec) = force.or_else(|| self.video_codec.map(Into::into)) {
			video = video.codec(codec);
		}

		let mut audio = select::Audio::default();
		if let Some(name) = &self.audio_name {
			audio = audio.name(name);
		}
		if let Some(codec) = self.audio_codec {
			audio = audio.codec(codec.into());
		}

		select::Broadcast::default().video(video).audio(audio)
	}
}

/// The resolved stdout export settings (built from the `export` flags + format).
#[derive(Clone)]
pub struct SubscribeArgs {
	/// The format to write to stdout.
	pub format: SubscribeFormat,

	/// How far playback may drift from the live edge before skipping groups.
	pub max_age: Duration,

	/// Cap the output fragment duration (default: one GOP). Applies to fmp4 / mkv.
	pub fragment_duration: Option<Duration>,

	/// Catalog format for track discovery (default: detect from the broadcast suffix).
	pub catalog: Option<CatalogFormatArg>,

	/// Rendition selection (name / codec) applied before export.
	pub select: SelectArgs,
}

impl SubscribeArgs {
	/// Resolve the catalog format, falling back to detection from the broadcast
	/// name suffix and then to the default.
	pub fn catalog_format(&self, broadcast: &str) -> CatalogFormat {
		self.catalog
			.map(Into::into)
			.or_else(|| CatalogFormat::detect(broadcast))
			.unwrap_or_default()
	}

	/// Codec implied by the output format. The `h264` / `h265` sinks each force
	/// a single codec family; container formats leave it open.
	fn format_codec(&self) -> Option<VideoCodecKind> {
		match self.format {
			SubscribeFormat::H264 => Some(VideoCodecKind::H264),
			SubscribeFormat::H265 => Some(VideoCodecKind::H265),
			SubscribeFormat::Fmp4 | SubscribeFormat::Mkv | SubscribeFormat::Ts | SubscribeFormat::Flv => None,
		}
	}

	/// Build the rendition selection from the flags, plus any codec forced by
	/// the output format (the `h264` sink implies `codec = H264`).
	///
	/// Errors if `--video-codec` contradicts the format-implied codec, failing
	/// fast in the CLI rather than later in the exporter.
	fn selection(&self) -> anyhow::Result<select::Broadcast> {
		let user_codec = self.select.video_codec.map(VideoCodecKind::from);
		let codec = match (self.format_codec(), user_codec) {
			(Some(fmt), Some(user)) if fmt != user => {
				anyhow::bail!(
					"the output format implies video codec {fmt:?}, but --video-codec {user:?} was passed; \
					 remove --video-codec or pick a matching format"
				);
			}
			(Some(fmt), _) => Some(fmt),
			(None, user) => user,
		};

		Ok(self.select.selection(codec))
	}
}

/// Exports one broadcast from the Origin to stdout in the requested format.
pub struct Subscribe {
	source: moq_mux::Source,
	catalog: CatalogFormat,
	args: SubscribeArgs,
}

impl Subscribe {
	/// Wrap the broadcast + resolved settings; [`run`](Self::run) drives it.
	pub fn new(source: moq_mux::Source, catalog: CatalogFormat, args: SubscribeArgs) -> Self {
		Self { source, catalog, args }
	}

	/// Build the catalog stream, narrowed by the rendition selection flags. The
	/// catalog source honors the requested format (e.g. compressed `HangZ` or `Msf`).
	async fn stream(&self) -> anyhow::Result<catalog::Select<catalog::Consumer>> {
		let consumer = self.source.catalog(self.catalog).await?;
		Ok(consumer.select(self.args.selection()?))
	}

	/// Write the broadcast to stdout until it ends.
	pub async fn run(self) -> anyhow::Result<()> {
		match self.args.format {
			SubscribeFormat::Fmp4 => self.run_fmp4().await,
			SubscribeFormat::Mkv => self.run_mkv().await,
			SubscribeFormat::H264 => self.run_h264().await,
			SubscribeFormat::H265 => self.run_h265().await,
			SubscribeFormat::Ts => self.run_ts().await,
			SubscribeFormat::Flv => self.run_flv().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// Fmp4 builds the merged init segment from the first catalog snapshot, then
		// yields moof+mdat fragments in timestamp order across tracks.
		let stream = self.stream().await?;
		let mut fmp4 = moq_mux::container::fmp4::Export::new(self.source, stream)
			.with_max_age(self.args.max_age)
			.with_fragment_duration(self.args.fragment_duration);

		while let Some(chunk) = fmp4.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_mkv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// Mkv writes EBML + an unknown-size Segment header, then per-fragment
		// Cluster elements. Avc3/Hev1 sources are transcoded to avc1/hvc1
		// shape internally (synthesizing avcC/hvcC from inline parameter sets).
		let stream = self.stream().await?;
		let mut mkv = moq_mux::container::mkv::Export::new(self.source, stream)
			.with_max_age(self.args.max_age)
			.with_fragment_duration(self.args.fragment_duration);

		while let Some(chunk) = mkv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_h264(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		let stream = self.stream().await?;
		let mut h264 = moq_mux::codec::h264::Export::new(self.source, stream).with_max_age(self.args.max_age);

		while let Some(chunk) = h264.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_h265(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		let stream = self.stream().await?;
		let mut h265 = moq_mux::codec::h265::Export::new(self.source, stream).with_max_age(self.args.max_age);

		while let Some(chunk) = h265.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_ts(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// TS emits PAT/PMT then a continuous PES stream (re-emitting PAT/PMT at
		// keyframes for tune-in). Avc3/Hev1 sources pass through as Annex-B; AAC
		// is re-framed as ADTS. `fragment_duration` does not apply to TS. `with_ts`
		// selects the `mpegts` catalog extension so undecoded elementary streams
		// (SCTE-35, teletext, DVB AC-3, ...) are re-emitted verbatim on their PIDs.
		let mut ts = moq_mux::container::ts::Export::with_ts(self.source, self.catalog)
			.await?
			.with_max_age(self.args.max_age);

		// A TS byte stream carries no per-frame timing, so delivery time is the only
		// carrier of each frame's spacing: the exporter slices its output on the PCR
		// grid and stamps each slice at its slot boundary, on the contract that the
		// caller writes the bytes at the time the stamp asserts. Draining on arrival
		// instead collapses the clock into position clusters no downstream stage can
		// repair (#2984). See [`Delivery`] for how the pacing stays
		// bounded; it needs to know whether each frame was waited for, hence the
		// hand-rolled poll instead of `ts.next()`.
		let mut delivery = Delivery::new(self.args.max_age);
		loop {
			let mut waited = false;
			let frame = hang::moq_net::kio::wait(|waiter| match ts.poll_next(waiter) {
				std::task::Poll::Pending => {
					waited = true;
					std::task::Poll::Pending
				}
				ready => ready,
			})
			.await?;

			let Some(frame) = frame else { break };
			delivery.update(&frame, ts.discontinuity());
			delivery.deliver(&frame, waited, &mut stdout).await?;
		}

		Ok(())
	}

	async fn run_flv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// FLV emits the file header plus AVC/AAC sequence headers, then one tag per
		// frame interleaved by timestamp. Avc3 sources are transcoded to avc1 shape
		// internally (synthesizing avcC from inline parameter sets). Only H.264 video
		// and AAC audio are supported; `fragment_duration` does not apply to FLV.
		let mut flv = moq_mux::container::flv::Export::with_catalog_format(self.source, self.catalog)
			.await?
			.with_max_age(self.args.max_age);

		while let Some(chunk) = flv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}

/// Paced stdout delivery for the TS export: sleeps until each frame's send
/// instant, with total delivery lag bounded by the latency budget.
///
/// The bound is the subtle part. The pacer alone caps how far one frame may be
/// scheduled past the `now` it paces with, but our own sleeps push that `now`
/// forward, so a backlog arriving faster than real time (a tune-in group
/// replaying from its keyframe, a catch-up after a stall) stays within the lead
/// of every individual call while total delivery lag grows without bound. The
/// export's group skipping can't shed that lag either: it fires when the
/// *current group* is blocked with a newer alternative, so it measures producer
/// stalls, never consumer lag. So lag is measured here instead, against
/// `arrived`: the last instant the export made us wait, which is when a frame
/// obtained without waiting could first have been queued. When a frame's
/// schedule overshoots that epoch by more than the lead, it is delivered
/// immediately and becomes the live edge
/// ([`Pacer::hurry`](moq_mux::Pacer::hurry)). The anchor then rides the newest
/// frame through a backlog, so pacing resumes from the live edge.
///
/// A hurry moves the epoch with it, because the frame it delivers *is* the live
/// edge and the distance it is measured from has to be too. `hurry` returns `now`,
/// so the credit below can never fire on the frame that hurried; leaving the epoch
/// behind would put every later frame the same overshoot past it and shed the whole
/// stream at the arrival cadence, which the buffered export would never correct
/// since it never makes us wait. A sink that is genuinely slow still sheds on every
/// stall: each one leaves the next frame far enough past the epoch to overshoot
/// again.
///
/// Reaching a scheduled instant also advances the epoch, and has to. The TS export
/// holds a mux buffer: it always has the next grid slot ready, so it stops making
/// us wait, and an epoch that only moved on a wait would freeze for good and take
/// the budget with it (measured: a hurry roughly every second, each shedding the
/// pacing it was there to protect). Sleeping to the schedule is the proof that
/// replaces the wait, since nothing the export queued while we slept could have
/// gone out any earlier. What that gives up is a producer running faster than real
/// time indefinitely: the sink keeps pace with it and falls further behind live
/// without the budget noticing. Bounding *that* is the export's own
/// `--max-age`, which sheds media rather than compressing the clock.
///
/// The budget is the lead plus whatever standing lag the pacer has absorbed
/// ([`Pacer::slack`](moq_mux::Pacer::slack)), which is a distance it is holding on
/// purpose rather than lag to shed.
struct Delivery {
	discontinuity: u64,
	pacer: moq_mux::Pacer,
	/// The delivery-lag bound, and the pacer's lead: both are the export's
	/// latency budget.
	lead: Duration,
	/// The last instant the export made us wait for a frame: the conservative
	/// arrival epoch for frames obtained without waiting (see the type docs).
	arrived: tokio::time::Instant,
}

impl Delivery {
	fn new(lead: Duration) -> Self {
		Self {
			pacer: moq_mux::Pacer::default().with_lead(lead),
			discontinuity: 0,
			lead,
			// tokio's clock rather than the bare std one so tests can pause it; in
			// production they are identical.
			arrived: tokio::time::Instant::now(),
		}
	}

	fn update(&mut self, frame: &moq_mux::container::Frame, discontinuity: u64) {
		if discontinuity == self.discontinuity {
			return;
		}
		self.discontinuity = discontinuity;
		self.arrived = tokio::time::Instant::now();
		self.pacer.hurry(frame.timestamp, self.arrived.into_std());
	}

	/// Write one export frame to `out` at its paced instant. `waited` is whether
	/// the export made us wait for this frame rather than having it ready.
	async fn deliver(
		&mut self,
		frame: &moq_mux::container::Frame,
		waited: bool,
		out: &mut (impl tokio::io::AsyncWrite + Unpin),
	) -> anyhow::Result<()> {
		let now = tokio::time::Instant::now();
		if waited {
			self.arrived = now;
		}

		// The pacer's own slack is not lag: it is the producer's standing delivery
		// distance, which the pacer discovered and is holding on purpose. Counting
		// it here would shed the margin on a fixed cadence and put the writes back
		// on the arrival clock, which is the whole thing this is here to avoid.
		let budget = self.lead + self.pacer.slack();
		let mut send_at = self.pacer.pace(frame.timestamp, now.into_std());
		if send_at.saturating_duration_since(self.arrived.into_std()) > budget {
			send_at = self.pacer.hurry(frame.timestamp, now.into_std());
			// A hurry makes this frame the live edge, so the epoch it is measured
			// against becomes now as well. `hurry` returns `now`, so the credit below
			// can't do it, and a stale epoch would overshoot the budget on every
			// later frame, latching the shed on for good.
			self.arrived = now;
		}

		let send_at = tokio::time::Instant::from_std(send_at);
		tokio::time::sleep_until(send_at).await;
		// Reaching a scheduled instant is proof we are not behind, and it is the only
		// such proof once the export holds a buffer: it always has the next slot
		// ready, so it stops making us wait and the arrival epoch would freeze for
		// good, taking the budget with it. Credit the scheduled instant rather than
		// `now`, so an overshoot isn't credited as headroom.
		if send_at > now {
			self.arrived = send_at;
		}
		out.write_all(&frame.payload).await?;
		out.flush().await?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use hang::moq_net::{Timescale, Timestamp};

	fn frame(value: u64, scale: Timescale) -> moq_mux::container::Frame {
		moq_mux::container::Frame {
			timestamp: Timestamp::new(value, scale).unwrap(),
			duration: None,
			payload: bytes::Bytes::from_static(&[0x47; 188]),
			keyframe: false,
		}
	}

	/// Regression for #2984: the TS stdout writer must deliver each frame at the
	/// instant its timestamp asserts, not as fast as frames arrive. The exporter
	/// stamps PCR grid frames in microseconds and media frames at the source's own
	/// timescale, so the spacing must also survive a scale change mid-stream.
	#[tokio::test(start_paused = true)]
	async fn ts_frames_are_paced_on_the_media_clock() {
		let mut delivery = Delivery::new(Duration::from_millis(500));
		let mut out = Vec::new();

		let start = tokio::time::Instant::now();

		// The first frame anchors the pacer and is written immediately.
		delivery
			.deliver(&frame(0, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();
		assert_eq!(start.elapsed(), Duration::ZERO);

		// A PCR slot 25ms later (ready without waiting, like a backfilled grid
		// slot) waits for its grid boundary.
		delivery
			.deliver(&frame(25_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		assert_eq!(start.elapsed(), Duration::from_millis(25));

		// A media frame at the source's 90 kHz timescale paces on the same clock.
		delivery
			.deliver(&frame(3_600, Timescale::new(90_000).unwrap()), true, &mut out)
			.await
			.unwrap();
		assert_eq!(start.elapsed(), Duration::from_millis(40));

		assert_eq!(out.len(), 3 * 188, "every payload was written");
	}

	/// A sink that cannot reach its schedule still sheds the lag, which is the case
	/// the budget is left guarding.
	///
	/// The epoch advances on a wait, on reaching a scheduled instant, or on a hurry.
	/// A sink falling behind reaches none of its instants and is never made to wait,
	/// so only the hurry moves it, and each fresh stall puts the next frame far
	/// enough past that epoch to overshoot the budget and hurry again.
	///
	/// What this no longer covers, deliberately, is a one-off pre-queued backlog
	/// (a tune-in group replaying from its keyframe). That now paces out at the
	/// media rate instead of being shed, because it is indistinguishable from the
	/// TS export's own mux buffer from here: both hand over a frame that is ready
	/// and ahead of the schedule. The size of such a backlog is bounded by the
	/// export's `--max-age`, which is where it belongs.
	#[tokio::test(start_paused = true)]
	async fn a_sink_that_cannot_keep_up_sheds_the_lag() {
		let mut delivery = Delivery::new(Duration::from_millis(500));
		let mut out = Vec::new();

		let start = tokio::time::Instant::now();
		delivery
			.deliver(&frame(0, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();

		// The writer stalls for 2s (a blocked pipe): wall clock runs on, the media
		// clock does not, and every frame after this is overdue on arrival.
		tokio::time::advance(Duration::from_secs(2)).await;

		delivery
			.deliver(&frame(400_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		assert_eq!(
			start.elapsed(),
			Duration::from_secs(2),
			"an overdue frame writes at once: the overshoot hurries and makes it the live edge"
		);

		// Shedding happens at the re-anchor, once, not on every frame after it. This
		// one is 200ms of media past the new edge and 200ms past the epoch the hurry
		// set with it, so it is inside the budget and paces.
		delivery
			.deliver(&frame(600_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		assert_eq!(start.elapsed(), Duration::from_secs(2) + Duration::from_millis(200));

		// A sink that goes on stalling goes on shedding, which is the property this
		// test is here for: each stall leaves the next frame far enough past the
		// epoch to overshoot the budget again, however recently the last hurry moved
		// it. Media steps 25ms per iteration while the wall clock steps 2s.
		for slot in 1..=3u64 {
			tokio::time::advance(Duration::from_secs(2)).await;
			let before = start.elapsed();
			delivery
				.deliver(&frame(600_000 + slot * 25_000, Timescale::MICRO), false, &mut out)
				.await
				.unwrap();
			assert_eq!(start.elapsed(), before, "stall {slot} must shed, not pace");
		}
	}

	/// A hurry has to move the arrival epoch with it, or it latches.
	///
	/// `hurry` returns `now`, so the credit for reaching a scheduled instant can
	/// never fire on the frame that hurried. Left there, the epoch stays wherever it
	/// was before the shed while the schedule walks forward from the new edge, so the
	/// next frame overshoots the budget too, and so does every one after it. The
	/// buffered export never waits, so nothing else would move the epoch back.
	#[tokio::test(start_paused = true)]
	async fn pacing_resumes_after_a_hurry_without_a_wait() {
		let mut delivery = Delivery::new(Duration::from_millis(500));
		let mut out = Vec::new();

		let start = tokio::time::Instant::now();
		delivery
			.deliver(&frame(0, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();

		// Stall long enough that the schedule outruns the budget and sheds.
		tokio::time::advance(Duration::from_secs(2)).await;
		delivery
			.deliver(&frame(400_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		let hurried = start.elapsed();
		assert_eq!(hurried, Duration::from_secs(2), "the overshoot sheds");

		// Grid slots from the new edge, every one already queued and none waited on.
		// Against a stale epoch each is a whole stall past it, so each would hurry and
		// write at once, pinning `start.elapsed()` at `hurried` for the whole loop.
		for slot in 1..=8u64 {
			delivery
				.deliver(&frame(400_000 + slot * 25_000, Timescale::MICRO), false, &mut out)
				.await
				.unwrap();
			assert_eq!(
				start.elapsed(),
				hurried + Duration::from_millis(slot * 25),
				"slot {slot} must be paced, not shed"
			);
		}
	}

	/// The TS export holds a mux buffer, so it always has the next grid slot ready
	/// and stops making the sink wait. Reaching a scheduled instant has to advance
	/// the epoch too, or the budget freezes and hurries on a fixed cadence, shedding
	/// the pacing it exists to protect.
	#[tokio::test(start_paused = true)]
	async fn a_buffered_producer_keeps_pacing() {
		let mut delivery = Delivery::new(Duration::from_millis(500));
		let mut out = Vec::new();

		let start = tokio::time::Instant::now();
		delivery
			.deliver(&frame(0, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();

		// A grid slot every 25ms, each already queued: an epoch that only moved on a
		// wait would freeze here and hurry once the schedule passed 500ms.
		for slot in 1..=40u64 {
			delivery
				.deliver(&frame(slot * 25_000, Timescale::MICRO), false, &mut out)
				.await
				.unwrap();
			assert_eq!(
				start.elapsed(),
				Duration::from_millis(slot * 25),
				"slot {slot} must be paced, not shed"
			);
		}
	}
	#[tokio::test(start_paused = true)]
	async fn a_rewind_re_anchors_the_pacer() {
		let mut delivery = Delivery::new(Duration::from_millis(500));
		let mut out = tokio::io::sink();
		delivery
			.deliver(&frame(10_000_000, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();
		let first = frame(0, Timescale::MICRO);
		let now = tokio::time::Instant::now();
		delivery.update(&first, 1);
		delivery.deliver(&first, false, &mut out).await.unwrap();
		assert_eq!(now.elapsed(), Duration::ZERO);
		let next = frame(40_000, Timescale::MICRO);
		delivery.update(&next, 1);
		delivery.deliver(&next, false, &mut out).await.unwrap();
		assert_eq!(now.elapsed(), Duration::from_millis(40));
	}
}
