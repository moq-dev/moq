use std::time::Duration;

use anyhow::Context;
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

	/// Leave video out (audio only); native playback opens no window.
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

	/// How far playback may drift from the live edge before skipping groups. TS also runs
	/// this far behind the source (`--delay`).
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

	/// Write the `m2ts` passthrough track back out instead of muxing the media (TS only).
	pub passthrough: bool,

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
	/// Watching for a replacement since before the path resolved. TS follows the path
	/// itself, through `container::ts::Follower`.
	replacement: Option<Replacement>,
}

impl Subscribe {
	/// Resolve the broadcast at `path` on `origin` to export it with the resolved settings;
	/// [`run`](Self::run) drives it.
	pub async fn new(
		origin: moq_net::origin::Consumer,
		path: &str,
		catalog: CatalogFormat,
		args: SubscribeArgs,
	) -> anyhow::Result<Self> {
		let path = moq_net::Path::new(path).to_owned();
		let replacement = match args.format {
			SubscribeFormat::Ts => None,
			_ => Some(Replacement::watch(&origin, &path)?),
		};
		let broadcast = origin
			.request_broadcast(&path, None)
			.await
			.with_context(|| format!("failed to resolve broadcast `{path}`"))?;
		let source = moq_mux::Source::new(origin.clone(), broadcast);
		Ok(Self {
			origin,
			path,
			source,
			catalog,
			args,
			replacement,
		})
	}

	/// Build the catalog stream, narrowed by the rendition selection flags. The
	/// catalog source honors the requested format (e.g. compressed `HangZ` or `Msf`).
	async fn stream(&self) -> anyhow::Result<catalog::Select<catalog::Consumer>> {
		let consumer = self.source.catalog(self.catalog).await?;
		Ok(consumer.select(self.args.selection()?))
	}

	/// Write the broadcast to stdout until it ends.
	pub async fn run(mut self) -> anyhow::Result<()> {
		match self.replacement.take() {
			Some(replacement) => replacement.guard(self.export()).await,
			None => self.export().await,
		}
	}

	async fn export(self) -> anyhow::Result<()> {
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
			.with_delay(self.args.max_delay)
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

		self.origin.routed(&self.path).await.with_context(|| {
			format!(
				"broadcast `{}` is outside the session's scope, or the origin closed before it was announced",
				self.path
			)
		})?;
		if self.args.passthrough {
			return self.run_ts_passthrough().await;
		}

		// TS emits PAT/PMT then a continuous PES stream (re-emitting PAT/PMT at
		// keyframes for tune-in). Avc3/Hev1 sources pass through as Annex-B; AAC
		// is re-framed as ADTS. `fragment_duration` does not apply to TS. The catalog stream
		// selects the `mpegts` catalog extension so undecoded elementary streams
		// (SCTE-35, teletext, DVB AC-3, ...) are re-emitted verbatim on their PIDs.
		let catalog = self.source.catalog::<moq_mux::container::ts::Ext>(self.catalog).await?;
		let mut ts = moq_mux::container::ts::Export::new(self.source, catalog).with_delay(self.args.max_delay);
		if let Some(mux_rate) = self.args.mux_rate {
			ts = ts.with_mux_rate(mux_rate);
		}

		// A TS byte stream carries no per-frame timing, so delivery time is the only
		// carrier of each frame's spacing (#2984). The export lays each slice of the PCR
		// grid out at its time on its own clock, which follows the source's, so each is
		// written as it comes. The path's announcements carry it across the broadcast's
		// returns, as they drive a player.
		let mut ts = moq_mux::container::ts::Follower::new(ts)?
			.with_linger(self.args.linger)
			.with_stitch(self.args.stitch);
		// Reports a track that stops reaching the output while the rest keeps flowing,
		// the way `publish` reports one that stops arriving.
		let mut log = moq_mux::container::ts::stats::Log::default();
		let mut sampled = tokio::time::Instant::now();
		// The export's drop, gap and out-of-tolerance counts, as last logged.
		let mut reported = (0, 0, 0, 0);
		let release = |stats: &moq_mux::container::ts::stats::Export| {
			tracing::info!(
				dropped = stats.dropped,
				trimmed = stats.trimmed,
				gaps = stats.gaps,
				drift_ppm = ?stats.drift,
				out_of_tolerance = stats.out_of_tolerance,
				"TS export release clock"
			);
		};
		let end = loop {
			let frame = match ts.next().await {
				Ok(Some(frame)) => frame,
				Ok(None) => break Ok(()),
				Err(err) => break Err(err),
			};
			stdout.write_all(&frame.payload).await?;
			stdout.flush().await?;

			if sampled.elapsed() >= moq_mux::container::ts::stats::Log::INTERVAL {
				sampled = tokio::time::Instant::now();
				let stats = ts.export().stats();
				let counts = (stats.dropped, stats.trimmed, stats.gaps, stats.out_of_tolerance);
				if counts != reported {
					reported = counts;
					release(&stats);
				}
				log.sample(stats.into());
			}
		};
		release(&ts.export().stats());
		match end {
			Err(err @ moq_mux::Error::Replaced(_)) => {
				Err(anyhow::Error::from(err).context("pass --stitch to follow a replacement as a program switch"))
			}
			end => Ok(end?),
		}
	}

