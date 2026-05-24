use std::time::Duration;

use clap::ValueEnum;
use hang::catalog::{AudioCodecKind, VideoCodecKind};
use hang::moq_net;
use moq_mux::catalog::{self, CatalogFormat, FilterAudio, FilterVideo, Stream, TargetAudio, TargetVideo};
use tokio::io::AsyncWriteExt;

#[derive(ValueEnum, Clone, Copy)]
pub enum SubscribeFormat {
	Fmp4,
	Mkv,
	/// H.264 Annex-B elementary stream (no container).
	H264,
	/// H.265 Annex-B elementary stream (no container).
	H265,
}

/// `clap` adapter for [`CatalogFormat`] (which is `#[non_exhaustive]` and so
/// can't derive `ValueEnum` itself).
#[derive(ValueEnum, Clone, Copy)]
pub enum CatalogFormatArg {
	Hang,
	Msf,
}

impl From<CatalogFormatArg> for CatalogFormat {
	fn from(format: CatalogFormatArg) -> Self {
		match format {
			CatalogFormatArg::Hang => Self::Hang,
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
}

impl From<AudioCodecArg> for AudioCodecKind {
	fn from(value: AudioCodecArg) -> Self {
		match value {
			AudioCodecArg::Aac => Self::AAC,
			AudioCodecArg::Opus => Self::Opus,
		}
	}
}

#[derive(clap::Args, Clone)]
pub struct SubscribeArgs {
	/// The format to write to stdout.
	#[arg(long)]
	pub format: SubscribeFormat,

	/// Maximum latency before skipping groups (e.g. `500ms`, `1s`).
	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub max_latency: Duration,

	/// Cap the output fragment duration (e.g. `2s`, `500ms`).
	///
	/// By default a fragment covers one GOP (rolled over on video keyframes).
	/// Setting this caps each fragment to roughly the given duration.
	/// The cap applies in addition to GOP rollover.
	#[arg(long, value_parser = humantime::parse_duration)]
	pub fragment_duration: Option<Duration>,

	/// Catalog format to subscribe to for track discovery.
	///
	/// When omitted, the format is auto-detected from the broadcast name suffix
	/// (`.hang` -> hang, `.msf` -> msf), falling back to hang.
	#[arg(long)]
	pub catalog: Option<CatalogFormatArg>,

	/// Pick the video rendition with this exact name.
	#[arg(long)]
	pub video_name: Option<String>,

	/// Keep only video renditions whose codec family matches.
	#[arg(long)]
	pub video_codec: Option<VideoCodecArg>,

	/// Prefer a video rendition no wider than this (px).
	#[arg(long)]
	pub video_width_max: Option<u32>,

	/// Prefer a video rendition no taller than this (px).
	#[arg(long)]
	pub video_height_max: Option<u32>,

	/// Prefer a video rendition with at most this many pixels (`coded_width * coded_height`).
	#[arg(long)]
	pub video_pixels_max: Option<u32>,

	/// Prefer a video rendition under this bitrate (bits per second).
	#[arg(long)]
	pub video_bitrate_max: Option<u64>,

	/// Pick the audio rendition with this exact name.
	#[arg(long)]
	pub audio_name: Option<String>,

	/// Keep only audio renditions whose codec family matches.
	#[arg(long)]
	pub audio_codec: Option<AudioCodecArg>,

	/// Prefer an audio rendition under this bitrate (bits per second).
	#[arg(long)]
	pub audio_bitrate_max: Option<u64>,
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

	/// Codec implied by the output format. `--format h264` / `--format h265`
	/// each force a single codec family; container formats leave it open.
	fn format_codec(&self) -> Option<VideoCodecKind> {
		match self.format {
			SubscribeFormat::H264 => Some(VideoCodecKind::H264),
			SubscribeFormat::H265 => Some(VideoCodecKind::H265),
			SubscribeFormat::Fmp4 | SubscribeFormat::Mkv => None,
		}
	}

	/// Build a video filter from the parsed flags, plus any codec defaulted by
	/// the chosen output format (e.g. `--format h264` implies `codec = H264`).
	///
	/// Errors if `--video-codec` contradicts the format-implied codec — fail
	/// fast in the CLI rather than later in the exporter.
	fn filter_video(&self) -> anyhow::Result<Option<FilterVideo>> {
		let user_codec = self.video_codec.map(VideoCodecKind::from);
		let codec = match (self.format_codec(), user_codec) {
			(Some(fmt), Some(user)) if fmt != user => {
				anyhow::bail!(
					"--format implies video codec {fmt:?}, but --video-codec {user:?} was passed; \
					 remove --video-codec or pick a matching format"
				);
			}
			(Some(fmt), _) => Some(fmt),
			(None, user) => user,
		};
		if self.video_name.is_none() && codec.is_none() {
			return Ok(None);
		}
		Ok(Some(FilterVideo {
			name: self.video_name.clone(),
			codec,
		}))
	}

	fn filter_audio(&self) -> Option<FilterAudio> {
		if self.audio_name.is_none() && self.audio_codec.is_none() {
			return None;
		}
		Some(FilterAudio {
			name: self.audio_name.clone(),
			codec: self.audio_codec.map(Into::into),
		})
	}

	fn target_video(&self) -> Option<TargetVideo> {
		if self.video_width_max.is_none()
			&& self.video_height_max.is_none()
			&& self.video_pixels_max.is_none()
			&& self.video_bitrate_max.is_none()
		{
			return None;
		}
		Some(TargetVideo {
			width: self.video_width_max,
			height: self.video_height_max,
			pixels: self.video_pixels_max,
			bitrate: self.video_bitrate_max,
		})
	}

	fn target_audio(&self) -> Option<TargetAudio> {
		self.audio_bitrate_max.map(|b| TargetAudio { bitrate: Some(b) })
	}
}

pub struct Subscribe {
	broadcast: moq_net::BroadcastConsumer,
	catalog: CatalogFormat,
	args: SubscribeArgs,
}

impl Subscribe {
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: CatalogFormat, args: SubscribeArgs) -> Self {
		Self {
			broadcast,
			catalog,
			args,
		}
	}

	/// Build the catalog stream from the configured filter/target flags.
	fn stream(&self) -> anyhow::Result<catalog::Target<catalog::Filter<catalog::Consumer>>> {
		let consumer = catalog::Consumer::new(&self.broadcast, self.catalog)?;

		let mut filter = consumer.filter();
		filter.set_video(self.args.filter_video()?);
		filter.set_audio(self.args.filter_audio());

		let mut target = filter.target();
		target.set_video(self.args.target_video());
		target.set_audio(self.args.target_audio());

		Ok(target)
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.args.format {
			SubscribeFormat::Fmp4 => self.run_fmp4().await,
			SubscribeFormat::Mkv => self.run_mkv().await,
			SubscribeFormat::H264 => self.run_h264().await,
			SubscribeFormat::H265 => self.run_h265().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		let stream = self.stream()?;
		let mut fmp4 = moq_mux::container::fmp4::Export::new(self.broadcast.clone(), stream)
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

		let stream = self.stream()?;
		let mut mkv = moq_mux::container::mkv::Export::new(self.broadcast.clone(), stream)
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

		let stream = self.stream()?;
		let mut h264 =
			moq_mux::codec::h264::Export::new(self.broadcast.clone(), stream).with_latency(self.args.max_latency);

		while let Some(chunk) = h264.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_h265(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		let stream = self.stream()?;
		let mut h265 =
			moq_mux::codec::h265::Export::new(self.broadcast.clone(), stream).with_latency(self.args.max_latency);

		while let Some(chunk) = h265.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}
