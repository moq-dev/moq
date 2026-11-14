//! Transcoder configuration: the rung ladder and catalog wiring.

use std::num::NonZeroUsize;
use std::sync::Arc;

use moq_net::path::Relative;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::Ladder;

/// Transcoder configuration for [`run`](crate::run).
///
/// `#[non_exhaustive]`: build via `Config::default()` and set fields, so future
/// knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Candidate output renditions, lowest first. Only rungs strictly below the
	/// source survive: a rung is dropped when its short side exceeds the source, when
	/// its bitrate is not below the source bitrate (when known), or when it
	/// matches the source short side without a known source bitrate to undercut. A
	/// 480p source is never transcoded up to 720p.
	///
	/// Filtering drops rungs but never reorders them, so the surviving ladder is
	/// still ascending. Build it with [`Ladder::new`](crate::Ladder::new), which
	/// takes the rungs in any order and refuses an ambiguous ladder.
	pub ladder: Ladder,

	/// Where the source broadcast lives relative to the output broadcast, e.g.
	/// `"."` when the output is published at `<source>/transcode.hang`. When
	/// set, the derivative catalog references the source renditions (all video
	/// and audio) through this path so players fetch them from the source
	/// directly; the transcoder never proxies or subscribes them. `None` omits
	/// them from the derivative catalog.
	pub source: Option<Relative>,

	/// Which video encoder implementation encodes the rungs. The default
	/// prefers hardware (NVENC on Linux, VideoToolbox on macOS, Media
	/// Foundation on Windows) and falls back to OpenH264 when enabled.
	pub encoder: moq_video::encode::Kind,

	/// Which video decoder implementation decodes the source. The default
	/// prefers hardware and falls back to OpenH264 when enabled (H.264 only;
	/// H.265 sources need a hardware decoder).
	pub decoder: moq_video::decode::Kind,

	/// Where decoded and resized frames live. Native output keeps GPU-backed
	/// frames on the GPU from decode through encode; CPU output downloads at
	/// the decoder and scales on the CPU.
	pub resize: moq_video::resize::Config,

	/// How many encoder sessions may be open at once, shared by every transcoder whose
	/// config holds a clone: each rung's live encoder, each fetched group's, and each catalog
	/// probe. A hardware encoder refuses sessions past a device limit, which a device shared
	/// with other work reaches sooner, so past the budget an encoder waits for one to close
	/// instead. The default sets no budget.
	pub sessions: Sessions,

	/// The VA-API render node the decoder and encoders open; see
	/// [`encode::Config::vaapi`](moq_video::encode::Config::vaapi).
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	pub vaapi: Option<std::path::PathBuf>,

	/// The V4L2 M2M node the decoder opens; see
	/// [`decode::Config::v4l2`](moq_video::decode::Config::v4l2).
	#[cfg(all(target_os = "linux", feature = "v4l2"))]
	pub v4l2_decoder: Option<std::path::PathBuf>,

	/// The V4L2 M2M node the encoders open; see
	/// [`encode::Config::v4l2`](moq_video::encode::Config::v4l2).
	#[cfg(all(target_os = "linux", feature = "v4l2"))]
	pub v4l2_encoder: Option<std::path::PathBuf>,
}

impl Config {
	/// The source decoder: the configured implementation and devices,
	/// delivering frames where the resize expects them, at native size.
	pub(crate) fn decoder(&self) -> moq_video::decode::Config {
		let mut decoder = moq_video::decode::Config::new();
		decoder.kind = self.decoder.clone();
		decoder.output = self.resize.output;
		#[cfg(all(target_os = "linux", feature = "vaapi"))]
		{
			decoder.vaapi = self.vaapi.clone();
		}
		#[cfg(all(target_os = "linux", feature = "v4l2"))]
		{
			decoder.v4l2 = self.v4l2_decoder.clone();
		}
		decoder
	}

	/// A rung encoder at `size` and `framerate`: the configured implementation and devices.
	pub(crate) fn encoder(&self, size: moq_video::Size, framerate: moq_video::FrameRate) -> moq_video::encode::Config {
		let mut encoder = moq_video::encode::Config::new(size.width, size.height, framerate);
		encoder.kind = self.encoder.clone();
		#[cfg(all(target_os = "linux", feature = "vaapi"))]
		{
			encoder.vaapi = self.vaapi.clone();
		}
		#[cfg(all(target_os = "linux", feature = "v4l2"))]
		{
			encoder.v4l2 = self.v4l2_encoder.clone();
		}
		encoder
	}
}

/// A budget of open encoder sessions, shared by its clones.
#[derive(Clone, Debug, Default)]
pub struct Sessions(Option<Arc<Semaphore>>);

impl Sessions {
	/// At most `limit` encoder sessions open at once across every clone.
	pub fn new(limit: NonZeroUsize) -> Self {
		Self(Some(Arc::new(Semaphore::new(limit.get()))))
	}

	/// Wait for a session, held until the permit drops; `None` without a budget.
	pub(crate) async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
		let semaphore = self.0.clone()?;
		Some(semaphore.acquire_owned().await.expect("the budget is never closed"))
	}

	/// The sessions still free, for tests.
	#[cfg(test)]
	pub(crate) fn available(&self) -> usize {
		self.0
			.as_ref()
			.map_or(usize::MAX, |semaphore| semaphore.available_permits())
	}
}
