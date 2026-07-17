//! Capture audio on demand and publish it as an encoded track.
//!
//! The turnkey entry point, mirroring `moq_video::encode::publish_capture`: the
//! capture-side settings come from [`capture::Config`](crate::capture::Config),
//! the encode-side settings from [`Options`], and the input PCM layout is read
//! off the source rather than declared by the caller.

use super::{Input, Options, Producer};
use crate::capture;
use crate::{Error, Format, Frame};

/// Capture audio on demand and publish it as an encoded moq track.
///
/// The catalog rendition is registered up front from the source's reported
/// format (no capture needed), but the device only opens while a subscriber is
/// listening and is released when the last one leaves. On resume the timeline
/// re-anchors (via [`Producer::reset_epoch`]) so the idle gap lands in the PTS,
/// keeping audio aligned with a wall-clock video track.
///
/// Frames are stamped from `clock`, so passing the same [`Clock`](moq_mux::Clock)
/// to a concurrent video publish keeps the two tracks aligned. Returns when the
/// broadcast is dropped or the capture loop fails.
pub async fn publish_capture(
	mut broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	capture: capture::Config,
	encode: Options,
	clock: moq_mux::Clock,
) -> Result<(), Error> {
	let (sample_rate, channels) = capture::format(&capture).await?;
	let input = Input {
		format: Format::F32,
		sample_rate,
		channels,
	};

	let mut producer = Producer::new(&mut broadcast, catalog, input, &encode)?;
	let track = producer.track().clone();

	let result = capture_loop(&mut producer, &track, &capture, &clock).await;

	// Best-effort clean close: flush the trailing sub-frame and finalize the
	// track. Runs only when the loop ends on its own; a Ctrl+C cancels the future
	// before this point, since async `Drop` can't finalize the track.
	if let Err(err) = producer.finish() {
		tracing::debug!(error = %err, "audio track finish after capture ended");
	}
	result
}

/// Async capture/encode loop: open the source while a listener is subscribed,
/// release it when the last one leaves, and re-anchor the timeline on resume so
/// the idle gap lands in the PTS.
///
/// Cancel safety: every wait is a real `.await` (a buffer read or a demand
/// transition), so dropping this future (e.g. on Ctrl+C) drops the input and
/// stops the underlying stream. No blocking thread is left behind.
async fn capture_loop(
	producer: &mut Producer,
	track: &moq_net::track::Producer,
	config: &capture::Config,
	clock: &moq_mux::Clock,
) -> Result<(), Error> {
	loop {
		// Idle until a listener subscribes; the track ending is a clean exit.
		if let Err(err) = track.used().await {
			log_track_ended(err);
			return Ok(());
		}

		let mut input = capture::open(config).await?;

		loop {
			// Race the next buffer against the last listener leaving so we release
			// the device promptly. `biased` checks demand first so an unwatched track
			// stops before reading another buffer.
			let samples = tokio::select! {
				biased;
				res = track.unused() => {
					if let Err(err) = res {
						log_track_ended(err);
						return Ok(());
					}
					break; // no listeners: release the device, then wait for one
				}
				samples = input.read() => samples,
			};

			let Some(samples) = samples else { break }; // device stopped producing samples

			// Stamp from the shared clock (including any idle gap) so the producer's
			// epoch re-anchors and audio stays aligned with the video track.
			producer.write(&frame(samples, clock.micros())?)?;
		}

		// Release the device and re-anchor so the next frame after resume reflects the gap.
		drop(input);
		producer.reset_epoch();
		tracing::info!("no listeners: released audio capture");
	}
}

/// A dropped or closed track is the normal end of a publish; any other cause is
/// a real abort (e.g. a transport reset) worth surfacing rather than treating as
/// a clean exit.
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("audio track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "audio track aborted; stopping capture");
	}
}

/// Pack interleaved `f32` samples into a timestamped [`Frame`] of little-endian
/// bytes (i.e. [`Format::F32`]).
fn frame(samples: Vec<f32>, timestamp_us: u64) -> Result<Frame, Error> {
	let mut bytes = Vec::with_capacity(samples.len() * size_of::<f32>());
	for sample in &samples {
		bytes.extend_from_slice(&sample.to_le_bytes());
	}
	Ok(Frame {
		timestamp: moq_net::Timestamp::from_micros(timestamp_us)?,
		data: bytes.into(),
	})
}
