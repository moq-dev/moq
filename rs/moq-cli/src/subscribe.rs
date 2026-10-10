use std::time::Duration;

use hang::catalog::{AudioCodecKind, VideoCodecKind};
use hang::moq_net;
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

/// Rendition selection flags for the stdout container sinks that honor them and
/// native playback. With no flags set, every rendition is kept.
#[derive(usage::Args, Clone, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct SelectArgs {
	/// Pick the video rendition with this exact name.
	#[usage(long)]
	pub video_name: Option<String>,

	/// Keep only video renditions whose codec family matches.
	#[usage(long, value_enum)]
	pub video_codec: Option<VideoCodecArg>,

	/// Leave video out (audio only).
	#[usage(long, conflicts("--video-name", "--video-codec", "--no-audio"))]
	pub no_video: bool,

	/// Pick the audio rendition with this exact name.
	#[usage(long)]
	pub audio_name: Option<String>,

	/// Keep only audio renditions whose codec family matches.
	#[usage(long, value_enum)]
	pub audio_codec: Option<AudioCodecArg>,

	/// Leave audio out (video only).
	#[usage(long, conflicts("--audio-name", "--audio-codec"))]
	pub no_audio: bool,
}

impl SelectArgs {
	/// Build the rendition selection shared by stdout exports and native playback.
	///
	/// `force` takes the place of `--video-codec`, for a sink whose format implies
	/// one. Pass `None` to use the flag as given.
	pub(crate) fn selection(&self, force: Option<VideoCodecKind>) -> select::Broadcast {
		let mut selection = select::Broadcast::default();

		if !self.no_video {
			let mut video = select::Video::default();
			if let Some(name) = &self.video_name {
				video = video.name(name);
			}
			if let Some(codec) = force.or_else(|| self.video_codec.map(Into::into)) {
				video = video.codec(codec);
			}
			selection = selection.video(video);
		}

		if !self.no_audio {
			let mut audio = select::Audio::default();
			if let Some(name) = &self.audio_name {
				audio = audio.name(name);
			}
			if let Some(codec) = self.audio_codec {
				audio = audio.codec(codec.into());
			}
			selection = selection.audio(audio);
		}

		selection
	}

	/// The first selection flag passed, to name it when a sink can't honor it.
	pub(crate) fn flag(&self) -> Option<&'static str> {
		[
			(self.video_name.is_some(), "--video-name"),
			(self.video_codec.is_some(), "--video-codec"),
			(self.no_video, "--no-video"),
			(self.no_audio, "--no-audio"),
		]
		.into_iter()
		.find_map(|(given, flag)| given.then_some(flag))
		.or_else(|| self.audio_flag())
	}

	/// The first flag passed that selects an audio rendition, for a sink with no audio.
	pub(crate) fn audio_flag(&self) -> Option<&'static str> {
		[
			(self.audio_name.is_some(), "--audio-name"),
			(self.audio_codec.is_some(), "--audio-codec"),
		]
		.into_iter()
		.find_map(|(given, flag)| given.then_some(flag))
	}
}

/// The resolved stdout export settings (built from the `export` flags + format).
#[derive(Clone)]
pub struct SubscribeArgs {
	/// The format to write to stdout.
	pub format: SubscribeFormat,

	/// How far playback may drift from the live edge before skipping groups. TS also
	/// holds every frame this long after its decode time (`--delay`).
	pub max_delay: Duration,

	/// How long to wait for the broadcast to come back after it ends (TS only).
	pub linger: Duration,

	/// Follow another publisher instance replacing the broadcast, as a program switch (TS only).
	pub stitch: bool,

	/// Cap the output duration: publisher groups by default for fMP4, video GOPs for MKV.
	pub fragment_duration: Option<Duration>,

	/// Pad MPEG-TS output with null packets to this rate, in bits per second,
	/// overriding the catalog's recorded multiplex rate.
	pub mux_rate: Option<u64>,

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
	origin: moq_net::origin::Consumer,
	path: moq_net::PathOwned,
	source: moq_mux::Source,
	catalog: CatalogFormat,
	args: SubscribeArgs,
}

