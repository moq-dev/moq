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

	/// How far playback may drift from the live edge before skipping groups. TS also
	/// holds every frame this long after its decode time (`--delay`).
	pub max_age: Duration,

	/// How long to wait for the broadcast to come back after it ends (TS only).
	pub linger: Duration,

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
		let source = self.source.clone();
		let mut broadcast = source.broadcast().await?;
		let mut ts = moq_mux::container::ts::Export::with_ts(self.source, self.catalog)
			.await?
			.with_delay(self.args.max_age);
		if let Some(mux_rate) = self.args.mux_rate {
			ts = ts.with_mux_rate(mux_rate);
		}

		// A TS byte stream carries no per-frame timing, so delivery time is the only
		// carrier of each frame's spacing (#2984). The export lays each slice of the PCR
		// grid out at its time on its own clock, which follows the source's, so each is
		// written as it comes.
		let linger = self.args.linger;
		loop {
			let end = loop {
				let frame = match ts.next().await {
					Ok(Some(frame)) => frame,
					Ok(None) => break Ok(()),
					Err(err) => break Err(err),
				};
				stdout.write_all(&frame.payload).await?;
				stdout.flush().await?;
			};

			// Any end waits out the linger, and on expiry the last one is the result: a
			// clean catalog finish exits 0, a drop or any other failure exits 1.
			if linger.is_zero() {
				return Ok(end?);
			}
			match &end {
				Ok(()) => tracing::info!(?linger, "broadcast finished, waiting for it to return"),
				Err(err) => tracing::warn!(%err, ?linger, "broadcast ended, waiting for it to return"),
			}
			let Some(returned) = resume_within(&source, &broadcast, &mut ts, linger).await? else {
				tracing::info!(?linger, "broadcast did not return");
				return Ok(end?);
			};
			broadcast = returned;
			tracing::info!("broadcast returned, resuming");
		}
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

/// Wait up to `linger` for the `ended` broadcast to return and `ts` to resume on it.
///
/// The linger bounds the whole return, catalog subscription included: a returned
/// broadcast whose catalog never resolves must not hold the export past it. `None`
/// when it did not return in time.
async fn resume_within(
	source: &moq_mux::Source,
	ended: &hang::moq_net::broadcast::Consumer,
	ts: &mut moq_mux::container::ts::Export<moq_mux::container::ts::Ext>,
	linger: Duration,
) -> anyhow::Result<Option<hang::moq_net::broadcast::Consumer>> {
	let resume = async {
		let returned = source.returned(ended).await?;
		ts.resume().await?;
		anyhow::Ok(returned)
	};
	match tokio::time::timeout(linger, resume).await {
		Ok(returned) => Ok(Some(returned?)),
		Err(_) => Ok(None),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A broadcast that returns but never serves its catalog gives up at the linger,
	/// rather than waiting on the catalog past it.
	#[tokio::test(start_paused = true)]
	async fn a_return_without_a_catalog_expires_with_the_linger() {
		let (origin, driver) = hang::moq_net::origin::Producer::new(Default::default());
		tokio::spawn(hang::moq_net::time::run(driver));
		let source = moq_mux::Source::new(origin.consume(), "live");

		let mut first = origin.publish("live", Default::default()).unwrap();
		let catalog = moq_mux::catalog::Producer::new(&mut first, Default::default()).unwrap();
		let ended = source.broadcast().await.unwrap();
		let mut ts = moq_mux::container::ts::Export::with_ts(source.clone(), CatalogFormat::Hang)
			.await
			.unwrap();
		drop((first, catalog));

		// Back, but its catalog request is never answered.
		let second = origin.publish("live", Default::default()).unwrap();
		let _unanswered = second.dynamic();

		let linger = Duration::from_secs(10);
		let start = tokio::time::Instant::now();
		let resumed = resume_within(&source, &ended, &mut ts, linger).await.unwrap();
		assert!(resumed.is_none(), "a return that never resumes is no return");
		assert_eq!(start.elapsed(), linger);
	}
}
