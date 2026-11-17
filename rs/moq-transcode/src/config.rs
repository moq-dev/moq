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
	/// config holds a clone. A hardware encoder refuses sessions past a device limit, which a
	/// device shared with other work reaches sooner, so past the budget an encoder waits for
	/// one to close instead. The default sets no budget.
	pub sessions: Sessions,

	/// The hardware the decoder opens; see
	/// [`decode::Config::device`](moq_video::decode::Config::device). Name the
	/// encoders' device too, so a decoded frame reaches them without a copy.
	pub decoder_device: moq_video::Device,

	/// The hardware the encoders open; see
	/// [`encode::Config::device`](moq_video::encode::Config::device).
	pub encoder_device: moq_video::Device,
}

impl Config {
	/// The source decoder: the configured implementation and device,
	/// delivering frames where the resize expects them, at native size.
	pub(crate) fn decoder(&self) -> moq_video::decode::Config {
		let mut decoder = moq_video::decode::Config::new();
		decoder.kind = self.decoder.clone();
		decoder.output = self.resize.output;
		decoder.device = self.decoder_device.clone();
		decoder
	}

	/// A rung encoder at `size` and `framerate`: the configured implementation and device.
	pub(crate) fn encoder(&self, size: moq_video::Size, framerate: moq_video::FrameRate) -> moq_video::encode::Config {
		let mut encoder = moq_video::encode::Config::new(size.width, size.height, framerate);
		encoder.kind = self.encoder.clone();
		encoder.device = self.encoder_device.clone();
		encoder
	}
}

/// A budget of open encoder sessions, shared by its clones.
///
/// Encoders that serve, each rung's live encoder and each fetched group's, share the budget's
/// serving sessions. One more session is kept for probing a new rung's catalog entry, so a
/// catalog update never waits on serving encoders that only the new catalog would retire.
#[derive(Clone, Debug, Default)]
pub struct Sessions(Option<Budget>);

#[derive(Clone, Debug)]
struct Budget {
	serving: Arc<Semaphore>,
	/// One permit, so probes across every clone take turns on the kept session.
	probe: Arc<Semaphore>,
}

impl Sessions {
	/// At most `serving` serving encoders open at once across every clone, plus one probe:
	/// leave room for `serving + 1` sessions on the device.
	pub fn new(serving: NonZeroUsize) -> Self {
		Self(Some(Budget {
			serving: Arc::new(Semaphore::new(serving.get())),
			probe: Arc::new(Semaphore::new(1)),
		}))
	}

	/// Whether a budget is set.
	pub(crate) fn is_set(&self) -> bool {
		self.0.is_some()
	}

	/// Wait for a serving session, held until the permit drops; `None` without a budget.
	pub(crate) async fn serve(&self) -> Option<OwnedSemaphorePermit> {
		Some(acquire(self.0.as_ref()?.serving.clone()).await)
	}

	/// Wait for the probe's session, held until the permit drops; `None` without a budget.
	pub(crate) async fn probe(&self) -> Option<OwnedSemaphorePermit> {
		Some(acquire(self.0.as_ref()?.probe.clone()).await)
	}

	/// The serving sessions still free, for tests.
	#[cfg(test)]
	pub(crate) fn available(&self) -> usize {
		self.0
			.as_ref()
			.map_or(usize::MAX, |budget| budget.serving.available_permits())
	}
}

/// Run `open`, which opens a codec, holding `session`, and hand both back.
///
/// A codec keeps opening on its own thread after its open future drops, so under a budget the
/// open runs as a task holding the session: a caller that leaves lets it finish, and the codec
/// it opened closes, joining its thread, before the session returns to the budget.
pub(crate) async fn opening<T: Send + 'static>(
	session: Option<OwnedSemaphorePermit>,
	open: impl Future<Output = T> + Send + 'static,
) -> (T, Option<OwnedSemaphorePermit>) {
	let Some(session) = session else {
		return (open.await, None);
	};
	// The result drops before the session if nobody takes it.
	let task = tokio::spawn(async move { (open.await, session) });
	match task.await {
		Ok((opened, session)) => (opened, Some(session)),
		Err(err) => std::panic::resume_unwind(err.into_panic()),
	}
}

async fn acquire(semaphore: Arc<Semaphore>) -> OwnedSemaphorePermit {
	semaphore.acquire_owned().await.expect("the budget is never closed")
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A caller that leaves while a codec opens returns its session only once the open ends,
	/// as the codec's own thread keeps opening after the future drops.
	#[tokio::test]
	async fn an_abandoned_open_holds_its_session_until_it_ends() {
		let sessions = Sessions::new(NonZeroUsize::MIN);
		let (finish, finished) = tokio::sync::oneshot::channel::<()>();
		let mut open = Box::pin(opening(sessions.serve().await, async move {
			let _ = finished.await;
		}));
		// Polled once, so the open is under way, then abandoned.
		tokio::select! {
			biased;
			_ = &mut open => unreachable!("the open has not ended"),
			() = std::future::ready(()) => {}
		}
		drop(open);
		assert_eq!(
			sessions.available(),
			0,
			"the abandoned open gave its session back early"
		);

		finish.send(()).unwrap();
		let _ = sessions.serve().await;
	}
}