	/// Write the multiplex the broadcast's `m2ts` track carries, each run of packets as its
	/// time on the source's PCR comes round.
	async fn run_ts_passthrough(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();
		let mut release = moq_mux::container::ts::Release::new(self.source, self.catalog)
			.await?
			.with_delay(self.args.max_delay);
		let log = |release: &moq_mux::container::ts::Release| {
			tracing::info!(
				dropped = release.dropped(),
				drift_ppm = ?release.drift().map(|drift| drift * 1e6),
				out_of_tolerance = release.out_of_tolerance(),
				"TS passthrough release clock"
			);
		};
		let mut sampled = tokio::time::Instant::now();
		let mut reported = (0, 0);
		while let Some(object) = release.next().await? {
			stdout.write_all(&object.payload).await?;
			stdout.flush().await?;

			if sampled.elapsed() >= moq_mux::container::ts::stats::Log::INTERVAL {
				sampled = tokio::time::Instant::now();
				let counts = (release.dropped(), release.out_of_tolerance());
				if counts != reported {
					reported = counts;
					log(&release);
				}
			}
		}
		log(&release);
		Ok(())
	}

	async fn run_flv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// FLV emits the file header plus AVC/AAC sequence headers, then one tag per
		// frame interleaved by timestamp. Avc3 sources are transcoded to avc1 shape
		// internally (synthesizing avcC from inline parameter sets). Only H.264 video
		// and AAC audio are supported; `fragment_duration` does not apply to FLV.
		let stream = self.stream().await?;
		let mut flv = moq_mux::container::flv::Export::new(self.source, stream).with_delay(self.args.max_delay);

		while let Some(chunk) = flv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}

/// Watches a path for another publisher instance taking it from the one resolved there.
///
/// Only TS can mark a break in its output, so the other formats end at a replacement.
struct Replacement {
	follow: moq_net::announce::Follow,
	path: moq_net::PathOwned,
}

impl Replacement {
	/// Watch `path` from the route serving it now. Build it before resolving the path, so a
	/// replacement racing the resolution is caught rather than folded into the first start.
	fn watch(origin: &moq_net::origin::Consumer, path: &moq_net::PathOwned) -> anyhow::Result<Self> {
		let mut follow = origin.follow(path)?;
		// The cursor replays the routes on hand at once: take the start naming the one serving
		// the path now, so the next restart marks another instance.
		let _ = follow.poll_next(&moq_net::kio::Waiter::noop());
		Ok(Self {
			follow,
			path: path.clone(),
		})
	}

