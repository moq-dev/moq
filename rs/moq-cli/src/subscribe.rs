use std::time::Duration;

use clap::ValueEnum;
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

/// `clap` adapter for [`CatalogFormat`] (which is `#[non_exhaustive]` and so
/// can't derive `ValueEnum` itself).
#[derive(ValueEnum, Clone, Copy)]
pub enum CatalogFormatArg {
	Hang,
	#[value(name = "hangz")]
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

/// `clap` adapter for [`VideoCodecKind`].
#[derive(ValueEnum, Clone, Copy)]
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

/// `clap` adapter for [`AudioCodecKind`].
#[derive(ValueEnum, Clone, Copy)]
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
#[derive(clap::Args, Clone, Default)]
pub struct SelectArgs {
	/// Pick the video rendition with this exact name.
	#[arg(long)]
	pub video_name: Option<String>,

	/// Keep only video renditions whose codec family matches.
	#[arg(long)]
	pub video_codec: Option<VideoCodecArg>,

	/// Pick the audio rendition with this exact name.
	#[arg(long)]
	pub audio_name: Option<String>,

	/// Keep only audio renditions whose codec family matches.
	#[arg(long)]
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

	/// Maximum latency before skipping groups.
	pub max_latency: Duration,

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
		let broadcast = self.source.broadcast().await?;
		let consumer = catalog::Consumer::new(&broadcast, self.catalog).await?;
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
			.with_latency(self.args.max_latency)
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
			.with_latency(self.args.max_latency)
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
		let mut h264 = moq_mux::codec::h264::Export::new(self.source, stream).with_latency(self.args.max_latency);

		while let Some(chunk) = h264.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_h265(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		let stream = self.stream().await?;
		let mut h265 = moq_mux::codec::h265::Export::new(self.source, stream).with_latency(self.args.max_latency);

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
			.with_latency(self.args.max_latency);

		// A TS byte stream carries no per-frame timing, so delivery time is the only
		// carrier of each frame's spacing: the exporter emits its PCR grid as
		// standalone frames stamped at their slot boundaries, on the contract that
		// the caller writes the bytes at the time the stamp asserts. Draining on
		// arrival instead collapses the clock into position clusters no downstream
		// stage can repair (#2984). See [`Delivery`] for how the pacing stays
		// bounded; it needs to know whether each frame was waited for, hence the
		// hand-rolled poll instead of `ts.next()`.
		let mut delivery = Delivery::new(self.args.max_latency);
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
			.with_latency(self.args.max_latency);

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
/// frame through a backlog, so pacing resumes from the live edge once the
/// export makes us wait again.
///
/// The epoch is deliberately conservative: a ready frame may in truth have
/// arrived later, during a pacing sleep, but one-frame polling can't observe
/// that, and crediting sleep intervals would let a pre-queued backlog restart
/// the budget on every sleep. The cost is bounded: a hurry can collapse at
/// most the lead-sized interval ahead of the epoch, and smoothing resumes at
/// the next actual wait.
struct Delivery {
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
			lead,
			// tokio's clock rather than the bare std one so tests can pause it; in
			// production they are identical.
			arrived: tokio::time::Instant::now(),
		}
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

		let mut send_at = self.pacer.pace(frame.timestamp, now.into_std());
		if send_at.saturating_duration_since(self.arrived.into_std()) > self.lead {
			send_at = self.pacer.hurry(frame.timestamp, now.into_std());
		}

		tokio::time::sleep_until(tokio::time::Instant::from_std(send_at)).await;
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

	/// A backlog delivered faster than real time (a tune-in group replaying from
	/// its keyframe) must not be slept through step by step: each frame is within
	/// the lead of the previous sleep's end, but total delivery lag versus the
	/// frames' arrival would grow with the backlog's length and then stand for
	/// the pipe's lifetime. Lag is measured against the last wait instead, so the
	/// drain hurries once it overshoots the latency budget.
	#[tokio::test(start_paused = true)]
	async fn backlog_lag_is_bounded_by_the_latency_budget() {
		let mut delivery = Delivery::new(Duration::from_millis(500));
		let mut out = Vec::new();

		let start = tokio::time::Instant::now();

		// Frames spanning 1200ms of media, all available at once (waited = false
		// after the first): pacing may hold each for at most the 500ms budget
		// past the last wait, not restart the budget after every sleep.
		delivery
			.deliver(&frame(0, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();
		delivery
			.deliver(&frame(400_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		assert_eq!(start.elapsed(), Duration::from_millis(400), "within the budget: paced");

		delivery
			.deliver(&frame(800_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		delivery
			.deliver(&frame(1_200_000, Timescale::MICRO), false, &mut out)
			.await
			.unwrap();
		assert_eq!(
			start.elapsed(),
			Duration::from_millis(400),
			"past the budget: the drain hurries instead of sleeping"
		);

		// The hurry made the newest frame the live edge, so pacing resumes
		// relative to it once the export makes us wait again.
		delivery
			.deliver(&frame(1_240_000, Timescale::MICRO), true, &mut out)
			.await
			.unwrap();
		assert_eq!(start.elapsed(), Duration::from_millis(440));
	}
}