impl Subscribe {
	/// Export the broadcast at `path` on `origin` with the resolved settings; [`run`](Self::run)
	/// drives it.
	pub fn new(origin: moq_net::origin::Consumer, path: &str, catalog: CatalogFormat, args: SubscribeArgs) -> Self {
		let path = moq_net::Path::new(path).to_owned();
		let source = moq_mux::Source::new(origin.clone(), &path);
		Self {
			origin,
			path,
			source,
			catalog,
			args,
		}
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
			.with_max_delay(self.args.max_delay)
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
			.with_max_delay(self.args.max_delay)
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
		let mut h264 = moq_mux::codec::h264::Export::new(self.source, stream).with_max_delay(self.args.max_delay);

		while let Some(chunk) = h264.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_h265(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		let stream = self.stream().await?;
		let mut h265 = moq_mux::codec::h265::Export::new(self.source, stream).with_max_delay(self.args.max_delay);

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
		// is re-framed as ADTS. `fragment_duration` does not apply to TS. The export
		// carries the `mpegts` catalog extension so undecoded elementary streams
		// (SCTE-35, teletext, DVB AC-3, ...) are re-emitted verbatim on their PIDs.
		// The path's announcements drive it, as they drive a player.
		let mut ts = moq_mux::container::ts::Follower::new(self.origin, &self.path, self.catalog)
			.await?
			.with_delay(self.args.max_delay)
			.with_linger(self.args.linger)
			.with_stitch(self.args.stitch);
		if let Some(mux_rate) = self.args.mux_rate {
			ts = ts.with_mux_rate(mux_rate);
		}

		// A TS byte stream carries no per-frame timing, so delivery time is the only
		// carrier of each frame's spacing (#2984). The export lays each slice of the PCR
		// grid out at its time on its own clock, which follows the source's, so each is
		// written as it comes.
		// Reports a track that stops reaching the output while the rest keeps flowing,
		// the way `publish` reports one that stops arriving.
		let mut log = moq_mux::container::ts::stats::Log::default();
		let mut sampled = tokio::time::Instant::now();
		// The jitter buffer's late drops and out-of-tolerance count, as last logged.
		let mut reported = (0, 0);
		let release = |stats: &moq_mux::container::ts::stats::Export| {
			tracing::info!(
				dropped = stats.dropped,
				drift_ppm = ?stats.drift,
				out_of_tolerance = stats.out_of_tolerance,
				"TS export release clock"
			);
		};
		loop {
			let frame = match ts.next().await {
				Ok(Some(frame)) => frame,
				Ok(None) => break,
				Err(moq_mux::Error::Replaced(path)) => anyhow::bail!(
					"another publisher instance replaced broadcast `{path}`; pass --stitch to follow it as a program switch"
				),
				Err(err) => return Err(err.into()),
			};
			stdout.write_all(&frame.payload).await?;
			stdout.flush().await?;

			if sampled.elapsed() >= moq_mux::container::ts::stats::Log::INTERVAL {
				sampled = tokio::time::Instant::now();
				let stats = ts.stats();
				if (stats.dropped, stats.out_of_tolerance) != reported {
					reported = (stats.dropped, stats.out_of_tolerance);
					release(&stats);
				}
				log.sample(stats.into());
			}
		}
		release(&ts.stats());
		Ok(())
	}

	async fn run_flv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// FLV emits the file header plus AVC/AAC sequence headers, then one tag per
		// frame interleaved by timestamp. Avc3 sources are transcoded to avc1 shape
		// internally (synthesizing avcC from inline parameter sets). Only H.264 video
		// and AAC audio are supported; `fragment_duration` does not apply to FLV.
		let stream = self.stream().await?;
		let mut flv = moq_mux::container::flv::Export::new(self.source, stream).with_max_delay(self.args.max_delay);

		while let Some(chunk) = flv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}