	/// Run `export` until it ends, failing it if another instance took the path first.
	///
	/// Subscriptions are sticky, so the export rides the old instance out before failing, as
	/// TS does without `--stitch`.
	async fn guard(mut self, export: impl Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
		let replaced = async {
			while let Some(event) = self.follow.next().await {
				if let moq_net::announce::Event::Restart(_) = event {
					return;
				}
			}
			// The origin closed, so nothing can replace the broadcast any more.
			std::future::pending().await
		};

		tokio::pin!(export);
		tokio::select! {
			end = &mut export => return end,
			() = replaced => {}
		}
		let path = &self.path;
		tracing::info!(%path, "broadcast replaced, exporting the old instance until it ends");
		let replaced =
			format!("another publisher instance replaced broadcast `{path}`; only `export ts --stitch` can follow it");
		match export.await {
			Ok(()) => Err(anyhow::anyhow!(replaced)),
			Err(err) => Err(err.context(replaced)),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Announce a broadcast at `room` under `epoch`, returning what keeps it up.
	fn announce(origin: &moq_net::origin::Producer, epoch: &moq_net::Epoch) -> moq_net::broadcast::Producer {
		origin
			.publish("room", moq_net::origin::Route::default().with_epoch(epoch.clone()))
			.unwrap()
	}

	/// Follow `room`, once it is announced, for an export that runs until `end` sends.
	async fn export(
		origin: &moq_net::origin::Producer,
	) -> (
		tokio::sync::oneshot::Sender<()>,
		tokio::task::JoinHandle<anyhow::Result<()>>,
	) {
		let consumer = origin.consume();
		consumer.routed("room").await.unwrap();
		let replacement = Replacement::watch(&consumer, &"room".into()).unwrap();
		let (end, ended) = tokio::sync::oneshot::channel();
		let task = tokio::spawn(replacement.guard(async move {
			ended.await.unwrap();
			Ok(())
		}));
		(end, task)
	}

	/// Another instance taking the path, as every return of an epochless route is, keeps the
	/// export on the old one until it ends, then fails rather than reading as a finish.
	#[tokio::test]
	async fn a_replacement_fails_the_export_once_it_ends() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let _old = announce(&origin, &moq_net::Epoch::mint());
		let (end, task) = export(&origin).await;

		let _new = announce(&origin, &moq_net::Epoch::mint());
		tokio::time::sleep(moq_net::origin::DEFAULT_UPDATE_HOLD * 2).await;
		assert!(!task.is_finished(), "the export left the old instance before it ended");

		end.send(()).unwrap();
		let err = task.await.unwrap().expect_err("a replaced export ended cleanly");
		assert!(err.to_string().contains("replaced broadcast `room`"), "{err:#}");
	}

	/// The same epoch is the same content, so the export carries on.
	#[tokio::test]
	async fn the_same_epoch_does_not_fail_the_export() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let epoch = moq_net::Epoch::mint();
		let _old = announce(&origin, &epoch);
		let (end, task) = export(&origin).await;

		let _new = announce(&origin, &epoch);
		tokio::time::sleep(moq_net::origin::DEFAULT_UPDATE_HOLD * 2).await;

		end.send(()).unwrap();
		task.await.unwrap().unwrap();
	}

	/// A broadcast that goes offline ends the export as it always has, so what returns
	/// after it is no replacement.
	#[tokio::test]
	async fn a_return_after_an_end_is_no_replacement() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let old = announce(&origin, &moq_net::Epoch::mint());
		let (end, task) = export(&origin).await;

		drop(old);
		tokio::time::sleep(moq_net::origin::DEFAULT_UPDATE_HOLD * 2).await;
		let _new = announce(&origin, &moq_net::Epoch::mint());
		tokio::time::sleep(moq_net::origin::DEFAULT_UPDATE_HOLD * 2).await;

		end.send(()).unwrap();
		task.await.unwrap().unwrap();
	}
}
