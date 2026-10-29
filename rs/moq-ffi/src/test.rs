use super::origin::*;
use super::producer::*;
use super::server::{MoqServer, MoqServerConfig, MoqServerTls};
use super::session::{MoqClient, MoqClientConfig, MoqClientTls, MoqSession};
use crate::consumer::MoqBroadcastConsumer;
use crate::consumer::MoqFetchGroupOptions;
use crate::consumer::MoqGroupConsumer;
use crate::consumer::MoqTrackConsumer;
use crate::consumer::{MoqAnchor, MoqOrder, MoqRange, MoqSubscription};
use crate::error::{MoqError, MoqProtocolError, MoqProtocolKind};
use crate::json::{
	MoqJsonSnapshotConfig, MoqJsonSnapshotConsumer, MoqJsonSnapshotProducer, MoqJsonStreamConsumer,
	MoqJsonStreamProducer, MoqJsonWindowConfig, MoqJsonWindowConsumer, MoqJsonWindowEvent, MoqJsonWindowProducer,
	MoqJsonWindowTimed,
};
use crate::media::*;
use crate::session::{
	MoqBackoff, MoqCongestionControl, MoqConnectionPhase, MoqQuicConfig, MoqWebSocketConfig, moq_client_defaults,
	moq_qlog_supported, moq_versions,
};

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// An explicitly timed raw track, whose frame API accepts microsecond timestamps.
fn timed_info() -> MoqTrackInfo {
	MoqTrackInfo {
		priority: 0,
		max_age: None,
		refresh: None,
		timescale: Some(1_000_000),
		compress: false,
	}
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// Wait until the FFI runtime has polled work spawned ahead of this call.
async fn ffi_caught_up() {
	let (tx, rx) = tokio::sync::oneshot::channel();
	crate::ffi::spawn(async move {
		let _ = tx.send(());
	});
	rx.await.expect("ffi runtime dropped the catch-up task");
}

/// Spawn `fut`, poll it once, and wait until the FFI runtime has caught up with what that started.
async fn spawn_parked<F, T>(fut: F) -> tokio::task::JoinHandle<T>
where
	F: Future<Output = T> + Send + 'static,
	T: Send + 'static,
{
	let (started, started_rx) = tokio::sync::oneshot::channel();
	let handle = tokio::spawn(async move {
		tokio::pin!(fut);
		let mut started = Some(started);
		let first = std::future::poll_fn(|cx| {
			let poll = fut.as_mut().poll(cx);
			if let Some(started) = started.take() {
				let _ = started.send(());
			}
			std::task::Poll::Ready(poll)
		})
		.await;
		match first {
			std::task::Poll::Ready(value) => value,
			std::task::Poll::Pending => fut.await,
		}
	});
	tokio::time::timeout(TIMEOUT, started_rx)
		.await
		.expect("timed out waiting for the read to start")
		.expect("the read task dropped before polling");
	tokio::time::timeout(TIMEOUT, ffi_caught_up())
		.await
		.expect("timed out waiting for the FFI runtime to poll the read");
	handle
}

/// Trust any certificate, for dialing a server with a generated one.
fn insecure_tls() -> MoqClientTls {
	MoqClientTls {
		insecure: true,
		..Default::default()
	}
}

/// A self-signed identity for `localhost`.
fn localhost_tls() -> MoqServerTls {
	MoqServerTls {
		generate: vec!["localhost".into()],
		..Default::default()
	}
}

/// Retry pacing fast enough for a test, retrying forever.
fn fast_backoff() -> MoqBackoff {
	MoqBackoff {
		initial: Some(Duration::from_millis(50)),
		multiplier: Some(2),
		max: Some(Duration::from_millis(200)),
		timeout: Some(Duration::ZERO),
	}
}

async fn wait_for_config_error(
	mut op: impl FnMut() -> Result<(), MoqError>,
	want: impl Fn(&MoqError) -> bool,
) -> MoqError {
	tokio::time::timeout(TIMEOUT, async {
		loop {
			match op() {
				Err(err) if want(&err) => return err,
				Ok(()) => tokio::task::yield_now().await,
				Err(err) => panic!("unexpected configuration error while waiting: {err:?}"),
			}
		}
	})
	.await
	.expect("timed out waiting for the expected configuration error")
}

/// Wait until `read_frame` has taken `group` off the ordered cursor.
async fn wait_group_acquired(group: &MoqGroupProducer) {
	tokio::time::timeout(TIMEOUT, group.wanted())
		.await
		.expect("timed out waiting for the group to be acquired")
		.unwrap();
}

/// Run `read_frame` until it has taken `group`, proving it did not treat the
/// current group as EOF, then abort that call. The group stays in the reader.
async fn cancel_parked_read_frame(consumer: &Arc<MoqTrackConsumer>, group: &MoqGroupProducer) {
	let mut read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.read_frame().await }).await
	};
	tokio::select! {
		result = &mut read => match result {
			Ok(Ok(Some(_))) => panic!("read_frame returned a frame before one was written"),
			Ok(Ok(None)) => panic!("read_frame returned EOF before a frame was written"),
			Ok(Err(err)) => panic!("read_frame errored before a frame was written: {err:?}"),
			Err(err) => panic!("read task failed: {err:?}"),
		},
		_ = wait_group_acquired(group) => {}
	}
	read.abort();
	match read.await {
		Err(err) => assert!(err.is_cancelled()),
		Ok(Ok(Some(_))) => panic!("aborted read returned a frame"),
		Ok(Ok(None)) => panic!("aborted read returned EOF"),
		Ok(Err(err)) => panic!("aborted read errored: {err:?}"),
	}
}

fn assert_protocol(err: &MoqError, scope: crate::error::MoqErrorScope, kind: crate::error::MoqProtocolKind) {
	match err {
		MoqError::Protocol { details: protocol } => {
			assert_eq!(protocol.scope, scope, "{err:?}");
			assert_eq!(protocol.kind, kind, "{err:?}");
		}
		other => panic!("expected Protocol {kind:?}/{scope:?}, got {other:?}"),
	}
}

#[tokio::test]
async fn detached_cancels_inner_on_drop() {
	struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

	impl Drop for DropSignal {
		fn drop(&mut self) {
			let _ = self.0.take().unwrap().send(());
		}
	}

	let (started_tx, started_rx) = tokio::sync::oneshot::channel();
	let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
	let outer = tokio::spawn(crate::ffi::detached(async move {
		let _drop = DropSignal(Some(dropped_tx));
		started_tx.send(()).unwrap();
		std::future::pending::<Result<(), MoqError>>().await
	}));

	tokio::time::timeout(TIMEOUT, started_rx)
		.await
		.expect("timed out waiting for detached task to start")
		.unwrap();
	outer.abort();
	assert!(outer.await.unwrap_err().is_cancelled());
	tokio::time::timeout(TIMEOUT, dropped_rx)
		.await
		.expect("timed out waiting for detached task to be dropped")
		.unwrap();
}

/// A bare [`MoqAudioInit`] with a format and its init bytes.
fn audio_init(format: MoqAudioFormat, data: Vec<u8>) -> MoqAudioInit {
	MoqAudioInit {
		format,
		data,
		label: None,
	}
}

fn video_init(format: MoqVideoFormat, data: Vec<u8>) -> MoqVideoInit {
	MoqVideoInit {
		format,
		data,
		label: None,
		hint: None,
	}
}

/// Wait for `path` to be announced on `origin` and return its consumer.
///
/// A local reference published asynchronously may appear after discovery has synced empty.
/// These tests wait for that future announcement before resolving the reference.
async fn await_announced(consumer: &MoqOriginConsumer, path: &str) -> Arc<MoqBroadcastConsumer> {
	let announced = consumer.announced_broadcast(path.into()).unwrap();
	tokio::time::timeout(TIMEOUT, announced.available())
		.await
		.unwrap_or_else(|_| panic!("timed out waiting for {path} to be announced"))
		.unwrap()
}

/// Create a broadcast and announce its exact path: the create / populate / announce
/// order with nothing to populate yet.
fn create_announced(origin: &MoqOriginProducer, path: &str) -> Arc<MoqBroadcastProducer> {
	let publication = origin.create_broadcast(path.into()).unwrap();
	publication.announce(MoqAnnounceRoute::default()).unwrap();
	publication.broadcast()
}

/// The next announce event, failing the test on a timeout or a closed origin.
async fn next_event(announced: &MoqAnnounceConsumer) -> MoqAnnounceEvent {
	tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out waiting for an announce event")
		.unwrap()
		.expect("origin ended while waiting for an announce event")
}

/// The next newly announced route.
async fn next_announced(announced: &MoqAnnounceConsumer) -> MoqAnnounce {
	match next_event(announced).await {
		MoqAnnounceEvent::Start { announce } => announce,
		other => panic!("expected an announcement, got {other:?}"),
	}
}

/// A rendition whose ID is its track name, for a track with no catalog of its own.
fn rendition(id: &str, container: MoqContainer) -> MoqRendition {
	MoqRendition::Video {
		id: id.into(),
		config: MoqVideo {
			preference: 0,
			label: None,
			broadcast: None,
			epoch: None,
			track: None,
			codec: "avc1.64001f".into(),
			description: None,
			coded: None,
			display_aspect: None,
			bitrate: None,
			enabled: true,
			framerate: None,
			container,
		},
	}
}

/// A rendition whose catalog `broadcast` field names `reference`, served by `epoch`.
fn referencing(reference: Option<&str>, epoch: Option<&str>) -> MoqRendition {
	let mut rendition = rendition("video", MoqContainer::Legacy);
	let MoqRendition::Video { config, .. } = &mut rendition else {
		unreachable!()
	};
	config.broadcast = reference.map(Into::into);
	config.epoch = epoch.map(Into::into);
	rendition
}

/// An Opus rendition whose catalog `broadcast` field names `reference`.
#[cfg(feature = "audio")]
fn sibling_audio(reference: &str) -> crate::media::MoqAudio {
	use crate::media::{MoqAudio, MoqContainer};

	MoqAudio {
		label: None,
		broadcast: Some(reference.to_string()),
		epoch: None,
		track: None,
		codec: "opus".to_string(),
		description: None,
		sample_rate: 48_000,
		channel_count: 2,
		bitrate: None,
		enabled: true,
		container: MoqContainer::Legacy,
		jitter: None,
	}
}

#[cfg(feature = "audio")]
fn audio_output() -> crate::audio::MoqAudioDecoderOutput {
	use crate::audio::{MoqAudioDecoderOutput, MoqAudioSampleFormat};
	MoqAudioDecoderOutput {
		format: MoqAudioSampleFormat::F32,
		sample_rate: None,
		channels: None,
		max_delay: None,
		delay: None,
	}
}

/// Build a valid OpusHead init buffer (RFC 7845 §5.1).
fn opus_head() -> Vec<u8> {
	let mut head = Vec::with_capacity(19);
	head.extend_from_slice(b"OpusHead");
	head.push(1); // version
	head.push(2); // channel count (stereo)
	head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	head.extend_from_slice(&48000u32.to_le_bytes()); // sample rate
	head.extend_from_slice(&0u16.to_le_bytes()); // output gain
	head.push(0); // channel mapping family
	head
}

/// H.264 Annex B init with SPS + PPS extracted from Big Buck Bunny (1280x720, High profile, Level 3.1).
fn h264_init() -> Vec<u8> {
	let mut init = Vec::new();
	// SPS NAL unit (from bbb.mp4 avcC)
	init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // start code
	init.extend_from_slice(&[
		0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
		0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
	]);
	// PPS NAL unit (from bbb.mp4 avcC)
	init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // start code
	init.extend_from_slice(&[0x68, 0xee, 0x32, 0xc8, 0xb0]);
	init
}

#[test]
fn origin_config_set_cache_capacity() {
	let origin = MoqOriginProducer::new(MoqOriginConfig {
		cache_capacity_bytes: Some(4096),
	});
	assert_eq!(origin.inner().config().pool.capacity(), Some(4096));
	assert_eq!(
		origin.inner().config().pool.expiry(),
		Some(moq_net::cache::DEFAULT_EXPIRY)
	);
}

#[test]
fn route_conversions_keep_cost_and_derive_anonymity() {
	// An observed route reports its static cost.
	let route = moq_net::origin::Route::default().with_cost(moq_net::origin::Cost::new(9));
	assert_eq!(MoqRoute::from(route).cost, 9);

	// A publisher seeds its production cost with one number.
	let seeded = moq_net::origin::Route::try_from(MoqAnnounceRoute { cost: 5, epoch: None }).unwrap();
	assert_eq!(seeded.cost, moq_net::origin::Cost::new(5));
	assert!(seeded.hops.is_empty());

	let anonymous = MoqRoute::from(
		moq_net::origin::Route::default().with_hops(moq_net::Hops::try_from(vec![moq_net::Hop::UNKNOWN]).unwrap()),
	);
	assert!(anonymous.anonymous);
	assert_eq!(anonymous.hops, vec![0]);
}

#[tokio::test]
async fn announced_route_keeps_static_cost_on_reannounce() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let publication = origin.create_broadcast("priced-route".into()).unwrap();
	publication.announce(MoqAnnounceRoute { cost: 9, epoch: None }).unwrap();

	// Reading an advertisement preserves its static production price.
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let route = loop {
		if let MoqAnnounceEvent::Start { announce } | MoqAnnounceEvent::Update { announce } =
			next_event(&announced).await
			&& announce.prefix == "priced-route"
		{
			break announce.route;
		}
	};
	assert_eq!(route.cost, 9);

	// Re-announcing what was observed keeps the static price.
	publication
		.announce(MoqAnnounceRoute {
			cost: route.cost,
			epoch: route.epoch.clone(),
		})
		.unwrap();
	assert_eq!(publication.inner.route().map(MoqRoute::from), Some(route));

	publication.broadcast().close().unwrap();
}

#[test]
fn epoch_helpers_mint_parse_and_refuse() {
	let epoch = moq_mint_epoch();
	let parsed: moq_net::Epoch = epoch.parse().unwrap();
	assert!(moq_mint_epoch() > epoch, "a later mint orders newer");

	// The text the route carries round-trips, and anything but a UUIDv7 is refused.
	let route = MoqAnnounceRoute {
		epoch: Some(epoch.clone()),
		..Default::default()
	};
	let back = moq_net::origin::Route::try_from(route).unwrap();
	assert_eq!(back.epoch, Some(parsed));
	assert_eq!(MoqRoute::from(back).epoch, Some(epoch.clone()));

	let uuid_v4 = "6ba7b810-9dad-41d1-80b4-00c04fd430c8".to_string();
	for bad in [uuid_v4, epoch.to_uppercase(), String::new()] {
		let route = MoqAnnounceRoute {
			epoch: Some(bad),
			..Default::default()
		};
		assert!(matches!(
			moq_net::origin::Route::try_from(route),
			Err(MoqError::InvalidEpoch { .. })
		));
	}
}

#[tokio::test]
async fn announced_epoch_restarts_on_a_new_epoch() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let publication = origin.create_broadcast("epoch-route".into()).unwrap();

	let first = moq_mint_epoch();
	publication
		.announce(MoqAnnounceRoute {
			epoch: Some(first.clone()),
			..Default::default()
		})
		.unwrap();
	let announce = next_announced(&announced).await;
	assert_eq!(announce.route.epoch, Some(first));

	// A new run of the same publisher is another instance: consumers restart onto it.
	let second = moq_mint_epoch();
	publication
		.announce(MoqAnnounceRoute {
			epoch: Some(second.clone()),
			..Default::default()
		})
		.unwrap();
	match next_event(&announced).await {
		MoqAnnounceEvent::Restart { announce } => {
			assert_eq!(announce.prefix, "epoch-route");
			assert_eq!(announce.route.epoch, Some(second));
		}
		other => panic!("expected a restart, got {other:?}"),
	}

	publication.broadcast().close().unwrap();
}

/// A media track reserves through its demand, so an app that publishes
/// pre-encoded frames (OBS) can follow the grant like a raw track can.
#[tokio::test]
async fn media_track_reserves_through_its_demand() {
	let estimate = moq_net::bandwidth::Producer::new();
	estimate
		.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
		.unwrap();
	let bandwidth = crate::bandwidth::MoqAllocator::new(moq_net::bandwidth::Allocator::new(estimate.consume()));

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let audio = |name: &str| {
		MoqMediaTrackProducer::audio(
			&broadcast,
			MoqMediaTarget::Named {
				name: Some(name.into()),
			},
			audio_init(MoqAudioFormat::Opus, opus_head()),
		)
		.unwrap()
	};
	let media = audio("media");
	let sibling = audio("sibling");
	let demand = media.demand().unwrap();
	let sibling_demand = sibling.demand().unwrap();
	let _sibling_sub = subscribe(&broadcast, &sibling_demand.name()).await;
	let reservation = bandwidth.reserve(&demand, 4_000_000);
	let sibling_reservation = bandwidth.reserve(&sibling_demand, 4_000_000);
	// No subscriber yet: it claims nothing from its sibling, and reads what it would get.
	assert_eq!(sibling_reservation.rate(), Some(3_000_000));
	assert_eq!(reservation.rate(), Some(1_500_000));

	let _sub = subscribe(&broadcast, &demand.name()).await;
	assert_eq!(sibling_reservation.rate(), Some(1_500_000));
	assert_eq!(reservation.rate(), Some(1_500_000));
}

#[tokio::test]
async fn raw_track_activity() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let demand = track.demand().unwrap();
	assert_eq!(demand.name(), "status");

	let consumer = track.consume(None).unwrap();
	tokio::time::timeout(TIMEOUT, demand.until_wanted())
		.await
		.expect("timed out waiting for raw track to become used")
		.unwrap();

	drop(consumer);
	tokio::time::timeout(TIMEOUT, demand.until_unwanted())
		.await
		.expect("timed out waiting for raw track to become unused")
		.unwrap();
}

#[cfg(feature = "audio")]
#[tokio::test]
async fn raw_audio_activity() {
	use crate::audio::*;

	const SAMPLE_RATE: u32 = 48_000;
	const FRAME_DURATION: Duration = Duration::from_millis(20);
	// FRAME_DURATION at SAMPLE_RATE.
	const FRAME_SAMPLES: usize = 960;
	const FIRST_TIMESTAMP_US: u64 = 1_000_000;
	const RESUMED_TIMESTAMP_US: u64 = 2_000_000;

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();
	let audio = MoqAudioProducer::new(
		&MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::audio::MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: SAMPLE_RATE,
				channels: 1,
			},
			output: MoqAudioEncoderOutput {
				codec: MoqAudioCodec::opus(),
				sample_rate: None,
				channels: None,
				bitrate: None,
				frame_duration: Some(FRAME_DURATION),
			},
			track: Some("microphone".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	assert_eq!(audio.demand().unwrap().name(), "microphone");

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for raw audio catalog")
		.unwrap()
		.expect("expected a raw audio catalog");
	let rendition = MoqRendition::Audio {
		id: "microphone".into(),
		config: catalog.audio["microphone"].clone(),
	};
	let subscription = MoqMediaTrackConsumer::subscribe(
		&consumer,
		MoqMediaTrackConfig {
			rendition: rendition.clone(),
			subscription: None,
		},
	)
	.await
	.unwrap();
	tokio::time::timeout(TIMEOUT, audio.demand().unwrap().until_wanted())
		.await
		.expect("timed out waiting for raw audio to become used")
		.unwrap();
	audio
		.write(MoqAudioFrame {
			timestamp_us: FIRST_TIMESTAMP_US,
			data: vec![0; FRAME_SAMPLES * std::mem::size_of::<f32>()],
		})
		.unwrap();
	let first = tokio::time::timeout(TIMEOUT, next_encoded_frame(&subscription))
		.await
		.expect("timed out waiting for raw audio frame")
		.unwrap()
		.expect("expected a raw audio frame");
	assert_eq!(first.timestamp_us, FIRST_TIMESTAMP_US);

	drop(subscription);
	tokio::time::timeout(TIMEOUT, audio.demand().unwrap().until_unwanted())
		.await
		.expect("timed out waiting for raw audio to become unused")
		.unwrap();

	let subscription = MoqMediaTrackConsumer::subscribe(
		&consumer,
		MoqMediaTrackConfig {
			rendition,
			subscription: None,
		},
	)
	.await
	.unwrap();
	tokio::time::timeout(TIMEOUT, audio.demand().unwrap().until_wanted())
		.await
		.expect("timed out waiting for raw audio to become used again")
		.unwrap();
	audio.reanchor().unwrap();
	audio
		.write(MoqAudioFrame {
			timestamp_us: RESUMED_TIMESTAMP_US,
			data: vec![0; FRAME_SAMPLES * std::mem::size_of::<f32>()],
		})
		.unwrap();
	let mut resumed = tokio::time::timeout(TIMEOUT, next_encoded_frame(&subscription))
		.await
		.expect("timed out waiting for resumed raw audio frame")
		.unwrap()
		.expect("expected a resumed raw audio frame");
	if resumed.timestamp_us == first.timestamp_us {
		resumed = tokio::time::timeout(TIMEOUT, next_encoded_frame(&subscription))
			.await
			.expect("timed out waiting for re-anchored raw audio frame")
			.unwrap()
			.expect("expected a re-anchored raw audio frame");
	}
	assert_eq!(resumed.timestamp_us, RESUMED_TIMESTAMP_US);

	audio.finish().unwrap();
	broadcast.close().unwrap();
}

/// Distinct channel tones, continuous across calls from different producer threads.
#[cfg(feature = "audio")]
fn audio_tone(start: usize, frames: usize, channels: usize) -> Vec<u8> {
	(start..start + frames)
		.flat_map(|sample| {
			(0..channels).flat_map(move |channel| {
				let frequency = if channel == 0 { 400.0 } else { 1000.0 };
				(0.4 * (std::f32::consts::TAU * frequency * sample as f32 / 48_000.0).sin()).to_le_bytes()
			})
		})
		.collect()
}

#[cfg(feature = "audio")]
fn tone_power(samples: &[f32], frequency: f64) -> f64 {
	let coefficient = 2.0 * (std::f64::consts::TAU * frequency / 48_000.0).cos();
	let (mut previous, mut older) = (0.0, 0.0);
	for &sample in samples {
		let value = sample as f64 + coefficient * previous - older;
		older = previous;
		previous = value;
	}
	previous * previous + older * older - coefficient * previous * older
}

/// A listener holding a muted rendition passes its disabled config, which subscribes and plays
/// whatever the publisher sends once it unmutes.
#[cfg(feature = "audio")]
#[tokio::test]
async fn audio_subscribes_a_held_disabled_rendition() {
	use crate::audio::*;
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalogs = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();
	let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	let audio = MoqAudioProducer::new(
		&catalog,
		MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: 48_000,
				channels: 2,
			},
			output: MoqAudioEncoderOutput {
				codec: MoqAudioCodec::opus(),
				sample_rate: None,
				channels: None,
				bitrate: None,
				frame_duration: Some(Duration::from_millis(20)),
			},
			track: Some("voice".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	let snapshot = catalogs.next().await.unwrap().unwrap();
	let mut muted = snapshot.audio["voice"].clone();
	muted.enabled = false;
	let decoder = MoqAudioConsumer::subscribe(
		&consumer,
		"voice".into(),
		muted,
		MoqAudioDecoderOutput {
			max_delay: Some(Duration::from_secs(30)),
			..audio_output()
		},
	)
	.await
	.unwrap();

	for index in 0..4 {
		audio
			.write(MoqAudioFrame {
				timestamp_us: index * 20_000,
				data: audio_tone(index as usize * 960, 960, 2),
			})
			.unwrap();
	}
	audio.finish().unwrap();
	let mut frames = 0;
	while let Some(read) = tokio::time::timeout(TIMEOUT, decoder.next()).await.unwrap().unwrap() {
		if matches!(read, MoqAudioRead::Frame { .. }) {
			frames += 1;
		}
	}
	assert!(frames > 0, "the held rendition played nothing");
	broadcast.close().unwrap();
}

/// PCM written and finished from separate host threads survives the binding's locking and drain.
#[cfg(feature = "audio")]
#[tokio::test]
async fn audio_raw_publish_from_many_threads() {
	use crate::audio::*;
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalogs = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();
	let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	let output = MoqAudioEncoderOutput {
		codec: MoqAudioCodec::opus(),
		sample_rate: None,
		channels: None,
		bitrate: None,
		frame_duration: Some(Duration::from_millis(20)),
	};
	let audio = MoqAudioProducer::new(
		&catalog,
		MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: 48_000,
				channels: 2,
			},
			output,
			track: Some("tone".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	let snapshot = catalogs.next().await.unwrap().unwrap();
	let decoder = MoqAudioConsumer::subscribe(
		&consumer,
		"tone".into(),
		snapshot.audio["tone"].clone(),
		MoqAudioDecoderOutput {
			// The track's whole retention: reading starts only after finish, and the budget
			// ages groups by wall clock, so a stalled host would otherwise skip the start.
			max_delay: Some(Duration::from_secs(30)),
			..audio_output()
		},
	)
	.await
	.unwrap();
	for index in 0..8 {
		let producer = audio.clone();
		std::thread::spawn(move || {
			producer.write(MoqAudioFrame {
				timestamp_us: 250_000 + index * 20_000,
				data: audio_tone(index as usize * 960, 960, 2),
			})
		})
		.join()
		.unwrap()
		.unwrap();
	}
	let closer = audio.clone();
	std::thread::spawn(move || closer.finish()).join().unwrap().unwrap();
	let mut pcm = Vec::new();
	while let Some(read) = tokio::time::timeout(TIMEOUT, decoder.next()).await.unwrap().unwrap() {
		let frame = match read {
			MoqAudioRead::Frame { frame } => frame,
			MoqAudioRead::End { end_us } => {
				assert_eq!(end_us, 250_000 + 8 * 20_000, "the finish ends after the last frame");
				continue;
			}
			MoqAudioRead::Gap { .. } => panic!("nothing was lost"),
		};
		assert_eq!(frame.timestamp_us, 250_000 + pcm.len() as u64 / 2 * 1_000_000 / 48_000);
		pcm.extend(frame.data.as_chunks::<4>().0.iter().map(|s| f32::from_le_bytes(*s)));
	}
	assert_eq!(pcm.len(), 8 * 960 * 2, "finish must preserve the entire PCM duration");
	for (channel, want, other) in [(0, 400.0, 1000.0), (1, 1000.0, 400.0)] {
		let samples: Vec<f32> = pcm.iter().skip(channel).step_by(2).copied().collect();
		assert!(
			tone_power(&samples, want) > tone_power(&samples, other),
			"channel {channel} lost its tone"
		);
	}
	assert!(matches!(
		audio.write(MoqAudioFrame {
			timestamp_us: 0,
			data: vec![0; 8]
		}),
		Err(MoqError::Closed)
	));
	broadcast.close().unwrap();
}

/// Reject incomplete PCM channel frames before feeding the codec.
#[cfg(feature = "audio")]
#[tokio::test]
async fn audio_raw_publish_rejects_bad_frames() {
	use crate::audio::*;
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	let audio = MoqAudioProducer::new(
		&catalog,
		MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: 48_000,
				channels: 2,
			},
			output: MoqAudioEncoderOutput {
				codec: MoqAudioCodec::opus(),
				sample_rate: None,
				channels: None,
				bitrate: None,
				frame_duration: Some(Duration::from_millis(20)),
			},
			track: None,
			bandwidth: None,
		},
	)
	.unwrap();
	for size in [1, 4, 7] {
		assert!(
			audio
				.write(MoqAudioFrame {
					timestamp_us: 0,
					data: vec![0; size]
				})
				.is_err()
		);
	}
	audio.finish().unwrap();
	broadcast.close().unwrap();
}

/// The decoder's playout delay is configurable and stays observable while a read waits:
/// the decoder sits behind the read's lock, so the getter must not need it.
#[cfg(feature = "audio")]
#[tokio::test]
async fn raw_audio_decode_delay() {
	use crate::audio::*;

	// 20 ms of a 400 Hz mono tone at 48 kHz.
	const FRAME_SAMPLES: usize = 960;
	const FRAME_US: u64 = 20_000;
	const FIXED: Duration = Duration::from_millis(80);

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();
	let audio = MoqAudioProducer::new(
		&MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::audio::MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: 48_000,
				channels: 1,
			},
			output: MoqAudioEncoderOutput {
				codec: MoqAudioCodec::opus(),
				sample_rate: None,
				channels: None,
				bitrate: None,
				frame_duration: Some(Duration::from_micros(FRAME_US)),
			},
			track: Some("microphone".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for raw audio catalog")
		.unwrap()
		.expect("expected a raw audio catalog");
	let rendition = catalog.audio.get("microphone").unwrap().clone();

	let subscribe = |delay| {
		MoqAudioConsumer::subscribe(
			&consumer,
			"microphone".into(),
			rendition.clone(),
			MoqAudioDecoderOutput {
				delay,
				..audio_output()
			},
		)
	};
	let fixed = subscribe(Some(FIXED)).await.unwrap();
	let estimated = subscribe(None).await.unwrap();
	assert_eq!(fixed.delay(), FIXED);
	let initial = estimated.delay();

	// Both reads park on the decoder before any audio exists.
	let read = |decoded: Arc<MoqAudioConsumer>| tokio::spawn(async move { decoded.next().await });
	let (fixed_read, estimated_read) = (read(fixed.clone()), read(estimated.clone()));
	tokio::task::yield_now().await;
	assert_eq!(fixed.delay(), FIXED, "readable while a read holds the decoder");

	for i in 0..2 {
		audio
			.write(MoqAudioFrame {
				timestamp_us: i * FRAME_US,
				data: audio_tone(i as usize * FRAME_SAMPLES, FRAME_SAMPLES, 1),
			})
			.unwrap();
	}
	for read in [fixed_read, estimated_read] {
		let read = tokio::time::timeout(TIMEOUT, read)
			.await
			.expect("timed out waiting for decoded audio")
			.unwrap()
			.unwrap();
		let Some(MoqAudioRead::Frame { frame }) = read else {
			panic!("expected a decoded frame");
		};
		let pcm: Vec<f32> = frame
			.data
			.as_chunks::<4>()
			.0
			.iter()
			.map(|s| f32::from_le_bytes(*s))
			.collect();
		assert!(
			tone_power(&pcm, 400.0) > tone_power(&pcm, 1000.0),
			"delay path lost the tone"
		);
	}

	assert_eq!(fixed.delay(), FIXED, "a fixed delay ignores arrivals");
	// The decoded frame's duration joins the estimate, so the mirror must have moved.
	assert_ne!(estimated.delay(), initial, "the estimate updates as frames are read");

	audio.finish().unwrap();
	broadcast.close().unwrap();
}

/// Each encoder the publisher stops, by a reanchor or a finish, reaches the caller as an
/// end in microseconds rather than vanishing between frames.
#[cfg(feature = "audio")]
#[tokio::test]
async fn raw_audio_decode_reports_encoder_ends() {
	use crate::audio::*;

	const FRAME_SAMPLES: usize = 960;
	const FRAME_US: u64 = 20_000;
	const RESUMED_US: u64 = 1_000_000;

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalogs = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();
	let audio = MoqAudioProducer::new(
		&MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: 48_000,
				channels: 1,
			},
			output: MoqAudioEncoderOutput {
				codec: MoqAudioCodec::opus(),
				sample_rate: None,
				channels: None,
				bitrate: None,
				frame_duration: Some(Duration::from_micros(FRAME_US)),
			},
			track: Some("microphone".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	let snapshot = catalogs.next().await.unwrap().unwrap();
	let decoder = MoqAudioConsumer::subscribe(
		&consumer,
		"microphone".into(),
		snapshot.audio["microphone"].clone(),
		MoqAudioDecoderOutput {
			// Reading starts only after finish, so keep every group.
			max_delay: Some(Duration::from_secs(30)),
			..audio_output()
		},
	)
	.await
	.unwrap();

	for (i, timestamp_us) in [0, FRAME_US, RESUMED_US, RESUMED_US + FRAME_US].into_iter().enumerate() {
		if timestamp_us == RESUMED_US {
			audio.reanchor().unwrap();
		}
		audio
			.write(MoqAudioFrame {
				timestamp_us,
				data: audio_tone(i * FRAME_SAMPLES, FRAME_SAMPLES, 1),
			})
			.unwrap();
	}
	audio.finish().unwrap();

	let mut ends = Vec::new();
	let mut frames = Vec::new();
	while let Some(read) = tokio::time::timeout(TIMEOUT, decoder.next()).await.unwrap().unwrap() {
		match read {
			MoqAudioRead::Frame { frame } => frames.push((ends.len(), frame.timestamp_us)),
			MoqAudioRead::Gap { .. } => panic!("nothing was lost"),
			MoqAudioRead::End { end_us } => ends.push(end_us),
		}
	}
	// The reanchor drops whatever the first encoder's lookahead still held, while the
	// finish drains the second one completely.
	assert_eq!(ends.len(), 2, "{ends:?}");
	assert!(ends[0] > 0 && ends[0] <= 2 * FRAME_US, "{ends:?}");
	assert_eq!(ends[1], RESUMED_US + 2 * FRAME_US);
	assert!(
		frames.iter().any(|&(epoch, _)| epoch == 1),
		"the reanchored encoder decodes after the first end: {frames:?}"
	);
	for (epoch, timestamp_us) in frames {
		if epoch == 0 {
			assert!(timestamp_us < ends[0], "{timestamp_us} past the first end");
		} else {
			assert!(timestamp_us >= RESUMED_US, "{timestamp_us} before the reanchor");
		}
	}
	broadcast.close().unwrap();
}

/// Opus' 2.5 ms frame survives the trip to the encoder, and a duration Opus can't frame is
/// refused.
#[cfg(feature = "audio")]
#[tokio::test]
async fn raw_audio_frame_durations() {
	use crate::audio::*;

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let input = || MoqAudioEncoderInput {
		format: MoqAudioSampleFormat::F32,
		sample_rate: 48_000,
		channels: 1,
	};
	let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	let output = |frame_duration| MoqAudioEncoderOutput {
		codec: MoqAudioCodec::opus(),
		sample_rate: None,
		channels: None,
		bitrate: None,
		frame_duration: Some(frame_duration),
	};

	let audio = MoqAudioProducer::new(
		&catalog,
		crate::audio::MoqAudioProducerOptions {
			input: input(),
			output: output(Duration::from_micros(2_500)),
			track: Some("fine".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	// 2.5 ms of silence at 48 kHz, mono f32: exactly one encoded frame.
	audio
		.write(MoqAudioFrame {
			timestamp_us: 0,
			data: vec![0; 120 * std::mem::size_of::<f32>()],
		})
		.unwrap();
	audio.finish().unwrap();

	let coarse = MoqAudioProducer::new(
		&catalog,
		crate::audio::MoqAudioProducerOptions {
			input: input(),
			output: output(Duration::from_millis(2)),
			track: Some("coarse".into()),
			bandwidth: None,
		},
	);
	assert!(
		matches!(coarse, Err(MoqError::Audio { .. })),
		"2 ms is not an opus frame duration"
	);

	broadcast.close().unwrap();
}

/// An absent frame duration takes the codec's own frame, and AAC, which encodes only
/// through a platform encoder, is refused where there is none.
#[cfg(feature = "audio")]
#[tokio::test]
async fn raw_audio_codec_default_frame() {
	use crate::audio::*;

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let input = || MoqAudioEncoderInput {
		format: MoqAudioSampleFormat::F32,
		sample_rate: 48_000,
		channels: 2,
	};
	let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	let output = |codec| MoqAudioEncoderOutput {
		codec,
		sample_rate: None,
		channels: None,
		bitrate: None,
		frame_duration: None,
	};

	let opus = MoqAudioProducer::new(
		&catalog,
		crate::audio::MoqAudioProducerOptions {
			input: input(),
			output: output(MoqAudioCodec::opus()),
			track: Some("opus".into()),
			bandwidth: None,
		},
	)
	.unwrap();
	opus.finish().unwrap();

	assert_eq!(MoqAudioCodec::aac().codec(), moq_audio::encode::Codec::Aac);
	let aac = MoqAudioProducer::new(
		&catalog,
		crate::audio::MoqAudioProducerOptions {
			input: input(),
			output: output(MoqAudioCodec::aac()),
			track: Some("aac".into()),
			bandwidth: None,
		},
	);
	let Err(MoqError::Audio { reason: message }) = aac else {
		panic!("no platform AAC encoder on this host");
	};
	assert!(message.contains("no aac audio encoder"), "{message}");

	// 0 still means 1024 samples after an output-rate override, not the input rate.
	let mut resampled = output(MoqAudioCodec::aac());
	resampled.sample_rate = Some(48_000);
	let aac = MoqAudioProducer::new(
		&catalog,
		crate::audio::MoqAudioProducerOptions {
			input: MoqAudioEncoderInput {
				format: MoqAudioSampleFormat::F32,
				sample_rate: 44_100,
				channels: 2,
			},
			output: resampled,
			track: Some("aac-rate".into()),
			bandwidth: None,
		},
	);
	let Err(MoqError::Audio { reason: message }) = aac else {
		panic!("no platform AAC encoder on this host");
	};
	assert!(message.contains("no aac audio encoder"), "{message}");

	broadcast.close().unwrap();
}

#[tokio::test]
async fn raw_track_datagram_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast
		.publish_track(
			"events".into(),
			Some(MoqTrackInfo {
				priority: 0,
				max_age: None,
				refresh: None,
				timescale: Some(1_000_000),
				compress: false,
			}),
		)
		.unwrap();
	let consumer = track.consume(None).unwrap();
	let payload = b"hello datagram".to_vec();

	let sequence = track
		.append_datagram(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(123_456),
		})
		.unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for datagram")
		.unwrap()
		.expect("expected a datagram");

	assert_eq!(datagram.sequence, sequence);
	assert_eq!(datagram.timestamp_us, Some(123_456));
	assert_eq!(datagram.payload, payload);
}

#[tokio::test]
async fn raw_track_info_reports_publisher_properties() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let info = MoqTrackInfo {
		priority: 7,
		max_age: Some(Duration::from_millis(2_500)),
		refresh: Some(2),
		timescale: Some(90_000),
		compress: true,
	};
	let track = broadcast.publish_track("status".into(), Some(info)).unwrap();
	let consumer = track.consume(None).unwrap();

	let got = consumer.info();
	assert_eq!(got.priority, 7);
	assert_eq!(got.max_age, Some(Duration::from_millis(2_500)));
	assert_eq!(got.refresh, Some(2));
	assert_eq!(got.timescale, Some(90_000));
	assert!(got.compress);
}

#[tokio::test]
async fn raw_track_update_does_not_wait_for_pending_read() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	let read = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.read_frame().await })
	};

	consumer
		.update(MoqSubscription {
			priority: 10,
			max_delay: Some(Duration::from_millis(25)),
			ranges: Some(vec![MoqRange {
				start: MoqAnchor::Absolute { group: 0 },
				end: None,
			}]),
			order: None,
		})
		.unwrap();

	let payload = b"updated subscription".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(20_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, read)
		.await
		.expect("timed out waiting for raw frame")
		.expect("read task panicked")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(20_000));
}

/// Subscribe to `name` on `broadcast` as a raw track, for a typed consumer to take over.
async fn subscribe(broadcast: &MoqBroadcastProducer, name: &str) -> Arc<MoqTrackConsumer> {
	broadcast
		.consume()
		.unwrap()
		.subscribe_track(name.into(), None)
		.await
		.unwrap()
}

#[tokio::test(start_paused = true)]
async fn data_writes_preserve_optional_timestamps() {
	for timestamp_us in [None, Some(123_456)] {
		for kind in ["json snapshot", "json stream"] {
			let broadcast = MoqBroadcastProducer::new().unwrap();
			let track = broadcast
				.publish_track(
					"data".into(),
					Some(MoqTrackInfo {
						priority: 0,
						max_age: None,
						refresh: None,
						timescale: timestamp_us.map(|_| 1_000_000),
						compress: false,
					}),
				)
				.unwrap();
			let consumer = subscribe(&broadcast, "data").await;
			let payload = b"{\"n\":1}";
			match kind {
				"json snapshot" => {
					let producer =
						MoqJsonSnapshotProducer::new(&broadcast, &track, MoqJsonSnapshotConfig { deltas: false })
							.unwrap();
					producer
						.update(String::from_utf8(payload.to_vec()).unwrap(), timestamp_us)
						.unwrap();
					producer.finish().unwrap();
				}
				"json stream" => {
					let producer = MoqJsonStreamProducer::new(&broadcast, &track).unwrap();
					producer
						.append(String::from_utf8(payload.to_vec()).unwrap(), timestamp_us)
						.unwrap();
					producer.finish().unwrap();
				}
				_ => unreachable!(),
			}
			let frame = consumer.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.payload, payload, "{kind}");
			assert_eq!(frame.timestamp_us, timestamp_us, "{kind}");
			assert_eq!(consumer.info().timescale, timestamp_us.map(|_| 1_000_000));
			broadcast.close().unwrap();
		}
	}
}

#[tokio::test(start_paused = true)]
async fn whole_media_frames_require_the_callers_timestamp() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, opus_head()),
	)
	.unwrap();
	assert!(matches!(
		media.write_frame(MoqFrame {
			payload: b"opus frame".to_vec(),
			timestamp_us: None
		}),
		Err(MoqError::Codec { .. })
	));
	media
		.write_frame(MoqFrame {
			payload: b"opus frame".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	media.finish().unwrap();
	broadcast.close().unwrap();
}

#[tokio::test]
async fn catalog_now_fixes_the_clock_for_later_media() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	let clock = broadcast
		.with_state(|state| Ok(state.catalog.snapshot().clock))
		.unwrap();
	// Withhold publication so only the explicit clock read can fix it before the first frame.
	let reserved = broadcast.with_state(|state| Ok(state.catalog.reserve())).unwrap();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, opus_head()),
	)
	.unwrap();
	let now = catalog.now().unwrap();
	assert!(now > 0);
	media
		.write_frame(MoqFrame {
			payload: b"opus frame".to_vec(),
			timestamp_us: Some(1_000_000),
		})
		.unwrap();
	assert_eq!(
		broadcast
			.with_state(|state| Ok(state.catalog.snapshot().clock))
			.unwrap(),
		clock
	);
	drop(reserved);
	media.finish().unwrap();
	broadcast.close().unwrap();
	assert!(matches!(catalog.now(), Err(MoqError::Closed)));
}

/// `next` yields every state in order with its timestamp; `latest` skips to the newest.
#[tokio::test]
async fn json_snapshot_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = MoqJsonSnapshotConfig { deltas: true };
	let info = MoqTrackInfo {
		priority: 0,
		max_age: None,
		timescale: Some(1_000_000),
		refresh: None,
		compress: true,
	};
	let track = broadcast.publish_track("meta".into(), Some(info)).unwrap();
	let producer = MoqJsonSnapshotProducer::new(&broadcast, &track, config.clone()).unwrap();
	assert!(
		matches!(track.demand(), Err(MoqError::Closed)),
		"the producer takes over the track"
	);
	let consumer = MoqJsonSnapshotConsumer::new(&*subscribe(&broadcast, "meta").await).unwrap();

	for n in 1..=4 {
		producer.update(format!(r#"{{"a":{n}}}"#), Some(n * 1000)).unwrap();
	}

	let value = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for json snapshot")
		.unwrap()
		.expect("expected a value");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(&value.value).unwrap(),
		serde_json::json!({ "a": 1 })
	);
	assert_eq!(value.timestamp_us, Some(1000));

	let value = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for json snapshot delta")
		.unwrap()
		.expect("expected a value");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(&value.value).unwrap(),
		serde_json::json!({ "a": 2 })
	);
	assert_eq!(value.timestamp_us, Some(2000));

	// `latest` skips the buffered third state.
	let value = tokio::time::timeout(TIMEOUT, consumer.latest())
		.await
		.expect("timed out waiting for the latest json snapshot")
		.unwrap()
		.expect("expected a value");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(&value.value).unwrap(),
		serde_json::json!({ "a": 4 })
	);
	assert_eq!(value.timestamp_us, Some(4000));

	producer.finish().unwrap();
	assert!(matches!(
		producer.update(r#"{"a":5}"#.into(), Some(5000)),
		Err(MoqError::Closed)
	));
	let end = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for the end of the json snapshot")
		.unwrap();
	assert_eq!(end, None);
}

/// An idle snapshot producer restates its value in a fresh group once a join finds more deltas
/// than the track's refresh ratio allows.
#[tokio::test]
async fn json_snapshot_refreshes_for_a_joiner() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast
		.publish_track(
			"meta".into(),
			Some(MoqTrackInfo {
				priority: 127,
				max_age: None,
				refresh: Some(1),
				timescale: None,
				compress: false,
			}),
		)
		.unwrap();
	let config = MoqJsonSnapshotConfig { deltas: true };
	let producer = MoqJsonSnapshotProducer::new(&broadcast, &track, config.clone()).unwrap();
	for n in 0..4 {
		producer.update(format!(r#"{{"n":{n}}}"#), None).unwrap();
	}

	// The join crosses the ratio, so the producer restates the value without another update. The
	// joiner may read the open group first or skip straight to the restatement.
	let joiner = subscribe(&broadcast, "meta").await;
	let refreshed = tokio::time::timeout(TIMEOUT, async {
		loop {
			let group = joiner.recv_group().await.unwrap().expect("expected a group");
			if group.sequence() > 0 {
				return group;
			}
		}
	})
	.await
	.expect("timed out waiting for the refresh");
	assert_eq!(refreshed.sequence(), 1);
	let consumer = MoqJsonSnapshotConsumer::new(&*subscribe(&broadcast, "meta").await).unwrap();
	let value = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for the restated value")
		.unwrap()
		.expect("expected a value");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(&value.value).unwrap(),
		serde_json::json!({ "n": 3 })
	);

	producer.finish().unwrap();
}

/// JSON producers report subscriber demand through a handle, like the media and raw track producers.
#[tokio::test]
async fn json_demand() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let snapshot_config = MoqJsonSnapshotConfig { deltas: true };
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let snapshot = MoqJsonSnapshotProducer::new(&broadcast, &track, snapshot_config.clone()).unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let stream = MoqJsonStreamProducer::new(&broadcast, &track).unwrap();
	let snapshot_demand = snapshot.demand().unwrap();
	let stream_demand = stream.demand().unwrap();
	assert_eq!(snapshot_demand.name(), "status");
	assert_eq!(stream_demand.name(), "events");
	assert!(!snapshot_demand.wanted().is_some());

	let snapshot_consumer = MoqJsonSnapshotConsumer::new(&*subscribe(&broadcast, "status").await).unwrap();
	let stream_consumer = MoqJsonStreamConsumer::new(&*subscribe(&broadcast, "events").await).unwrap();

	tokio::time::timeout(TIMEOUT, snapshot_demand.until_wanted())
		.await
		.expect("timed out waiting for the json snapshot to become used")
		.unwrap();
	tokio::time::timeout(TIMEOUT, stream_demand.until_wanted())
		.await
		.expect("timed out waiting for the json stream to become used")
		.unwrap();
	assert!(snapshot_demand.wanted().is_some());

	drop(snapshot_consumer);
	drop(stream_consumer);
	tokio::time::timeout(TIMEOUT, snapshot_demand.until_unwanted())
		.await
		.expect("timed out waiting for the json snapshot to become unused")
		.unwrap();
	tokio::time::timeout(TIMEOUT, stream_demand.until_unwanted())
		.await
		.expect("timed out waiting for the json stream to become unused")
		.unwrap();

	snapshot.finish().unwrap();
	stream.finish().unwrap();
	assert!(matches!(snapshot.demand(), Err(MoqError::Closed)));
	assert!(matches!(stream.demand(), Err(MoqError::Closed)));
	assert!(matches!(snapshot_demand.until_wanted().await, Err(MoqError::Closed)));
	assert!(matches!(stream_demand.until_wanted().await, Err(MoqError::Closed)));
}

/// Read the next window event, failing the test if none arrives.
async fn next_window(consumer: &MoqJsonWindowConsumer) -> Option<MoqJsonWindowTimed> {
	tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for a json window event")
		.unwrap()
}

fn window_push(index: u64, n: u64) -> MoqJsonWindowEvent {
	MoqJsonWindowEvent::Push {
		index,
		value: format!(r#"{{"n":{n}}}"#),
	}
}

/// A live reader sees each push and pop in order, with its timestamp.
#[tokio::test]
async fn json_window_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = MoqJsonWindowConfig {
		checkpoint_records: None,
	};
	let track = broadcast.publish_track("log".into(), Some(timed_info())).unwrap();
	let producer = MoqJsonWindowProducer::new(&track, config.clone()).unwrap();
	assert!(
		matches!(track.demand(), Err(MoqError::Closed)),
		"the producer takes over the track"
	);
	let consumer = MoqJsonWindowConsumer::new(&*subscribe(&broadcast, "log").await).unwrap();

	for n in 0..3 {
		producer.push(format!(r#"{{"n":{n}}}"#), Some(n * 1000)).unwrap();
	}
	producer.pop(2, Some(3000)).unwrap();

	for n in 0..3 {
		let event = next_window(&consumer).await.expect("expected a push");
		assert_eq!(
			event,
			MoqJsonWindowTimed {
				event: window_push(n, n),
				timestamp_us: Some(n * 1000),
			}
		);
	}
	assert_eq!(
		next_window(&consumer).await,
		Some(MoqJsonWindowTimed {
			event: MoqJsonWindowEvent::Pop { start: 0, end: 2 },
			timestamp_us: Some(3000),
		})
	);

	producer.finish().unwrap();
	assert!(matches!(
		producer.push(r#"{"n":3}"#.into(), Some(4000)),
		Err(MoqError::Closed)
	));
	assert!(matches!(producer.pop(1, Some(4000)), Err(MoqError::Closed)));
	assert_eq!(next_window(&consumer).await, None);
}

/// A cut closes the open group, so the next push opens a new group whose header restates the
/// retained window, while a live reader sees only the new record.
#[tokio::test]
async fn json_window_cut_restates_the_window() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = MoqJsonWindowConfig {
		checkpoint_records: None,
	};
	let track = broadcast.publish_track("log".into(), None).unwrap();
	let producer = MoqJsonWindowProducer::new(&track, config.clone()).unwrap();
	let raw = subscribe(&broadcast, "log").await;
	let consumer = MoqJsonWindowConsumer::new(&*subscribe(&broadcast, "log").await).unwrap();

	for n in 0..3 {
		producer.push(format!(r#"{{"n":{n}}}"#), None).unwrap();
	}
	producer.pop(1, None).unwrap();
	let events = [
		window_push(0, 0),
		window_push(1, 1),
		window_push(2, 2),
		MoqJsonWindowEvent::Pop { start: 0, end: 1 },
	];
	for event in events {
		assert_eq!(
			next_window(&consumer).await,
			Some(MoqJsonWindowTimed {
				event,
				timestamp_us: None,
			})
		);
	}

	producer.cut().unwrap();
	producer.cut().unwrap();
	producer.push(r#"{"n":3}"#.into(), None).unwrap();

	let group = tokio::time::timeout(TIMEOUT, async {
		loop {
			let group = raw.recv_group().await.unwrap().expect("expected a group");
			if group.sequence() > 0 {
				return group;
			}
		}
	})
	.await
	.expect("timed out waiting for the cut's group");
	assert_eq!(group.sequence(), 1, "a repeated cut opens no extra group");
	let header = group.read_frame().await.unwrap().expect("expected the header");
	assert_eq!(
		serde_json::from_slice::<serde_json::Value>(&header.payload).unwrap(),
		serde_json::json!({ "offset": 1, "records": [{ "n": 1 }, { "n": 2 }, { "n": 3 }] })
	);

	// The restated records are already delivered, so the live reader sees only the new one.
	assert_eq!(
		next_window(&consumer).await,
		Some(MoqJsonWindowTimed {
			event: window_push(3, 3),
			timestamp_us: None,
		})
	);

	producer.finish().unwrap();
	assert!(matches!(producer.cut(), Err(MoqError::Closed)));
}

/// A joiner past the refresh ratio gets the window restated in a fresh group, holding only the
/// checkpoint's records, so it learns of the older ones as a skip.
#[tokio::test]
async fn json_window_refreshes_a_bounded_checkpoint_for_a_joiner() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast
		.publish_track(
			"log".into(),
			Some(MoqTrackInfo {
				priority: 127,
				max_age: None,
				refresh: Some(1),
				timescale: None,
				compress: false,
			}),
		)
		.unwrap();
	let config = MoqJsonWindowConfig {
		checkpoint_records: Some(2),
	};
	let producer = MoqJsonWindowProducer::new(&track, config.clone()).unwrap();
	for n in 0..5 {
		producer.push(format!(r#"{{"n":{n}}}"#), None).unwrap();
	}

	// The join crosses the ratio, so the producer restates the window without another edit.
	let joiner = subscribe(&broadcast, "log").await;
	tokio::time::timeout(TIMEOUT, async {
		while joiner.recv_group().await.unwrap().expect("expected a group").sequence() == 0 {}
	})
	.await
	.expect("timed out waiting for the refresh");

	let consumer = MoqJsonWindowConsumer::new(&*subscribe(&broadcast, "log").await).unwrap();
	let events = [
		MoqJsonWindowEvent::Skip { start: 0, end: 3 },
		window_push(3, 3),
		window_push(4, 4),
	];
	for event in events {
		assert_eq!(
			next_window(&consumer).await,
			Some(MoqJsonWindowTimed {
				event,
				timestamp_us: None,
			})
		);
	}

	producer.finish().unwrap();
}

#[test]
fn json_window_refuses_zero_checkpoint_records() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("log".into(), None).unwrap();
	let config = MoqJsonWindowConfig {
		checkpoint_records: Some(0),
	};
	assert!(matches!(
		MoqJsonWindowProducer::new(&track, config),
		Err(MoqError::JsonTrack { .. })
	));
	assert!(track.demand().is_ok(), "a refused config leaves the track open");
}

/// The window producer reports subscriber demand, and drops it once finished.
#[tokio::test]
async fn json_window_demand() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = MoqJsonWindowConfig {
		checkpoint_records: None,
	};
	let track = broadcast.publish_track("log".into(), None).unwrap();
	let producer = MoqJsonWindowProducer::new(&track, config.clone()).unwrap();
	let demand = producer.demand().unwrap();
	assert_eq!(demand.name(), "log");
	assert!(demand.wanted().is_none());

	let consumer = MoqJsonWindowConsumer::new(&*subscribe(&broadcast, "log").await).unwrap();
	tokio::time::timeout(TIMEOUT, demand.until_wanted())
		.await
		.expect("timed out waiting for the json window to become used")
		.unwrap();
	drop(consumer);
	tokio::time::timeout(TIMEOUT, demand.until_unwanted())
		.await
		.expect("timed out waiting for the json window to become unused")
		.unwrap();

	producer.finish().unwrap();
	assert!(matches!(producer.demand(), Err(MoqError::Closed)));
	assert!(matches!(demand.until_wanted().await, Err(MoqError::Closed)));
}

/// A demand handle waits without the producer, and fails once the track is gone.
///
/// A raw track's `finish` keeps its handle open for a later `abort`, so its track ends when the
/// handle is released; a media producer's `finish` hands it to the broadcast, so its track ends
/// when the broadcast does.
#[tokio::test]
async fn demand_handle_outlives_finish() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, opus_head()),
	)
	.unwrap();
	let track_demand = track.demand().unwrap();
	let media_demand = media.demand().unwrap();
	assert_eq!(media_demand.name(), media.demand().unwrap().name());

	let consumer = track.consume(None).unwrap();
	tokio::time::timeout(TIMEOUT, track_demand.until_wanted())
		.await
		.expect("timed out waiting for the raw track to become used")
		.unwrap();
	drop(consumer);

	track.finish().unwrap();
	drop(track);
	media.finish().unwrap();
	broadcast.close().unwrap();
	let track_err = tokio::time::timeout(TIMEOUT, track_demand.until_wanted())
		.await
		.expect("timed out waiting for the finished raw track");
	let media_err = tokio::time::timeout(TIMEOUT, media_demand.until_wanted())
		.await
		.expect("timed out waiting for the finished media track");
	assert!(matches!(track_err, Err(MoqError::Closed)), "{track_err:?}");
	assert!(matches!(media_err, Err(MoqError::Closed)), "{media_err:?}");
}

#[tokio::test]
async fn json_stream_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let producer = MoqJsonStreamProducer::new(&broadcast, &track).unwrap();
	let consumer = MoqJsonStreamConsumer::new(&*subscribe(&broadcast, "events").await).unwrap();

	for n in 0..3 {
		producer.append(format!(r#"{{"n":{n}}}"#), None).unwrap();
		let value = tokio::time::timeout(TIMEOUT, consumer.next())
			.await
			.expect("timed out waiting for json stream record")
			.unwrap()
			.expect("expected a record");
		assert_eq!(
			serde_json::from_str::<serde_json::Value>(&value.value).unwrap(),
			serde_json::json!({ "n": n })
		);
		assert_eq!(value.timestamp_us, None, "an untimed track yields no timestamp");
	}
	producer.finish().unwrap();
}

/// A JSON producer serves a track a subscriber requested, which the broadcast never created by name.
#[tokio::test]
async fn json_on_requested_track() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();

	// The subscribe stays pending until the request is accepted, so run it concurrently.
	let subscribe = {
		let consumer = broadcast.consume().unwrap();
		tokio::spawn(async move { consumer.subscribe_track("events".into(), None).await })
	};
	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();
	let producer = MoqJsonStreamProducer::new(&broadcast, &request.accept(None).unwrap()).unwrap();
	let track = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for the subscribe")
		.unwrap()
		.unwrap();
	let consumer = MoqJsonStreamConsumer::new(&track).unwrap();

	producer.append(r#"{"n":1}"#.into(), None).unwrap();
	let value = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for json stream record")
		.unwrap()
		.expect("expected a record");
	assert_eq!(value.value, r#"{"n":1}"#);
}

/// A JSON consumer refuses a track that has already read a group, and closes one it takes over.
#[tokio::test]
async fn json_consumer_takes_an_unread_track() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();

	let read = broadcast
		.consume()
		.unwrap()
		.subscribe_track("events".into(), None)
		.await
		.unwrap();
	track.append_group().unwrap().finish().unwrap();
	tokio::time::timeout(TIMEOUT, read.recv_group())
		.await
		.expect("timed out waiting for a group")
		.unwrap()
		.expect("expected a group");
	assert!(matches!(
		MoqJsonStreamConsumer::new(&read),
		Err(MoqError::AlreadyCommitted)
	));

	let unread = subscribe(&broadcast, "events").await;
	MoqJsonStreamConsumer::new(&unread).unwrap();
	assert!(matches!(unread.recv_group().await, Err(MoqError::Cancelled)));
	assert!(matches!(MoqJsonStreamConsumer::new(&unread), Err(MoqError::Closed)));
}

#[tokio::test]
async fn track_request_demand_survives_accept() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();
	let subscribe = tokio::spawn(async move { consumer.subscribe_track("events".into(), None).await });
	let request = dynamic.requested_track().await.unwrap();
	let demand = request.demand().unwrap();
	assert_eq!(demand.name(), "events");
	assert!(demand.wanted().is_some());
	let track = request.accept(None).unwrap();
	assert!(matches!(request.demand(), Err(MoqError::Closed)));
	let consumer = subscribe.await.unwrap().unwrap();
	demand.until_wanted().await.unwrap();
	consumer.cancel();
	demand.until_unwanted().await.unwrap();
	let consumer = track.consume(None).unwrap();
	demand.until_wanted().await.unwrap();
	consumer.cancel();
	demand.until_unwanted().await.unwrap();
	// Released unfinished, the track aborts, and its watcher keeps the cause.
	drop(track);
	broadcast.close().unwrap();
	let Err(err) = demand.until_wanted().await else {
		panic!("the watcher outlived its track");
	};
	assert_protocol(
		&err,
		crate::error::MoqErrorScope::Stream,
		crate::error::MoqProtocolKind::Unroutable,
	);
}

#[tokio::test]
async fn track_request_demand_ends_on_rejection() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();
	let subscribe = tokio::spawn(async move { consumer.subscribe_track("events".into(), None).await });
	let request = dynamic.requested_track().await.unwrap();
	let demand = request.demand().unwrap();
	request.abort(404).unwrap();
	assert!(subscribe.await.unwrap().is_err());
	assert!(demand.until_wanted().await.is_err());
	assert!(matches!(request.demand(), Err(MoqError::Closed)));
}

#[tokio::test]
async fn dynamic_track_request() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();

	// The subscribe stays pending until the request is accepted below, so run it on a
	// concurrent task.
	let subscribe = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.subscribe_track("events".into(), None).await })
	};

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();
	assert_eq!(request.name().unwrap(), "events");

	// Accept the request as a raw track (which unblocks the subscribe), then write.
	let track = request.accept(Some(timed_info())).unwrap();
	let payload = b"hello dynamic track".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(0),
		})
		.unwrap();

	let track_consumer = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for subscribe")
		.expect("subscribe task panicked")
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, track_consumer.read_frame())
		.await
		.expect("timed out waiting for dynamic track frame")
		.unwrap()
		.expect("expected a frame");

	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(0));
	track.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn raw_untimed_frames_and_datagrams_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("untimed".into(), None).unwrap();
	let consumer = track.consume(None).unwrap();
	assert_eq!(consumer.info().timescale, None);
	track
		.write_frame(MoqFrame {
			payload: b"frame".to_vec(),
			timestamp_us: None,
		})
		.unwrap();
	let frame = consumer.read_frame().await.unwrap().unwrap();
	assert_eq!(frame.payload, b"frame");
	assert_eq!(frame.timestamp_us, None);
	let group = track.append_group().unwrap();
	let reader = group.consume().unwrap();
	group
		.write_frame(MoqFrame {
			payload: b"group".to_vec(),
			timestamp_us: None,
		})
		.unwrap();
	group.finish().unwrap();
	let frame = reader.read_frame().await.unwrap().unwrap();
	assert_eq!(frame.payload, b"group");
	assert_eq!(frame.timestamp_us, None);
	track
		.append_datagram(MoqFrame {
			payload: b"datagram".to_vec(),
			timestamp_us: None,
		})
		.unwrap();
	let datagram = consumer.recv_datagram().await.unwrap().unwrap();
	assert_eq!(datagram.payload, b"datagram");
	assert_eq!(datagram.timestamp_us, None);
	track.finish().unwrap();
}

/// An explicitly timed raw track refuses frames and datagrams without a timestamp.
#[test]
fn raw_writes_need_a_timestamp() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let untimed = || MoqFrame {
		payload: b"ready".to_vec(),
		timestamp_us: None,
	};
	let refused = |err: MoqError| {
		matches!(
			err,
			MoqError::Protocol {
				details: MoqProtocolError {
					kind: MoqProtocolKind::TimestampMismatch,
					..
				}
			}
		)
	};
	assert!(refused(track.write_frame(untimed()).unwrap_err()));
	assert!(refused(track.append_datagram(untimed()).unwrap_err()));
	let group = track.append_group().unwrap();
	assert!(refused(group.write_frame(untimed()).unwrap_err()));
}

#[tokio::test]
async fn raw_frame_timestamps() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	let payload = b"ready".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(12_345),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, consumer.read_frame())
		.await
		.expect("timed out waiting for raw track frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(12_345));

	let group = track.append_group().unwrap();
	let group_consumer = group.consume().unwrap();
	let payload = b"group frame".to_vec();
	group
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(23_456),
		})
		.unwrap();
	group.finish().unwrap();

	let frame = tokio::time::timeout(TIMEOUT, group_consumer.read_frame())
		.await
		.expect("timed out waiting for raw group frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(23_456));

	track.finish().unwrap();
}

#[test]
fn raw_track_supports_sparse_groups_and_known_end() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("sparse".into(), None).unwrap();

	let group = track.create_group(2).unwrap();
	assert_eq!(group.sequence(), 2);
	group.finish().unwrap();

	track.finish_at(5).unwrap();
	let group = track.create_group(4).unwrap();
	group.finish().unwrap();
	assert!(track.create_group(5).is_err());
	track.finish().unwrap();
}

#[tokio::test]
async fn abort_after_finish_keeps_the_track_handle() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("aborted".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	track
		.write_frame(MoqFrame {
			payload: b"late abort".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	track.finish().unwrap();
	assert!(consumer.read_frame().await.unwrap().is_some());
	track.abort(409).unwrap();
}

#[tokio::test]
async fn abort_after_finish_reaches_group_consumer() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("aborted".into(), Some(timed_info())).unwrap();
	let group = track.append_group().unwrap();
	let consumer = group.consume().unwrap();

	group
		.write_frame(MoqFrame {
			payload: b"late abort".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	group.finish().unwrap();
	group.abort(409).unwrap();

	assert!(consumer.read_frame().await.is_err());
}

#[tokio::test]
async fn raw_group_abort_reaches_consumer() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("aborted".into(), None).unwrap();
	let group = track.append_group().unwrap();
	let consumer = group.consume().unwrap();

	group.abort(409).unwrap();
	assert!(consumer.read_frame().await.is_err());
}

#[tokio::test]
async fn dynamic_track_request_can_abort() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();

	// The subscribe stays pending until the request is resolved; aborting an
	// unaccepted request rejects it, so the subscribe fails instead of succeeding.
	let subscribe = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.subscribe_track("unknown".into(), None).await })
	};

	let track = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();

	track.abort(404).unwrap();
	assert!(matches!(track.name(), Err(MoqError::Closed)));

	let result = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for subscribe")
		.expect("subscribe task panicked");
	assert!(result.is_err(), "subscribe to a rejected track should fail");
}

#[tokio::test]
async fn fetches_cached_group_without_subscribing() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), Some(timed_info())).unwrap();
	let group = track.append_group().unwrap();
	group
		.write_frame(MoqFrame {
			payload: b"first".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	group
		.write_frame(MoqFrame {
			payload: b"second".to_vec(),
			timestamp_us: Some(20_000),
		})
		.unwrap();
	group.finish().unwrap();

	let consumer = broadcast.consume().unwrap();
	let fetched = consumer
		.fetch_group(
			"events".into(),
			0,
			Some(MoqFetchGroupOptions {
				priority: 7,
				max_delay: None,
			}),
		)
		.await
		.unwrap();

	assert_eq!(fetched.sequence(), 0);
	let frame = fetched.read_frame().await.unwrap().expect("expected first frame");
	assert_eq!(frame.payload, b"first".to_vec());
	assert_eq!(frame.timestamp_us, Some(0));
	let frame = fetched.read_frame().await.unwrap().expect("expected second frame");
	assert_eq!(frame.payload, b"second".to_vec());
	assert_eq!(frame.timestamp_us, Some(20_000));
	assert!(fetched.read_frame().await.unwrap().is_none());
}

#[tokio::test]
async fn fetches_cached_media_group_and_decodes_container() {
	let broadcast = moq_net::broadcast::Info::new().produce();
	let track = broadcast.create_track("media", timed()).unwrap();
	let consumer = MoqBroadcastConsumer::new(broadcast.consume());
	let mut media = moq_mux::container::Producer::new(
		track,
		moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
	);

	media
		.write(moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(1_000_000).unwrap(),
			payload: bytes::Bytes::from_static(b"keyframe"),
			keyframe: true,
			duration: None,
		})
		.unwrap();
	media
		.write(moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(1_020_000).unwrap(),
			payload: bytes::Bytes::from_static(b"delta"),
			keyframe: false,
			duration: None,
		})
		.unwrap();
	media.finish().unwrap();

	let fetched = MoqMediaGroupConsumer::fetch(
		&consumer,
		MoqMediaGroupConfig {
			rendition: rendition("media", crate::media::MoqContainer::Legacy),
			sequence: 0,
			options: Some(MoqFetchGroupOptions {
				priority: 7,
				max_delay: None,
			}),
		},
	)
	.await
	.unwrap();

	assert_eq!(fetched.sequence(), 0);
	let frame = fetched.next().await.unwrap().expect("expected keyframe");
	assert_eq!(frame.payload, b"keyframe");
	assert_eq!(frame.timestamp_us, 1_000_000);
	assert!(frame.keyframe);
	let frame = fetched.next().await.unwrap().expect("expected delta frame");
	assert_eq!(frame.payload, b"delta");
	assert_eq!(frame.timestamp_us, 1_020_000);
	assert!(!frame.keyframe);
	assert!(fetched.next().await.unwrap().is_none());
}

#[tokio::test]
async fn fetch_media_group_rejects_invalid_container_before_fetching() {
	let broadcast = moq_net::broadcast::Info::new().produce();
	let _track = broadcast.create_track("media", timed()).unwrap();
	let consumer = MoqBroadcastConsumer::new(broadcast.consume());

	let result = MoqMediaGroupConsumer::fetch(
		&consumer,
		MoqMediaGroupConfig {
			rendition: rendition("media", crate::media::MoqContainer::Cmaf { init: Vec::new() }),
			sequence: 0,
			options: None,
		},
	)
	.await;

	assert!(matches!(result, Err(MoqError::Codec { .. })));
}

#[tokio::test]
async fn fetch_media_group_decodes_multiple_cmaf_samples() {
	let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	let muxer = moq_mux::container::fmp4::Muxer::video(&config).unwrap();
	let init = muxer.init().unwrap().expect("VP8 init should be available");
	let catalog_container = hang::catalog::Container::Cmaf { init: init.clone() };
	let container =
		moq_mux::catalog::hang::Container::new(&catalog_container, moq_mux::container::Kind::Video).unwrap();

	let broadcast = moq_net::broadcast::Info::new().produce();
	// A CMAF track counts in its init's ticks.
	let info = moq_net::track::Info::default().with_timescale(muxer.timescale());
	let track = broadcast.create_track("video", info).unwrap();
	let consumer = MoqBroadcastConsumer::new(broadcast.consume());
	// Buffer both samples into one moof+mdat, which is what this decodes.
	let mut media = moq_mux::container::Producer::new(track, container).with_buffer(Duration::from_secs(1));
	for (timestamp_us, payload, keyframe) in [
		(2_000_000, bytes::Bytes::from_static(b"keyframe"), true),
		(2_020_000, bytes::Bytes::from_static(b"delta"), false),
	] {
		media
			.write(moq_mux::container::Frame {
				timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
				payload,
				keyframe,
				duration: Some(moq_net::Timestamp::from_micros(20_000).unwrap()),
			})
			.unwrap();
	}
	media.finish().unwrap();

	let fetched = tokio::time::timeout(
		TIMEOUT,
		MoqMediaGroupConsumer::fetch(
			&consumer,
			MoqMediaGroupConfig {
				rendition: rendition("video", crate::media::MoqContainer::Cmaf { init: init.to_vec() }),
				sequence: 0,
				options: None,
			},
		),
	)
	.await
	.expect("timed out fetching CMAF group")
	.unwrap();

	let first = tokio::time::timeout(TIMEOUT, fetched.next())
		.await
		.expect("timed out reading first CMAF sample")
		.unwrap()
		.expect("expected first sample");
	assert_eq!(first.payload, b"keyframe");
	assert_eq!(first.timestamp_us, 2_000_000);
	assert!(first.keyframe);
	let second = tokio::time::timeout(TIMEOUT, fetched.next())
		.await
		.expect("timed out reading second CMAF sample")
		.unwrap()
		.expect("expected second sample");
	assert_eq!(second.payload, b"delta");
	assert_eq!(second.timestamp_us, 2_020_000);
	assert!(!second.keyframe);
	assert!(
		tokio::time::timeout(TIMEOUT, fetched.next())
			.await
			.expect("timed out finishing CMAF group")
			.unwrap()
			.is_none()
	);
}

/// Write one legacy-container frame whose transport timestamp disagrees with its media time,
/// the way a receiver that stamps arrival time would see it.
fn write_legacy(group: &mut moq_net::group::Producer, transport_us: u64, media_us: u64, payload: &'static [u8]) {
	let mut encoded = bytes::BytesMut::new();
	hang::container::Frame {
		timestamp: moq_net::Timestamp::from_micros(media_us).unwrap(),
		payload: bytes::Bytes::from_static(payload),
	}
	.encode(&mut encoded)
	.unwrap();
	write_raw(group, transport_us, encoded.freeze());
}

fn write_raw(group: &mut moq_net::group::Producer, transport_us: u64, payload: bytes::Bytes) {
	group
		.write_frame(moq_net::Timestamp::from_micros(transport_us).unwrap(), payload)
		.unwrap();
}

/// A broadcast with one legacy-container track, and a raw consumer subscribed to it.
async fn legacy_track() -> (
	moq_net::broadcast::Producer,
	moq_net::track::Producer,
	Arc<MoqTrackConsumer>,
) {
	let broadcast = moq_net::broadcast::Info::new().produce();
	let track = broadcast.create_track("audio", hang::container::track_info(0)).unwrap();
	let consumer = MoqBroadcastConsumer::new(broadcast.consume())
		.subscribe_track("audio".into(), None)
		.await
		.unwrap();
	(broadcast, track, consumer)
}

async fn recv_raw_group(track: &MoqTrackConsumer) -> Arc<MoqGroupConsumer> {
	tokio::time::timeout(TIMEOUT, track.recv_group())
		.await
		.expect("timed out waiting for a group")
		.unwrap()
		.expect("expected a group")
}

async fn recv_media_group(track: &MoqTrackConsumer) -> Arc<MoqMediaGroupConsumer> {
	let group = recv_raw_group(track).await;
	MoqMediaGroupConsumer::new(&group, MoqContainer::Legacy).unwrap()
}

async fn next_media(group: &MoqMediaGroupConsumer) -> Result<Option<MoqMediaFrame>, MoqError> {
	tokio::time::timeout(TIMEOUT, group.next())
		.await
		.expect("timed out reading a media frame")
}

/// Raw groups received in arrival order decode as their frames land: a stalled group is
/// neither skipped nor blocks a newer one, and each timestamp is the container's media time,
/// not the transport's.
#[tokio::test]
async fn media_group_decodes_arrival_order_groups() {
	let (_broadcast, track, consumer) = legacy_track().await;

	let mut first = track.append_group().unwrap();
	write_legacy(&mut first, 1, 1_000_000, b"a");
	let first_media = recv_media_group(&consumer).await;
	assert_eq!(first_media.sequence(), 0);
	let frame = next_media(&first_media).await.unwrap().expect("expected a frame");
	assert_eq!(frame.payload, b"a");
	assert_eq!(frame.timestamp_us, 1_000_000);
	assert!(frame.keyframe);

	// A newer group lands while the first is stalled mid-way.
	let mut second = track.append_group().unwrap();
	write_legacy(&mut second, 2, 1_040_000, b"c");
	let second_media = recv_media_group(&consumer).await;
	assert_eq!(second_media.sequence(), 1);
	let frame = next_media(&second_media).await.unwrap().expect("expected a frame");
	assert_eq!(frame.payload, b"c");
	assert_eq!(frame.timestamp_us, 1_040_000);

	// The stalled group resumes and is still delivered.
	write_legacy(&mut first, 3, 1_020_000, b"b");
	let frame = next_media(&first_media)
		.await
		.unwrap()
		.expect("expected the stalled group's frame");
	assert_eq!(frame.payload, b"b");
	assert_eq!(frame.timestamp_us, 1_020_000);
	assert!(!frame.keyframe);

	first.finish().unwrap();
	second.finish().unwrap();
	assert!(next_media(&first_media).await.unwrap().is_none());
	assert!(next_media(&second_media).await.unwrap().is_none());
}

/// Decoding takes over the raw group, and refuses one that already read a frame, since the
/// decoder would start mid-group. A refusal leaves the raw handle readable.
#[tokio::test]
async fn media_group_takes_over_an_unread_group() {
	let (_broadcast, track, consumer) = legacy_track().await;
	let mut group = track.append_group().unwrap();
	write_legacy(&mut group, 1, 1_000_000, b"a");
	write_legacy(&mut group, 2, 1_020_000, b"b");
	group.finish().unwrap();

	let raw = recv_raw_group(&consumer).await;
	assert!(raw.read_frame().await.unwrap().is_some());
	assert!(matches!(
		MoqMediaGroupConsumer::new(&raw, MoqContainer::Legacy),
		Err(MoqError::AlreadyCommitted)
	));
	assert!(
		raw.read_frame().await.unwrap().is_some(),
		"the refusal kept the raw handle"
	);

	let mut group = track.append_group().unwrap();
	write_legacy(&mut group, 3, 1_040_000, b"c");
	let raw = recv_raw_group(&consumer).await;
	let media = MoqMediaGroupConsumer::new(&raw, MoqContainer::Legacy).unwrap();
	assert!(matches!(raw.read_frame().await, Err(MoqError::Cancelled)));
	assert_eq!(
		next_media(&media).await.unwrap().expect("expected a frame").payload,
		b"c"
	);
}

/// A container that can't be configured fails before taking the group, and a frame the
/// container can't decode fails the read rather than being passed through.
#[tokio::test]
async fn media_group_fails_loudly_on_bad_containers() {
	let (_broadcast, track, consumer) = legacy_track().await;
	let mut group = track.append_group().unwrap();
	// A truncated varint timestamp: the 2-byte form with only its first byte.
	write_raw(&mut group, 1, bytes::Bytes::from_static(&[0x40]));

	let raw = recv_raw_group(&consumer).await;
	assert!(matches!(
		MoqMediaGroupConsumer::new(&raw, MoqContainer::Cmaf { init: Vec::new() }),
		Err(MoqError::Codec { .. })
	));
	let media = MoqMediaGroupConsumer::new(&raw, MoqContainer::Legacy).unwrap();
	assert!(next_media(&media).await.is_err());
}

#[tokio::test]
async fn dynamic_track_serves_fetch_miss_and_priority() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), Some(timed_info())).unwrap();
	let dynamic = track.requests().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move {
		consumer
			.fetch_group(
				"events".into(),
				5,
				Some(MoqFetchGroupOptions {
					priority: 11,
					max_delay: None,
				}),
			)
			.await
	});

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_fill())
		.await
		.expect("timed out waiting for fill")
		.unwrap();
	assert_eq!(
		(request.start_group(), request.start_frame(), request.end_group()),
		(5, 0, 6)
	);
	assert_eq!(request.priority(), 11);
	assert_eq!(request.next(), Some(5));

	let group = request.accept(5).unwrap();
	group
		.write_frame(MoqFrame {
			payload: b"fetched".to_vec(),
			timestamp_us: Some(100_000),
		})
		.unwrap();
	group.finish().unwrap();

	let fetched = tokio::time::timeout(TIMEOUT, fetch)
		.await
		.expect("timed out waiting for fetch")
		.expect("fetch task panicked")
		.unwrap();
	assert_eq!(fetched.sequence(), 5);
	let frame = fetched.read_frame().await.unwrap().expect("expected fetched frame");
	assert_eq!(frame.payload, b"fetched".to_vec());
	assert_eq!(frame.timestamp_us, Some(100_000));
}

#[tokio::test]
async fn fill_goes_unused_when_the_fetcher_leaves() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let dynamic = track.requests().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move { consumer.fetch_group("events".into(), 5, None).await });
	let fill = tokio::time::timeout(TIMEOUT, dynamic.requested_fill())
		.await
		.expect("timed out waiting for fill")
		.unwrap();
	assert!(fill.is_used());
	assert!(fill.is_wanted(5));
	assert!(!fill.is_wanted(6), "outside the fill");

	fetch.abort();
	tokio::time::timeout(TIMEOUT, fill.unused())
		.await
		.expect("timed out waiting for the abandoned fetch to become unused")
		.unwrap();
	assert!(!fill.is_used());
	tokio::time::timeout(TIMEOUT, fill.unwanted(5))
		.await
		.expect("an unused fill wants none of its groups")
		.unwrap();
	assert!(!fill.is_wanted(5));
}

#[tokio::test]
async fn dynamic_track_rejects_fetch_miss() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let dynamic = track.requests().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move { consumer.fetch_group("events".into(), 5, None).await });
	let fill = tokio::time::timeout(TIMEOUT, dynamic.requested_fill())
		.await
		.expect("timed out waiting for fill")
		.unwrap();
	fill.drop_groups(5, 6);
	assert_eq!(fill.next(), None, "the dropped group is answered");

	let result = tokio::time::timeout(TIMEOUT, fetch)
		.await
		.expect("timed out waiting for rejected fetch")
		.expect("fetch task panicked");
	assert!(matches!(result, Err(MoqError::NotFound)), "got {:?}", result.err());
	assert!(fill.accept(5).is_err(), "a dropped group is answered already");
}

#[tokio::test]
async fn fetch_miss_without_dynamic_is_not_found() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let _track = broadcast.publish_track("events".into(), None).unwrap();
	let consumer = broadcast.consume().unwrap();

	let result = consumer.fetch_group("events".into(), 5, None).await;
	assert!(matches!(result, Err(MoqError::NotFound)));
}

#[tokio::test]
async fn fetch_unknown_track_is_not_found() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();

	let result = consumer.fetch_group("missing".into(), 0, None).await;
	assert!(matches!(result, Err(MoqError::NotFound)));
}

#[tokio::test]
async fn requested_track_dynamic_survives_accept() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let broadcast_dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move { consumer.fetch_group("archive".into(), 9, None).await });
	let request = tokio::time::timeout(TIMEOUT, broadcast_dynamic.requested_track())
		.await
		.expect("timed out waiting for track request")
		.unwrap();
	let track_dynamic = request.requests().unwrap();
	let _track = request.accept(Some(timed_info())).unwrap();

	let fill = tokio::time::timeout(TIMEOUT, track_dynamic.requested_fill())
		.await
		.expect("timed out waiting for fill")
		.unwrap();
	assert_eq!(fill.start_group(), 9);
	let group = fill.accept(9).unwrap();
	group
		.write_frame(MoqFrame {
			payload: b"archive".to_vec(),
			timestamp_us: Some(180_000),
		})
		.unwrap();
	group.finish().unwrap();

	let fetched = tokio::time::timeout(TIMEOUT, fetch)
		.await
		.expect("timed out waiting for fetch")
		.expect("fetch task panicked")
		.unwrap();
	let frame = fetched.read_frame().await.unwrap().expect("expected archive frame");
	assert_eq!(frame.payload, b"archive".to_vec());
	assert_eq!(frame.timestamp_us, Some(180_000));
}

#[tokio::test]
async fn video_publish_named_track() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();

	let init = || video_init(MoqVideoFormat::Avc3, h264_init());
	let hd = MoqMediaTrackProducer::video(
		&broadcast,
		MoqMediaTarget::Named {
			name: Some("hd".into()),
		},
		init(),
	)
	.unwrap();
	assert_eq!(hd.demand().unwrap().name(), "hd");
	let sd = MoqMediaTrackStreamProducer::video(
		&broadcast,
		MoqMediaTarget::Named {
			name: Some("sd".into()),
		},
		init(),
	)
	.unwrap();
	assert_eq!(sd.demand().unwrap().name(), "sd");
	sd.finish().unwrap();
	assert!(matches!(sd.demand(), Err(MoqError::Closed)));
	drop(sd);

	// A name is the caller's contract, so a duplicate fails rather than being made unique.
	assert!(matches!(
		MoqMediaTrackProducer::video(
			&broadcast,
			MoqMediaTarget::Named {
				name: Some("hd".into())
			},
			init()
		),
		Err(MoqError::Codec { .. })
	));

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	assert!(catalog.video.contains_key("hd"), "catalog: {:?}", catalog.video.keys());
}

/// A keyframe whose SPS names another level is a new decoder identity, so the track producer moves
/// to a new track, and its demand follows rather than watching the one that ended.
#[tokio::test]
async fn video_track_demand_follows_a_new_level() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let hd = MoqMediaTrackProducer::video(
		&broadcast,
		MoqMediaTarget::Named {
			name: Some("hd".into()),
		},
		video_init(MoqVideoFormat::Avc3, h264_init()),
	)
	.unwrap();
	let keyframe = |level: u8| {
		let mut bytes = h264_init();
		// The SPS's level_idc, after its NAL header, profile, and constraint bytes.
		bytes[7] = level;
		bytes.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
		bytes
	};
	hd.write_frame(MoqFrame {
		payload: keyframe(0x1f),
		timestamp_us: Some(0),
	})
	.unwrap();
	assert_eq!(hd.demand().unwrap().name(), "hd");
	hd.write_frame(MoqFrame {
		payload: keyframe(0x28),
		timestamp_us: Some(40_000),
	})
	.unwrap();
	assert_eq!(hd.demand().unwrap().name(), "hd/1");
	hd.finish().unwrap();
}

/// The raw-stream producer frames Annex-B written in arbitrary chunks, and
/// `finish` publishes the last access unit even with no start code after it.
#[tokio::test]
async fn track_stream_producer_publishes_its_last_access_unit() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let stream = MoqMediaTrackStreamProducer::video(
		&broadcast,
		MoqMediaTarget::Named {
			name: Some("video".into()),
		},
		video_init(MoqVideoFormat::Avc3, Vec::new()),
	)
	.unwrap();
	let track = consumer.subscribe_track("video".into(), None).await.unwrap();

	let idr = [0x65, 0x88, 0x84, 0x21];
	let mut bytes = h264_init();
	bytes.extend_from_slice(&[0, 0, 0, 1]);
	bytes.extend_from_slice(&idr);
	for chunk in bytes.chunks(5) {
		stream.write(chunk.to_vec()).unwrap();
	}
	stream.finish().unwrap();

	let frame = tokio::time::timeout(TIMEOUT, track.read_frame())
		.await
		.expect("timed out waiting for the access unit")
		.unwrap()
		.expect("finish dropped the last access unit");
	assert!(frame.payload.ends_with(&idr), "{:02x?}", frame.payload);
}

#[tokio::test]
async fn requested_track_keeps_its_name() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();
	let subscribe = tokio::spawn(async move {
		MoqMediaTrackConsumer::subscribe(
			&consumer,
			MoqMediaTrackConfig {
				rendition: rendition("requested", crate::media::MoqContainer::Legacy),
				subscription: None,
			},
		)
		.await
	});

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();

	let media = MoqMediaTrackProducer::video(
		&broadcast,
		MoqMediaTarget::Requested {
			request: request.clone(),
		},
		video_init(MoqVideoFormat::Avc3, h264_init()),
	)
	.unwrap();
	assert_eq!(media.demand().unwrap().name(), "requested");
	subscribe.abort();
}

#[tokio::test]
async fn dynamic_track_request_can_publish_media() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();

	// Importing onto the request accepts it (at the media timescale), which is what
	// unblocks the media subscribe, so it runs on a concurrent task until then.
	let subscribe = {
		let consumer = consumer.clone();
		tokio::spawn(async move {
			MoqMediaTrackConsumer::subscribe(
				&consumer,
				MoqMediaTrackConfig {
					rendition: rendition("requested-audio", crate::media::MoqContainer::Legacy),
					subscription: None,
				},
			)
			.await
		})
	};

	let track = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();
	assert_eq!(track.name().unwrap(), "requested-audio");

	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Requested { request: track.clone() },
		audio_init(MoqAudioFormat::Opus, opus_head()),
	)
	.unwrap();
	assert_eq!(media.demand().unwrap().name(), "requested-audio");
	assert!(matches!(track.name(), Err(MoqError::Closed)));

	let media_consumer = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for subscribe")
		.expect("subscribe task panicked")
		.unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	let audio = catalog
		.audio
		.get("requested-audio")
		.expect("requested track should be in catalog");
	assert_eq!(audio.codec, "opus");
	assert_eq!(audio.sample_rate, 48000);
	assert_eq!(audio.channel_count, 2);

	let payload = b"dynamic opus frame".to_vec();
	media
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(20_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&media_consumer))
		.await
		.expect("timed out waiting for media frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 20_000);

	media.discontinuity().unwrap();
	media.finish().unwrap();
	assert!(matches!(media.discontinuity(), Err(MoqError::Closed)));
}

#[tokio::test]
async fn media_track_activity_and_name() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	)
	.unwrap();
	let track_name = media.demand().unwrap().name();
	assert_eq!(track_name, "0.opus");

	let broadcast_consumer = broadcast.consume().unwrap();
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	assert!(catalog.audio.contains_key(&track_name));

	let track_consumer = broadcast_consumer.subscribe_track(track_name, None).await.unwrap();
	tokio::time::timeout(TIMEOUT, media.demand().unwrap().until_wanted())
		.await
		.expect("timed out waiting for media track to become used")
		.unwrap();

	drop(track_consumer);
	tokio::time::timeout(TIMEOUT, media.demand().unwrap().until_unwanted())
		.await
		.expect("timed out waiting for media track to become unused")
		.unwrap();
}

#[tokio::test]
async fn publish_media_aac_populates_description() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = moq_mux::codec::aac::Config {
		profile: 2,
		sample_rate: 44_100,
		channel_count: 2,
	};
	let init = config.encode().unwrap();
	let _media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Aac, init.to_vec()),
	)
	.unwrap();

	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&consumer).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");

	assert_eq!(catalog.audio.len(), 1);
	let audio = catalog.audio.values().next().unwrap();
	assert_eq!(audio.codec, "mp4a.40.2");
	assert_eq!(audio.sample_rate, config.sample_rate);
	assert_eq!(audio.channel_count, config.channel_count);
	assert_eq!(audio.description.as_deref(), Some(init.as_ref()));
}

/// Audio resolves its rendition from the init bytes, so bad ones fail here rather than surfacing
/// as a decode error on the first frame. (An unrecognized *format* can no longer be expressed: it
/// is an enum, and moq-mux owns the string boundary where that check still means something.)
#[test]
fn audio_rejects_bad_init_bytes() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let err = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, vec![]),
	)
	.err()
	.expect("an OpusHead-less opus track should fail");
	assert!(
		matches!(err, crate::error::MoqError::Codec { .. }),
		"expected Codec error, got {err}"
	);
}

/// Creating a broadcast does not make it exist for anyone: nothing crosses the
/// announce cursor and nothing resolves until `announce`, locally exactly as for a
/// peer. `announced_broadcast` waits for the announcement.
#[tokio::test]
async fn create_broadcast_is_invisible_until_announced() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let publication = origin.create_broadcast("live".into()).unwrap();

	let unroutable = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("live".into()))
		.await
		.expect("an unroutable request answers at once");
	assert!(unroutable.is_err(), "an unannounced broadcast is unroutable");

	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let pending = tokio::spawn(async move { announced.next().await });
	let waiting = consumer.announced_broadcast("live".into()).unwrap();
	let waited = tokio::spawn(async move { waiting.available().await });
	tokio::time::sleep(std::time::Duration::from_millis(50)).await;
	assert!(!pending.is_finished(), "create_broadcast must not advertise the path");
	assert!(!waited.is_finished(), "an unannounced broadcast must not resolve");

	publication.announce(MoqAnnounceRoute::default()).unwrap();
	tokio::time::timeout(TIMEOUT, waited)
		.await
		.expect("timed out waiting for the announced broadcast")
		.expect("task")
		.expect("the announced broadcast resolves");
	let update = tokio::time::timeout(TIMEOUT, pending)
		.await
		.expect("timed out waiting for announce")
		.expect("task")
		.expect("announce reaches the cursor")
		.expect("the cursor is still open");
	assert!(
		matches!(&update, MoqAnnounceEvent::Start { announce } if announce.prefix == "live"),
		"{update:?}"
	);
	publication.broadcast().close().unwrap();
}

/// Waiting for an exact path must hand the broadcast back named by that path, the base a
/// catalog's relative `broadcast` references resolve against. Implementing the wait by
/// rooting the cursor *at* the path would name it "", making the broadcast its own root, so
/// a legal `../sibling` reference would read as escaping for every binding built on this.
#[tokio::test]
async fn announced_broadcast_keeps_the_requested_path() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let broadcast = create_announced(&origin, "a/pub");

	let announced = consumer.announced_broadcast("a/pub".into()).unwrap();
	let waited = tokio::time::timeout(TIMEOUT, announced.available())
		.await
		.expect("timed out waiting for the announcement")
		.unwrap();
	assert_eq!(waited.inner().info().path.as_str(), "a/pub");

	// The same broadcast reached by request names itself identically.
	let requested = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("a/pub".into()))
		.await
		.expect("timed out requesting the broadcast")
		.unwrap();
	assert_eq!(requested.inner().info().path.as_str(), "a/pub");

	broadcast.close().unwrap();
}

/// A catalog rendition may name a sibling broadcast (`./source`), and the track then lives
/// there, not on the broadcast the catalog came from. Dropping the reference either fails to
/// find the track or silently decodes a same-named local one with mismatched metadata.
#[cfg(feature = "audio")]
#[tokio::test]
async fn audio_consumer_follows_a_sibling_broadcast_reference() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();

	// The catalog's broadcast deliberately has no "audio" track: only the sibling serves it.
	let catalog = create_announced(&origin, "a/pub");
	let source = create_announced(&origin, "a/source");
	let _audio = source.publish_track("audio".into(), None).unwrap();

	// Both, not just the catalog broadcast: the reference resolves against `a/source`.
	let broadcast = await_announced(&consumer, "a/pub").await;
	await_announced(&consumer, "a/source").await;

	let decoded = tokio::time::timeout(
		TIMEOUT,
		crate::audio::MoqAudioConsumer::subscribe(
			&broadcast,
			"audio".into(),
			sibling_audio("./source"),
			audio_output(),
		),
	)
	.await
	.expect("timed out subscribing to the referenced track");
	decoded.expect("the rendition's broadcast reference should resolve to the sibling");

	// Without the reference the same call has nowhere to find the track.
	let missing = tokio::time::timeout(
		TIMEOUT,
		crate::audio::MoqAudioConsumer::subscribe(&broadcast, "audio".into(), sibling_audio(""), audio_output()),
	)
	.await
	.expect("timed out subscribing to the catalog broadcast");
	assert!(
		missing.is_err(),
		"the catalog broadcast does not serve the track itself"
	);

	catalog.close().unwrap();
	source.close().unwrap();
}

/// A container subscription and a group fetch take the rendition, not a track name, and follow
/// both its broadcast reference and its `track`. Every other pairing of broadcast and name serves
/// a decoy, so dropping either half of the rendition reads the wrong content.
#[tokio::test]
async fn media_consumers_follow_a_rendition_track_in_a_sibling_broadcast() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let catalog = create_announced(&origin, "a/pub");
	let source = create_announced(&origin, "a/source");

	let publish = |broadcast: &MoqBroadcastProducer, name: &str, payload: &'static [u8]| {
		let track = broadcast
			.with_state(|state| Ok(state.broadcast.create_track(name, timed()).unwrap()))
			.unwrap();
		let mut media = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		media
			.write(moq_mux::container::Frame {
				timestamp: moq_net::Timestamp::from_micros(0).unwrap(),
				payload: bytes::Bytes::from_static(payload),
				keyframe: true,
				duration: None,
			})
			.unwrap();
		media
	};
	let _right = publish(&source, "camera", b"right");
	let _decoys = [
		publish(&source, "hd", b"wrong"),
		publish(&catalog, "hd", b"wrong"),
		publish(&catalog, "camera", b"wrong"),
	];

	let broadcast = await_announced(&consumer, "a/pub").await;
	await_announced(&consumer, "a/source").await;

	let MoqRendition::Video { mut config, .. } = rendition("hd", MoqContainer::Legacy) else {
		unreachable!()
	};
	config.broadcast = Some("./source".into());
	config.track = Some("camera".into());
	let rendition = MoqRendition::Video {
		id: "hd".into(),
		config,
	};

	let live = tokio::time::timeout(
		TIMEOUT,
		MoqMediaTrackConsumer::subscribe(
			&broadcast,
			MoqMediaTrackConfig {
				rendition: rendition.clone(),
				subscription: None,
			},
		),
	)
	.await
	.expect("timed out subscribing to the rendition")
	.unwrap();
	let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&live))
		.await
		.expect("timed out reading the rendition")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, b"right");

	let fetched = tokio::time::timeout(
		TIMEOUT,
		MoqMediaGroupConsumer::fetch(
			&broadcast,
			MoqMediaGroupConfig {
				rendition,
				sequence: 0,
				options: None,
			},
		),
	)
	.await
	.expect("timed out fetching the rendition")
	.unwrap();
	let frame = tokio::time::timeout(TIMEOUT, fetched.next())
		.await
		.expect("timed out reading the fetched group")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, b"right");

	catalog.close().unwrap();
	source.close().unwrap();
}

/// Announcement filters do not re-root the origin, so relative broadcast references
/// keep the same meaning as they have on the unfiltered consumer.
#[tokio::test]
async fn announced_broadcasts_resolve_siblings_under_the_prefix() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let announced = consumer
		.announced(MoqAnnounceConfig {
			prefix: "a/".into(),
			filter: None,
			hidden: false,
		})
		.unwrap();

	let catalog = create_announced(&origin, "a/pub");
	let source = create_announced(&origin, "a/source");
	let _video = source.publish_track("video".into(), None).unwrap();

	// `a/source` may be announced first, so keep reading until the catalog broadcast arrives.
	let broadcast = loop {
		let announcement = next_announced(&announced).await;
		let prefix = announcement.prefix;
		assert!(
			matches!(prefix.as_str(), "a/pub" | "a/source"),
			"covered prefix should stay relative to the origin: {prefix}"
		);
		assert_eq!(
			announcement.captures,
			Some(vec![prefix.strip_prefix("a/").unwrap().to_string()]),
			"the implicit trailing glob captures the suffix beneath the literal prefix"
		);
		if prefix == "a/pub" {
			break await_announced(&consumer, "a/pub").await;
		}
	};

	await_announced(&consumer, "a/source").await;

	let sibling = tokio::time::timeout(TIMEOUT, broadcast.resolve(referencing(Some("./source"), None)))
		.await
		.expect("timed out resolving the reference")
		.unwrap();
	tokio::time::timeout(TIMEOUT, sibling.subscribe_track("video".into(), None))
		.await
		.expect("timed out subscribing on the resolved broadcast")
		.unwrap();

	catalog.close().unwrap();
	source.close().unwrap();
}

#[tokio::test]
async fn announced_filters_patterns_and_reports_captures() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let announced = consumer
		.announced(MoqAnnounceConfig {
			prefix: "room".into(),
			filter: Some("*/chat".into()),
			hidden: false,
		})
		.unwrap();

	let _broad = origin.requests("room".into()).unwrap();
	let overlap = next_announced(&announced).await;
	assert_eq!(overlap.prefix, "room");
	assert_eq!(overlap.captures, None, "an overlap does not pin the wildcard");

	let chat = create_announced(&origin, "room/alice/chat");
	let _audio = create_announced(&origin, "room/alice/audio");
	let update = next_announced(&announced).await;

	assert_eq!(update.prefix, "room/alice/chat");
	assert_eq!(update.captures, Some(vec!["alice".into()]));

	chat.close().unwrap();
}

/// A `.`-named broadcast is listed only when the config opts in or the prefix names it.
#[tokio::test]
async fn announced_hides_dot_paths_unless_asked() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let _stats = create_announced(&origin, ".stats/node");
	let _cam = create_announced(&origin, "cam");

	for (prefix, hidden, expected) in [
		("", false, "cam"),
		("", true, ".stats/node"),
		(".stats", false, ".stats/node"),
	] {
		let announced = consumer
			.announced(MoqAnnounceConfig {
				prefix: prefix.into(),
				filter: None,
				hidden,
			})
			.unwrap();
		// Updates arrive in path order, and `.` sorts before letters.
		let update = next_announced(&announced).await;
		assert_eq!(update.prefix, expected, "prefix {prefix:?}, hidden {hidden}");
	}
}

/// A broadcast consumed straight from a local producer has no origin, so a rendition naming a
/// sibling names nothing. Reporting that beats silently reading the catalog's own broadcast.
#[tokio::test]
async fn resolve_rejects_a_reference_without_an_origin() {
	let broadcast = moq_net::broadcast::Info::new().produce();
	let _audio = broadcast.create_track("audio", timed()).unwrap();
	let consumer = MoqBroadcastConsumer::new(broadcast.consume());

	// An absent or empty reference still names this broadcast, so it needs no origin. A reference
	// made only of slashes normalizes to the empty one, so it must be treated the same rather than
	// looking like a cross-broadcast reference.
	consumer.resolve(referencing(None, None)).await.unwrap();
	consumer.resolve(referencing(Some(""), None)).await.unwrap();
	consumer.resolve(referencing(Some("/"), None)).await.unwrap();

	// Reported in normalized form, matching `EscapingBroadcast` and moq-c: it names what the
	// resolver actually tried to reach, not the caller's spelling of it.
	match consumer.resolve(referencing(Some("./source"), None)).await {
		Err(MoqError::UnresolvableBroadcast { reason: reference }) => assert_eq!(reference, "source"),
		Err(err) => panic!("wrong error for an unresolvable reference: {err:?}"),
		Ok(_) => panic!("a standalone broadcast cannot resolve a sibling"),
	}
}

/// A resolved sibling carries the origin too, so its own catalog's references keep resolving.
#[tokio::test]
async fn resolve_returns_a_broadcast_that_resolves_further_references() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();

	let catalog = create_announced(&origin, "a/pub");
	let source = create_announced(&origin, "a/source");
	let _video = source.publish_track("video".into(), None).unwrap();

	let broadcast = await_announced(&consumer, "a/pub").await;
	await_announced(&consumer, "a/source").await;

	let sibling = tokio::time::timeout(TIMEOUT, broadcast.resolve(referencing(Some("./source"), None)))
		.await
		.expect("timed out resolving the reference")
		.unwrap();
	assert_eq!(sibling.inner().info().path.as_str(), "a/source");

	// The sibling is a full consumer: the referenced track subscribes on it directly.
	tokio::time::timeout(TIMEOUT, sibling.subscribe_track("video".into(), None))
		.await
		.expect("timed out subscribing on the resolved broadcast")
		.unwrap();

	// And it can follow a reference of its own, back to where we started.
	let back = tokio::time::timeout(TIMEOUT, sibling.resolve(referencing(Some("./pub"), None)))
		.await
		.expect("timed out resolving back")
		.unwrap();
	assert_eq!(back.inner().info().path.as_str(), "a/pub");

	catalog.close().unwrap();
	source.close().unwrap();
}

/// A reference's epoch names the instance it expects: a broadcast proven to serve another fails
/// the rendition, while an epochless reference, or a route that cannot prove its instance, reads
/// whichever instance resolves.
#[tokio::test]
async fn resolve_honors_the_reference_epoch() {
	async fn resolve(
		broadcast: &MoqBroadcastConsumer,
		reference: &str,
		epoch: Option<&str>,
	) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		tokio::time::timeout(TIMEOUT, broadcast.resolve(referencing(Some(reference), epoch)))
			.await
			.expect("timed out resolving the reference")
	}

	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let served = moq_mint_epoch();
	let other = moq_mint_epoch();
	let route = MoqAnnounceRoute {
		epoch: Some(served.clone()),
		..Default::default()
	};

	let catalog = origin.create_broadcast("a/pub".into()).unwrap();
	catalog.announce(route.clone()).unwrap();
	let catalog = catalog.broadcast();
	let source = origin.create_broadcast("a/source".into()).unwrap();
	source.announce(route).unwrap();
	let source = source.broadcast();
	let _video = source.publish_track("video".into(), None).unwrap();
	let plain = create_announced(&origin, "a/plain");

	let broadcast = await_announced(&consumer, "a/pub").await;
	await_announced(&consumer, "a/source").await;
	await_announced(&consumer, "a/plain").await;

	let matching = resolve(&broadcast, "./source", Some(&served)).await.unwrap();
	assert_eq!(matching.inner().info().path.as_str(), "a/source");
	resolve(&broadcast, "./source", None).await.unwrap();
	resolve(&broadcast, "./plain", Some(&other)).await.unwrap();

	assert!(matches!(
		resolve(&broadcast, "./source", Some(&other)).await,
		Err(MoqError::Mux { reason }) if reason.contains("serves epoch")
	));
	// Without a broadcast, the epoch names the catalog's own instance.
	resolve(&broadcast, "", Some(&served)).await.unwrap();
	assert!(matches!(
		resolve(&broadcast, "", Some(&other)).await,
		Err(MoqError::Mux { reason }) if reason.contains("serves epoch")
	));
	assert!(matches!(
		resolve(&broadcast, "./source", Some("not-an-epoch")).await,
		Err(MoqError::InvalidEpoch { .. })
	));

	// The rendition-based consumers read the epoch off the rendition.
	let config = MoqMediaTrackConfig {
		rendition: referencing(Some("./source"), Some(&other)),
		subscription: None,
	};
	let subscribed = tokio::time::timeout(TIMEOUT, MoqMediaTrackConsumer::subscribe(&broadcast, config))
		.await
		.expect("timed out subscribing to the rendition");
	assert!(matches!(subscribed, Err(MoqError::Mux { reason }) if reason.contains("serves epoch")));

	catalog.close().unwrap();
	source.close().unwrap();
	plain.close().unwrap();
}

#[tokio::test]
async fn announce_and_unannounce_toggles_discovery() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let publication = origin.create_broadcast("live".into()).unwrap();
	publication.announce(MoqAnnounceRoute::default()).unwrap();

	// The consumer observes the flag through the announce stream: an active
	// announcement, then its retraction.
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	async fn wait_live(announced: &MoqAnnounceConsumer, active: bool) {
		loop {
			match next_event(announced).await {
				MoqAnnounceEvent::Start { announce } if active && announce.prefix == "live" => return,
				MoqAnnounceEvent::End { announce } if !active && announce.prefix == "live" => return,
				_ => {}
			}
		}
	}
	wait_live(&announced, true).await;

	publication.unannounce().unwrap();
	wait_live(&announced, false).await;

	// Unannounced for local consumers exactly as for peers.
	let unroutable = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("live".into()))
		.await
		.expect("an unroutable request answers at once");
	assert!(unroutable.is_err(), "an unannounced broadcast is unroutable");

	// Announcing again brings it back.
	publication.announce(MoqAnnounceRoute::default()).unwrap();
	wait_live(&announced, true).await;
	tokio::time::timeout(TIMEOUT, consumer.request_broadcast("live".into()))
		.await
		.expect("timed out requesting the reannounced broadcast")
		.expect("a reannounced broadcast resolves");

	publication.broadcast().close().unwrap();
}

/// The publication and the fill handle each keep the broadcast and its route alive: dropping
/// either alone retracts nothing, and only the close does.
#[tokio::test]
async fn dropping_a_publication_keeps_its_route() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let publication = origin.create_broadcast("live".into()).unwrap();
	publication.announce(MoqAnnounceRoute::default()).unwrap();
	let broadcast = publication.broadcast();
	assert!(Arc::ptr_eq(&broadcast, &publication.broadcast()));
	drop(publication);
	await_announced(&consumer, "live").await;

	let publication = origin.create_broadcast("held".into()).unwrap();
	publication.announce(MoqAnnounceRoute::default()).unwrap();
	drop(publication.broadcast());
	await_announced(&consumer, "held").await;

	broadcast.close().unwrap();
	publication.broadcast().close().unwrap();
	for path in ["live", "held"] {
		let unroutable = tokio::time::timeout(TIMEOUT, consumer.request_broadcast(path.into()))
			.await
			.expect("an unroutable request answers at once");
		assert!(unroutable.is_err(), "a closed broadcast is unroutable");
	}
}

/// A publication outliving its closed broadcast cannot announce or withdraw the broadcast that
/// replaced it at the same path.
#[tokio::test]
async fn a_stale_publication_leaves_its_replacement_alone() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let stale = origin.create_broadcast("live".into()).unwrap();
	stale.announce(MoqAnnounceRoute::default()).unwrap();
	stale.broadcast().close().unwrap();

	let replacement = origin.create_broadcast("live".into()).unwrap();
	replacement.announce(MoqAnnounceRoute::default()).unwrap();
	assert!(matches!(
		stale.announce(MoqAnnounceRoute::default()),
		Err(MoqError::Closed)
	));
	assert!(matches!(stale.unannounce(), Err(MoqError::Closed)));
	tokio::time::timeout(TIMEOUT, consumer.request_broadcast("live".into()))
		.await
		.expect("timed out requesting the replacement")
		.expect("the replacement still resolves");

	replacement.broadcast().close().unwrap();
}

#[tokio::test]
async fn finish_unpublishes() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let broadcast = create_announced(&origin, "live");

	let announced = consumer.announced_broadcast("live".into()).unwrap();
	tokio::time::timeout(TIMEOUT, announced.available())
		.await
		.expect("timed out waiting for the announcement")
		.unwrap();

	// A graceful finish detaches immediately; the path stops resolving. Removal is
	// asynchronous, so poll until it takes effect.
	broadcast.close().unwrap();
	let removed = tokio::time::timeout(TIMEOUT, async {
		loop {
			if consumer.request_broadcast("live".into()).await.is_err() {
				return;
			}
			tokio::time::sleep(std::time::Duration::from_millis(10)).await;
		}
	})
	.await;
	assert!(removed.is_ok(), "close should unpublish the broadcast");
}

#[tokio::test]
async fn local_publish_consume_audio() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "live");
	let init = opus_head();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	)
	.unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();

	let announcement = next_announced(&announced).await;

	assert_eq!(announcement.prefix, "live");

	let broadcast_consumer = await_announced(&consumer, &announcement.prefix).await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");

	assert_eq!(catalog.audio.len(), 1);
	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	assert_eq!(audio.codec, "opus");
	assert_eq!(audio.sample_rate, 48000);
	assert_eq!(audio.channel_count, 2);
	assert!(catalog.video.is_empty());

	let media_consumer = MoqMediaTrackConsumer::subscribe(
		&broadcast_consumer,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Audio {
				id: track_name.clone(),
				config: audio.clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();

	let payload = b"opus audio payload data".to_vec();
	media
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(1_000_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&media_consumer))
		.await
		.expect("timed out waiting for frame")
		.unwrap()
		.expect("expected a frame");

	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 1_000_000);

	broadcast.close().unwrap();
}

#[tokio::test]
async fn video_publish_consume() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "video-test");
	let init = h264_init();
	let media = MoqMediaTrackProducer::video(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		video_init(MoqVideoFormat::Avc3, init),
	)
	.unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();

	let announcement = next_announced(&announced).await;

	let broadcast_consumer = await_announced(&consumer, &announcement.prefix).await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected catalog");

	assert_eq!(catalog.video.len(), 1);
	let (track_name, video) = catalog.video.iter().next().unwrap();
	assert!(
		video.codec.starts_with("avc1.") || video.codec.starts_with("avc3."),
		"codec should be avc1/avc3, got {}",
		video.codec
	);
	let coded = video.coded.as_ref().expect("coded dimensions should be set");
	assert_eq!(coded.width, 1280);
	assert_eq!(coded.height, 720);
	assert!(catalog.audio.is_empty());

	let media_consumer = MoqMediaTrackConsumer::subscribe(
		&broadcast_consumer,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Video {
				id: track_name.clone(),
				config: video.clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();

	let keyframe = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC];
	media
		.write_frame(MoqFrame {
			payload: keyframe,
			timestamp_us: Some(0),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&media_consumer))
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected frame");

	assert_eq!(frame.timestamp_us, 0);
	assert!(!frame.payload.is_empty(), "frame should have payload data");

	broadcast.close().unwrap();
}

/// The raw-video publish path: hand mid-gray RGBA to `MoqVideoProducer` and check
/// that the encoder's own output reaches a subscriber, described by a catalog
/// rendition the importer built from the encoded keyframe.
#[cfg(feature = "video")]
#[tokio::test]
async fn video_raw_publish_consume() {
	use crate::video::*;

	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "video-raw-test");

	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input: MoqVideoEncoderInput {
				format: MoqVideoPixelFormat::Rgba,
				width: 320,
				height: 240,
				framerate: crate::video::MoqVideoFrameRate {
					numerator: 30_000,
					denominator: 1001,
				},
			},
			output: MoqVideoEncoderOutput {
				codec: MoqVideoCodec::H264,
				bitrate: None,
				gop: None,
				// Software so the test is deterministic everywhere: `Auto` would
				// reach for a hardware backend that CI runners don't have.
				kind: MoqVideoEncoderKind::Software,
			},
			track: Some("camera".into()),
			bandwidth: None,
			lower: None,
		},
	)
	.unwrap();
	assert_eq!(video.demand().unwrap().name(), "camera");
	let local_consumer = broadcast.consume().unwrap();
	let demand_consumer = local_consumer.subscribe_track("camera".into(), None).await.unwrap();
	tokio::time::timeout(TIMEOUT, video.demand().unwrap().until_wanted())
		.await
		.expect("timed out waiting for video demand")
		.unwrap();
	drop(demand_consumer);
	tokio::time::timeout(TIMEOUT, video.demand().unwrap().until_unwanted())
		.await
		.expect("timed out waiting for video demand to clear")
		.unwrap();

	// Seed the track before subscribing so the consumer below has encoded media
	// waiting at the live edge as soon as it joins.
	video.cut().unwrap();
	let rgba = vec![0x80u8; 320 * 240 * 4];
	for i in 0..5u64 {
		video
			.write(MoqVideoFrame {
				timestamp_us: i * 1_001_000 / 30,
				data: rgba.clone(),
			})
			.unwrap();
	}

	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;

	let broadcast_consumer = await_announced(&consumer, &announcement.prefix).await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected catalog");

	assert_eq!(catalog.video.len(), 1);
	let (track_name, rendition) = catalog.video.iter().next().unwrap();
	assert_eq!(track_name, "camera");
	assert!(
		rendition.codec.starts_with("avc3."),
		"codec should be avc3, got {}",
		rendition.codec
	);
	let coded = rendition.coded.as_ref().expect("coded dimensions should be set");
	assert_eq!(coded.width, 320);
	assert_eq!(coded.height, 240);
	assert!(catalog.audio.is_empty());
	assert!((rendition.framerate.unwrap() - 30_000.0 / 1001.0).abs() < 0.001);

	let media_consumer = MoqMediaTrackConsumer::subscribe(
		&broadcast_consumer,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Video {
				id: track_name.clone(),
				config: rendition.clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();

	// Keep feeding the encoder so the subscriber has frames to read after it
	// joins, whatever the group boundary it landed on.
	for i in 5..20u64 {
		video
			.write(MoqVideoFrame {
				timestamp_us: i * 1_001_000 / 30,
				data: rgba.clone(),
			})
			.unwrap();
	}

	let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&media_consumer))
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected frame");
	assert!(!frame.payload.is_empty(), "frame should carry encoded video");

	video.finish().unwrap();
	broadcast.close().unwrap();
}

/// A decoded frame owns its surface and converts on demand: one frame yields
/// both CPU layouts, a portable decode has no surface view, and the frame stays
/// readable after its consumer is cancelled and dropped and the track is gone.
/// A surface decode keeps the decoder's pixel buffer and still downloads on
/// demand, where the platform has one (macOS) and is refused where it has none.
#[cfg(feature = "video")]
#[tokio::test]
async fn video_decode_frame_ownership() {
	use crate::video::*;

	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "video-decode-frame");

	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input: MoqVideoEncoderInput {
				format: MoqVideoPixelFormat::Rgba,
				width: 320,
				height: 240,
				framerate: crate::video::MoqVideoFrameRate {
					numerator: 30,
					denominator: 1,
				},
			},
			output: MoqVideoEncoderOutput {
				codec: MoqVideoCodec::H264,
				bitrate: None,
				gop: None,
				// Software so the encode is deterministic everywhere.
				kind: MoqVideoEncoderKind::Software,
			},
			track: Some("camera".into()),
			bandwidth: None,
			lower: None,
		},
	)
	.unwrap();

	// Seed the track so a subscriber joining below lands on encoded media.
	let rgba = vec![0x80u8; 320 * 240 * 4];
	video.cut().unwrap();
	for i in 0..10u64 {
		video
			.write(MoqVideoFrame {
				timestamp_us: i * 33_333,
				data: rgba.clone(),
			})
			.unwrap();
	}

	let consumer = origin.consume();
	let broadcast_consumer = await_announced(&consumer, "video-decode-frame").await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected catalog");
	let (track, rendition) = catalog.video.iter().next().unwrap();

	let portable = crate::video::MoqVideoConsumer::subscribe(
		&broadcast_consumer,
		track.clone(),
		rendition.clone(),
		MoqVideoDecoderOutput::default(),
	)
	.await
	.unwrap();
	let surface_output = MoqVideoDecoderOutput {
		surface: true,
		..Default::default()
	};
	let retaining = crate::video::MoqVideoConsumer::subscribe(
		&broadcast_consumer,
		track.clone(),
		rendition.clone(),
		surface_output,
	)
	.await;
	// A platform with no surface variant refuses the opt-in up front, rather than
	// decoding to a surface the caller can neither view nor always download.
	let retaining = if cfg!(any(target_os = "macos", target_os = "ios")) {
		Some(retaining.unwrap())
	} else {
		assert!(matches!(retaining, Err(MoqError::Unsupported)));
		None
	};

	// Keep the encoder fed so both decoders see frames after they joined.
	for i in 10..40u64 {
		video
			.write(MoqVideoFrame {
				timestamp_us: i * 33_333,
				data: rgba.clone(),
			})
			.unwrap();
	}

	let next = async |decoder: &MoqVideoConsumer| {
		let read = tokio::time::timeout(TIMEOUT, decoder.next())
			.await
			.expect("timed out")
			.unwrap();
		match read {
			Some(MoqVideoRead::Frame { frame }) => frame,
			_ => panic!("expected a frame"),
		}
	};
	let frame = next(&portable).await;
	let retained = match &retaining {
		Some(decoder) => Some(next(decoder).await),
		None => None,
	};

	// Release everything upstream of the frames before reading them.
	portable.cancel();
	if let Some(decoder) = &retaining {
		decoder.cancel();
	}
	drop((portable, retaining));
	video.finish().unwrap();
	broadcast.close().unwrap();

	if let Some(retained) = retained {
		assert_eq!((retained.width(), retained.height()), (320, 240));
		assert!(
			matches!(retained.surface(), Some(MoqVideoSurface::PixelBuffer { pointer }) if pointer != 0),
			"a surface decode on macOS retains the pixel buffer"
		);
		assert_eq!(
			retained.pixels(MoqVideoPixelFormat::I420).unwrap().len(),
			320 * 240 * 3 / 2
		);
	}

	assert_eq!((frame.width(), frame.height()), (320, 240));
	assert!(frame.surface().is_none(), "a portable decode holds CPU pixels");

	let i420 = frame.pixels(MoqVideoPixelFormat::I420).unwrap();
	assert_eq!(i420.len(), 320 * 240 * 3 / 2);

	let packed = frame.pixels(MoqVideoPixelFormat::Rgba).unwrap();
	assert_eq!(packed.len(), 320 * 240 * 4);
	// Every fourth byte is alpha, so an opaque frame proves the conversion ran rather than handing back planes.
	assert!(
		packed.as_chunks::<4>().0.iter().all(|px| px[3] == 0xFF),
		"RGBA output should be opaque"
	);

	// Converting again reads the same retained surface.
	assert_eq!(frame.pixels(MoqVideoPixelFormat::I420).unwrap(), i420);
}

/// Regression: a `MoqVideoProducer` is shared, so its calls land on whichever
/// thread the caller is on, and none of them need be the thread that published.
/// Holding a bare `Encoder` made that unsound on Windows, where the codec's COM
/// apartment is per-thread: it was opened on the publishing thread and closed on
/// whichever thread dropped the object. The confinement itself is asserted in
/// moq-video (`encode::sink`); this pins that the binding supports the usage
/// end to end, including the drain on `finish`.
#[cfg(feature = "video")]
#[tokio::test]
async fn video_raw_publish_from_many_threads() {
	use crate::video::*;

	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "video-raw-threads");

	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input: MoqVideoEncoderInput {
				format: MoqVideoPixelFormat::Rgba,
				width: 320,
				height: 240,
				framerate: crate::video::MoqVideoFrameRate {
					numerator: 30,
					denominator: 1,
				},
			},
			output: MoqVideoEncoderOutput {
				codec: MoqVideoCodec::H264,
				// An explicit ceiling so the retunes below stay under the rate the
				// encoder opened at, which openh264 requires.
				bitrate: Some(1_000_000),
				gop: None,
				kind: MoqVideoEncoderKind::Software,
			},
			track: None,
			bandwidth: None,
			lower: None,
		},
	)
	.unwrap();

	// A fresh caller thread per frame, never the one that published.
	let rgba = std::sync::Arc::new(vec![0x80u8; 320 * 240 * 4]);
	for i in 0..8u64 {
		let video = video.clone();
		let rgba = rgba.clone();
		std::thread::spawn(move || {
			if i == 0 {
				video.cut().unwrap();
			}
			video
				.write(MoqVideoFrame {
					timestamp_us: i * 33_333,
					data: rgba.as_ref().clone(),
				})
				.unwrap();
			video.set_bitrate(900_000 - i).unwrap();
		})
		.join()
		.unwrap();
	}

	// The rendition only exists once the importer parsed the codec config out of
	// an encoded keyframe, so this is what says the frames really were encoded.
	// Checked before finishing, which withdraws it again.
	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;
	let catalog_consumer =
		MoqMediaCatalogConsumer::subscribe(await_announced(&consumer, &announcement.prefix).await.as_ref())
			.await
			.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected catalog");
	assert_eq!(catalog.video.len(), 1);

	// ...and finished, so the encoder is drained and dropped, from yet another.
	let closer = video.clone();
	std::thread::spawn(move || closer.finish()).join().unwrap().unwrap();

	broadcast.close().unwrap();
}

/// A raw video producer rejects a buffer that isn't one picture at the
/// configured resolution, rather than reinterpreting it, and rejects any write
/// after `finish`.
#[cfg(feature = "video")]
#[tokio::test]
async fn video_raw_publish_rejects_bad_frames() {
	use crate::video::*;

	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "video-raw-reject-test");

	let input = |width, height| MoqVideoEncoderInput {
		format: MoqVideoPixelFormat::Rgba,
		width,
		height,
		framerate: crate::video::MoqVideoFrameRate {
			numerator: 30,
			denominator: 1,
		},
	};
	let output = || MoqVideoEncoderOutput {
		codec: MoqVideoCodec::H264,
		bitrate: None,
		gop: None,
		kind: MoqVideoEncoderKind::Software,
	};

	// A zero framerate is rejected before any track is advertised.
	assert!(
		crate::video::MoqVideoProducer::new(
			&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
			crate::video::MoqVideoProducerOptions {
				input: MoqVideoEncoderInput {
					framerate: crate::video::MoqVideoFrameRate {
						numerator: 0,
						denominator: 1
					},
					..input(320, 240)
				},
				output: output(),
				track: None,
				bandwidth: None,
				lower: None,
			}
		)
		.is_err()
	);

	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input: input(320, 240),
			output: output(),
			track: None,
			bandwidth: None,
			lower: None,
		},
	)
	.unwrap();

	// A 640x480 buffer against a 320x240 encoder: the frame carries no dimensions
	// of its own, so this is caught as a wrong-sized picture.
	assert!(
		video
			.write(MoqVideoFrame {
				timestamp_us: 0,
				data: vec![0x80u8; 640 * 480 * 4],
			})
			.is_err()
	);

	video.finish().unwrap();
	assert!(matches!(
		video.write(MoqVideoFrame {
			timestamp_us: 0,
			data: vec![0x80u8; 320 * 240 * 4],
		}),
		Err(MoqError::Closed)
	));

	broadcast.close().unwrap();
}

#[tokio::test]
async fn multiple_frames_ordering() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "ordering-test");
	let init = opus_head();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	)
	.unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;

	let broadcast_consumer = await_announced(&consumer, &announcement.prefix).await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	let media_consumer = MoqMediaTrackConsumer::subscribe(
		&broadcast_consumer,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Audio {
				id: track_name.clone(),
				config: audio.clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();

	let timestamps: [u64; 5] = [0, 20_000, 40_000, 60_000, 80_000];
	for (i, &ts) in timestamps.iter().enumerate() {
		let payload = format!("frame-{i}");
		media
			.write_frame(MoqFrame {
				payload: payload.into_bytes(),
				timestamp_us: Some(ts),
			})
			.unwrap();
	}

	for (i, &expected_ts) in timestamps.iter().enumerate() {
		let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&media_consumer))
			.await
			.unwrap_or_else(|_| panic!("timed out waiting for frame {i}"))
			.unwrap()
			.unwrap_or_else(|| panic!("expected frame {i}"));

		assert_eq!(frame.timestamp_us, expected_ts, "frame {i} has wrong timestamp");
		let expected = format!("frame-{i}");
		assert_eq!(frame.payload, expected.as_bytes(), "frame {i} has wrong payload");
	}

	broadcast.close().unwrap();
}

#[tokio::test]
async fn catalog_update_on_new_track() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let broadcast = create_announced(&origin, "catalog-update");
	let init = opus_head();
	let mut first = audio_init(MoqAudioFormat::Opus, init.clone());
	first.label = Some("English".to_string());
	let _media1 = MoqMediaTrackProducer::audio(&broadcast, MoqMediaTarget::Named { name: None }, first).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;

	let broadcast_consumer = await_announced(&consumer, &announcement.prefix).await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&broadcast_consumer).await.unwrap();

	let catalog1 = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(catalog1.audio.len(), 1);
	assert_eq!(catalog1.audio["0.opus"].label.as_deref(), Some("English"));

	let _media2 = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	)
	.unwrap();

	let catalog2 = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(catalog2.audio.len(), 2);
	assert_eq!(catalog2.audio["0.opus"].label.as_deref(), Some("English"));
	assert_eq!(catalog2.audio["1.opus"].label, None);

	broadcast.close().unwrap();
}

#[test]
fn close_twice_is_a_noop() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let _media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init.clone()),
	)
	.unwrap();
	broadcast.close().unwrap();
	broadcast.close().unwrap();

	let Err(err) = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	) else {
		panic!("publishing after close succeeded");
	};
	assert!(
		matches!(err, crate::error::MoqError::Closed),
		"expected Closed error, got {err}"
	);
}

/// Wait for the announcement of `path` to end, failing the test if it doesn't.
async fn await_unannounced(announced: &MoqAnnounceConsumer) {
	match next_event(announced).await {
		MoqAnnounceEvent::End { .. } => {}
		other => panic!("expected the announcement to end, got {other:?}"),
	}
}

/// A finished broadcast stays announced and serves its finished tracks to a late subscriber,
/// even once their handles are released, until it closes.
#[tokio::test]
async fn finish_serves_finished_tracks_until_close() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let broadcast = create_announced(&origin, "finish");
	next_announced(&announced).await;

	let audio = broadcast.publish_track("audio".into(), Some(timed_info())).unwrap();
	audio
		.write_frame(MoqFrame {
			payload: b"tail".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	audio.finish().unwrap();
	// A binding's garbage collector may release a finished handle before the broadcast closes.
	drop(audio);
	let catalog = crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
	catalog.finish().unwrap();

	let late = await_announced(&consumer, "finish").await;
	let track = late.subscribe_track("audio".into(), None).await.unwrap();
	let frame = tokio::time::timeout(TIMEOUT, track.read_frame())
		.await
		.expect("timed out reading the finished track")
		.unwrap()
		.expect("expected the finished track's frame");
	assert_eq!(frame.payload, b"tail");

	broadcast.close().unwrap();
	await_unannounced(&announced).await;
	assert!(matches!(catalog.finish(), Err(MoqError::Closed)));
}

/// One raw and one media handle on `broadcast`, each with a subscriber reading it.
async fn children(
	broadcast: &MoqBroadcastProducer,
) -> (
	Arc<MoqTrackProducer>,
	Arc<MoqMediaTrackProducer>,
	[Arc<MoqTrackConsumer>; 2],
) {
	let consumer = broadcast.consume().unwrap();
	let raw = broadcast.publish_track("raw".into(), Some(timed_info())).unwrap();
	let media = MoqMediaTrackProducer::audio(
		broadcast,
		MoqMediaTarget::Named {
			name: Some("media".into()),
		},
		audio_init(MoqAudioFormat::Opus, opus_head()),
	)
	.unwrap();
	let readers = [
		consumer.subscribe_track("raw".into(), None).await.unwrap(),
		consumer.subscribe_track("media".into(), None).await.unwrap(),
	];
	(raw, media, readers)
}

/// Each reader ends with `Unroutable`, the cause of a publisher leaving.
async fn assert_left<const N: usize>(readers: [Arc<MoqTrackConsumer>; N]) {
	for reader in readers {
		let read = tokio::time::timeout(TIMEOUT, reader.read_frame()).await;
		let Err(err) = read.expect("the track carried on") else {
			panic!("the track ended without a cause");
		};
		assert_protocol(
			&err,
			crate::error::MoqErrorScope::Stream,
			crate::error::MoqProtocolKind::Unroutable,
		);
	}
}

/// Closing a broadcast ends every track handle still open on it the way a publisher leaving
/// does, and the handles refuse later calls.
#[tokio::test(start_paused = true)]
async fn closing_a_broadcast_ends_its_open_tracks() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let (raw, media, readers) = children(&broadcast).await;
	broadcast.close().unwrap();
	assert_left(readers).await;

	let frame = MoqFrame {
		payload: b"late".to_vec(),
		timestamp_us: Some(0),
	};
	assert!(matches!(raw.write_frame(frame.clone()), Err(MoqError::Closed)));
	assert!(matches!(raw.finish(), Err(MoqError::Closed)));
	assert!(matches!(media.write_frame(frame), Err(MoqError::Closed)));
	assert!(matches!(media.finish(), Err(MoqError::Closed)));
}

/// A track handle released without a finish, as a garbage collector does, ends its track as a
/// publisher leaving rather than a cause-less drop.
#[tokio::test(start_paused = true)]
async fn dropping_an_unfinished_track_handle_ends_it() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let (raw, media, readers) = children(&broadcast).await;
	drop((raw, media));
	assert_left(readers).await;
	broadcast.close().unwrap();
}

/// A track finished before its broadcast closes ends cleanly, even once its handle is released.
#[tokio::test(start_paused = true)]
async fn finishing_before_close_ends_tracks_cleanly() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let (raw, media, readers) = children(&broadcast).await;
	raw.finish().unwrap();
	media.finish().unwrap();
	drop(raw);
	broadcast.close().unwrap();
	for reader in readers {
		let read = tokio::time::timeout(TIMEOUT, reader.read_frame()).await;
		assert!(read.expect("the track carried on").unwrap().is_none());
	}
}

/// One writer of each data kind on `broadcast`, each with a subscriber reading its track.
async fn data_children(
	broadcast: &MoqBroadcastProducer,
) -> (
	(
		Arc<MoqJsonSnapshotProducer>,
		Arc<MoqJsonStreamProducer>,
		Arc<MoqJsonWindowProducer>,
	),
	[Arc<MoqTrackConsumer>; 3],
) {
	let consumer = broadcast.consume().unwrap();
	let track = |name: &str| broadcast.publish_track(name.into(), None).unwrap();
	let writers = (
		MoqJsonSnapshotProducer::new(
			broadcast,
			&track("json-snapshot"),
			MoqJsonSnapshotConfig { deltas: true },
		)
		.unwrap(),
		MoqJsonStreamProducer::new(broadcast, &track("json-stream")).unwrap(),
		MoqJsonWindowProducer::new(
			&track("json-window"),
			MoqJsonWindowConfig {
				checkpoint_records: None,
			},
		)
		.unwrap(),
	);
	let readers = [
		consumer.subscribe_track("json-snapshot".into(), None).await.unwrap(),
		consumer.subscribe_track("json-stream".into(), None).await.unwrap(),
		consumer.subscribe_track("json-window".into(), None).await.unwrap(),
	];
	(writers, readers)
}

/// Closing a broadcast ends its data tracks the way it ends media ones, and their writers
/// refuse later calls.
#[tokio::test(start_paused = true)]
async fn closing_a_broadcast_ends_its_open_data_tracks() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let ((json_snapshot, json_stream, json_window), readers) = data_children(&broadcast).await;
	broadcast.close().unwrap();
	assert_left(readers).await;

	assert!(matches!(json_snapshot.update("1".into(), None), Err(MoqError::Closed)));
	assert!(matches!(json_stream.append("1".into(), None), Err(MoqError::Closed)));
	assert!(matches!(json_window.push("1".into(), None), Err(MoqError::Closed)));
}

/// A data writer released without a finish ends its track as a publisher leaving.
#[tokio::test(start_paused = true)]
async fn dropping_an_unfinished_data_writer_ends_it() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let (writers, readers) = data_children(&broadcast).await;
	drop(writers);
	assert_left(readers).await;
	broadcast.close().unwrap();
}

/// A track handle goes to one writer: while one is built from it, another is refused, and a
/// construction that fails hands the handle back.
#[tokio::test(start_paused = true)]
async fn a_track_is_adopted_by_one_writer() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("json".into(), None).unwrap();
	let config = || MoqJsonSnapshotConfig { deltas: false };
	let failed = track.adopt::<MoqJsonSnapshotProducer>(|_| {
		assert!(matches!(
			MoqJsonSnapshotProducer::new(&broadcast, &track, config()),
			Err(MoqError::Closed)
		));
		Err(MoqError::JsonTrack {
			reason: "refused".into(),
		})
	});
	assert!(matches!(failed, Err(MoqError::JsonTrack { .. })));
	let writer = MoqJsonSnapshotProducer::new(&broadcast, &track, config()).unwrap();
	assert!(matches!(
		MoqJsonSnapshotProducer::new(&broadcast, &track, config()),
		Err(MoqError::Closed)
	));
	drop(writer);
	broadcast.close().unwrap();
}

/// A media finish that fails ends the track with that failure, rather than dropping it without a
/// cause.
#[tokio::test(start_paused = true)]
async fn a_failed_finish_ends_the_track_with_its_cause() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();
	let stream = MoqMediaTrackStreamProducer::video(
		&broadcast,
		MoqMediaTarget::Named {
			name: Some("video".into()),
		},
		video_init(MoqVideoFormat::Avc3, h264_init()),
	)
	.unwrap();
	let reader = consumer.subscribe_track("video".into(), None).await.unwrap();

	// A delta slice with no keyframe before it, held by the splitter until the finish flushes it.
	stream.write(vec![0, 0, 0, 1, 0x61, 0xe0, 0x12, 0x34]).unwrap();
	assert!(matches!(stream.finish(), Err(MoqError::Codec { .. })));

	let read = tokio::time::timeout(TIMEOUT, reader.read_frame()).await;
	let Err(err) = read.expect("the track carried on") else {
		panic!("the track ended without a cause");
	};
	assert_protocol(
		&err,
		crate::error::MoqErrorScope::Stream,
		crate::error::MoqProtocolKind::Internal,
	);
	broadcast.close().unwrap();
}

/// A request accepted after its broadcast closed is refused, and its subscriber sees the
/// publisher leave.
#[tokio::test(start_paused = true)]
async fn accepting_a_request_after_close_is_refused() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.requests().unwrap();
	let consumer = broadcast.consume().unwrap();
	let subscribe = tokio::spawn(async move { consumer.subscribe_track("late".into(), None).await });
	let request = dynamic.requested_track().await.unwrap();
	broadcast.close().unwrap();
	assert!(matches!(request.accept(None), Err(MoqError::Closed)));
	let reader = subscribe.await.unwrap().unwrap();
	assert_left([reader]).await;
}

#[tokio::test]
async fn announced_broadcast() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let _broadcast = create_announced(&origin, "test/broadcast");

	let consumer = origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();

	let announcement = next_announced(&announced).await;

	assert_eq!(announcement.prefix, "test/broadcast");
	let _catalog = MoqMediaCatalogConsumer::subscribe(await_announced(&consumer, &announcement.prefix).await.as_ref())
		.await
		.unwrap();
	// Finish so consumers observe a deliberate end (the canonical end for a
	// publisher; dropping without finish reads as a failure).
	_broadcast.close().unwrap();
}

fn serve(origin: &MoqOriginProducer, prefix: &str) -> Arc<MoqOriginRequests> {
	origin.requests(prefix.into()).unwrap()
}

#[tokio::test]
async fn dynamic_broadcast_request() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "");
	let consumer = origin.consume();

	let request_broadcast = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.request_broadcast("dynamic/broadcast".into()).await })
	};

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("timed out waiting for requested broadcast")
		.unwrap();
	assert_eq!(request.path().unwrap(), "dynamic/broadcast");

	let epoch = moq_net::Epoch::mint().to_string();
	let prepared = request.prepare(epoch.clone()).unwrap();
	let served = prepared.broadcast();
	let track = served.publish_track("status".into(), Some(timed_info())).unwrap();
	// Only publish announces the answer, under its epoch and after admission, and it
	// hands back the fill handle the answer was prepared with.
	let publication = prepared.publish().unwrap();
	assert!(Arc::ptr_eq(&publication.broadcast(), &served));
	publication
		.announce(MoqAnnounceRoute {
			epoch: Some(epoch),
			cost: 7,
		})
		.unwrap();
	assert!(matches!(request.path(), Err(MoqError::Closed)));

	let broadcast = tokio::time::timeout(TIMEOUT, request_broadcast)
		.await
		.expect("timed out waiting for requested broadcast result")
		.expect("request task panicked")
		.unwrap();

	let track_consumer = broadcast.subscribe_track("status".into(), None).await.unwrap();
	let payload = b"served dynamically".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(20_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, track_consumer.read_frame())
		.await
		.expect("timed out waiting for dynamic broadcast frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(20_000));

	track.finish().unwrap();
	served.close().unwrap();
}

/// A prefix serves requests beneath it; cancelling the handle
/// rejects what it parked and later requests are unroutable.
#[tokio::test]
async fn dynamic_serves_a_request_under_a_prefix() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let dynamic = serve(&origin, "live");

	let request_broadcast = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.request_broadcast("live/cam".into()).await })
	};
	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("timed out waiting for the prefix request")
		.unwrap();
	assert_eq!(request.path().unwrap(), "live/cam");
	let prepared = request.prepare(moq_net::Epoch::mint().to_string()).unwrap();
	let served = prepared.broadcast();
	prepared.publish().unwrap();
	tokio::time::timeout(TIMEOUT, request_broadcast)
		.await
		.expect("timed out waiting for the request to resolve")
		.expect("request task panicked")
		.expect("the handler served the path");

	dynamic.cancel();
	let err = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("live/other".into()))
		.await
		.expect("timed out waiting for the retraction to reject the request")
		.err()
		.expect("nothing serves the prefix any more");
	assert_protocol(
		&err,
		crate::error::MoqErrorScope::Stream,
		crate::error::MoqProtocolKind::Unroutable,
	);

	served.close().unwrap();
}

/// A dynamic handler keeps its origin alive: dropping (or GC-finalizing) the
/// last `MoqOriginProducer` leaves the route serving, and a consumer made
/// earlier still resolves through it.
#[tokio::test]
async fn dynamic_keeps_the_origin_alive() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let consumer = origin.consume();
	let dynamic = serve(&origin, "");
	drop(origin);

	let request_broadcast = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.request_broadcast("live".into()).await })
	};
	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("the handler must still receive requests")
		.unwrap();
	let prepared = request.prepare(moq_net::Epoch::mint().to_string()).unwrap();
	let served = prepared.broadcast();
	prepared.publish().unwrap();
	tokio::time::timeout(TIMEOUT, request_broadcast)
		.await
		.expect("timed out waiting for the request to resolve")
		.expect("request task panicked")
		.expect("the handler served the path");

	served.close().unwrap();
}

/// Cancelling a handler retracts its route before returning.
#[tokio::test]
async fn cancel_retracts_the_route_synchronously() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "");
	let inner = origin.inner().consume();

	let mut queued = inner.request("x", None).into_inner();
	assert!(
		queued.poll_ok(&kio::Waiter::noop()).is_pending(),
		"the route must serve while the handler lives"
	);
	drop(queued);

	dynamic.cancel();
	let mut verdict = inner.request("y", None).into_inner();
	match verdict.poll_ok(&kio::Waiter::noop()) {
		std::task::Poll::Ready(Err(moq_net::Error::Unroutable)) => {}
		std::task::Poll::Ready(Err(err)) => panic!("unexpected error: {err}"),
		std::task::Poll::Ready(Ok(_)) => panic!("resolved through a retracted route"),
		std::task::Poll::Pending => panic!("queued on a route that cancel should have retracted"),
	}
}

/// The sequence cursor commits on first use, so every later read has to reach the same
/// converted handle. Repeating the conversion (or dropping it on the way through) leaves
/// the track in its transient state and panics the next read.
#[tokio::test]
async fn raw_track_next_group_is_repeatable() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("commands".into(), Some(timed_info())).unwrap();
	let consumer = broadcast
		.consume()
		.unwrap()
		.subscribe_track("commands".into(), None)
		.await
		.unwrap();

	for payload in [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()] {
		track
			.write_frame(MoqFrame {
				payload: payload.clone(),
				timestamp_us: Some(0),
			})
			.unwrap();

		let group = tokio::time::timeout(TIMEOUT, consumer.next_group())
			.await
			.expect("timed out waiting for a group")
			.unwrap()
			.expect("expected a group");
		let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("timed out waiting for a frame")
			.unwrap()
			.expect("expected a frame");
		assert_eq!(frame.payload, payload);
	}

	// The arrival cursor is gone once the track committed to sequence order.
	assert!(matches!(consumer.recv_group().await, Err(MoqError::AlreadyCommitted)));
}

/// The first group read commits the cursor either way: mixing arrival and sequence
/// reads on one track is refused rather than silently interleaving two cursors.
/// Datagrams are a separate cursor, so they flow regardless of the commitment.
#[tokio::test]
async fn raw_track_group_order_commits_on_first_read() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("commands".into(), Some(timed_info())).unwrap();
	let consumer = broadcast
		.consume()
		.unwrap()
		.subscribe_track("commands".into(), None)
		.await
		.unwrap();

	track
		.write_frame(MoqFrame {
			payload: b"one".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();

	// An arrival read commits the track to arrival order.
	let group = tokio::time::timeout(TIMEOUT, consumer.recv_group())
		.await
		.expect("timed out waiting for a group")
		.unwrap()
		.expect("expected a group");
	assert_eq!(group.sequence(), 0);
	assert!(matches!(consumer.next_group().await, Err(MoqError::AlreadyCommitted)));
	assert!(matches!(consumer.read_frame().await, Err(MoqError::AlreadyCommitted)));

	// The commitment refuses the other cursor without poisoning this one.
	track
		.write_frame(MoqFrame {
			payload: b"two".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	let group = tokio::time::timeout(TIMEOUT, consumer.recv_group())
		.await
		.expect("timed out waiting for a group")
		.unwrap()
		.expect("expected a group");
	assert_eq!(group.sequence(), 1);

	// Datagrams never commit and keep working after a commitment.
	let sequence = track
		.append_datagram(MoqFrame {
			payload: b"beep".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for a datagram")
		.unwrap()
		.expect("expected a datagram");
	assert_eq!(datagram.sequence, sequence);
	assert_eq!(datagram.payload, b"beep".to_vec());
}

fn raw_track() -> (
	Arc<MoqBroadcastProducer>,
	Arc<crate::producer::MoqTrackProducer>,
	Arc<crate::consumer::MoqTrackConsumer>,
) {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();
	(broadcast, track, consumer)
}

fn datagram_frame(payload: &[u8]) -> MoqFrame {
	MoqFrame {
		payload: payload.to_vec(),
		timestamp_us: Some(1),
	}
}

fn group_frame(payload: &[u8]) -> MoqFrame {
	MoqFrame {
		payload: payload.to_vec(),
		timestamp_us: Some(0),
	}
}

/// A pending group-lane read must not stall `recv_datagram` on the same subscription.
async fn pending_group_does_not_block_datagram<F, Fut, T>(start_group: F)
where
	F: FnOnce(Arc<crate::consumer::MoqTrackConsumer>) -> Fut,
	Fut: Future<Output = Result<T, MoqError>> + Send + 'static,
	T: Send + 'static,
{
	let (_broadcast, track, consumer) = raw_track();

	let group_read = {
		let consumer = consumer.clone();
		spawn_parked(start_group(consumer)).await
	};
	assert!(!group_read.is_finished(), "group read should still be pending");

	let sequence = track.append_datagram(datagram_frame(b"beep")).unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for datagram behind a pending group read")
		.unwrap()
		.expect("expected a datagram");
	assert_eq!(datagram.sequence, sequence);
	assert_eq!(datagram.payload, b"beep".to_vec());
	assert!(!group_read.is_finished(), "group read should stay pending");

	track.write_frame(group_frame(b"group")).unwrap();
	tokio::time::timeout(TIMEOUT, group_read)
		.await
		.expect("timed out waiting for the parked group")
		.expect("group read panicked")
		.unwrap();
}

/// A pending datagram read must not stall a group-lane read on the same subscription.
async fn pending_datagram_does_not_block_group<F, Fut, T>(start_group: F)
where
	F: FnOnce(Arc<crate::consumer::MoqTrackConsumer>) -> Fut,
	Fut: Future<Output = Result<T, MoqError>> + Send + 'static,
	T: Send + 'static,
{
	let (_broadcast, track, consumer) = raw_track();

	let datagram_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.recv_datagram().await }).await
	};
	assert!(!datagram_read.is_finished(), "datagram read should still be pending");

	track.write_frame(group_frame(b"group")).unwrap();
	tokio::time::timeout(TIMEOUT, start_group(consumer.clone()))
		.await
		.expect("timed out waiting for a group behind a pending datagram read")
		.unwrap();
	assert!(!datagram_read.is_finished(), "datagram read should stay pending");

	let sequence = track.append_datagram(datagram_frame(b"beep")).unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, datagram_read)
		.await
		.expect("timed out waiting for the parked datagram")
		.expect("datagram read panicked")
		.unwrap()
		.expect("expected a datagram");
	assert_eq!(datagram.sequence, sequence);
	assert_eq!(datagram.payload, b"beep".to_vec());
}

#[tokio::test]
async fn raw_track_pending_next_group_does_not_block_datagram() {
	pending_group_does_not_block_datagram(|consumer| async move { consumer.next_group().await }).await;
}

#[tokio::test]
async fn raw_track_pending_recv_group_does_not_block_datagram() {
	pending_group_does_not_block_datagram(|consumer| async move { consumer.recv_group().await }).await;
}

#[tokio::test]
async fn raw_track_pending_read_frame_does_not_block_datagram() {
	pending_group_does_not_block_datagram(|consumer| async move { consumer.read_frame().await }).await;
}

#[tokio::test]
async fn raw_track_pending_datagram_does_not_block_next_group() {
	pending_datagram_does_not_block_group(|consumer| async move { consumer.next_group().await }).await;
}

#[tokio::test]
async fn raw_track_pending_datagram_does_not_block_recv_group() {
	pending_datagram_does_not_block_group(|consumer| async move { consumer.recv_group().await }).await;
}

#[tokio::test]
async fn raw_track_pending_datagram_does_not_block_read_frame() {
	pending_datagram_does_not_block_group(|consumer| async move { consumer.read_frame().await }).await;
}

#[tokio::test]
async fn raw_track_pending_group_read_holds_the_lane() {
	let (_broadcast, _track, consumer) = raw_track();

	let group_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.next_group().await }).await
	};
	assert!(!group_read.is_finished(), "group read should still be pending");
	assert!(
		consumer.group_lane_busy(),
		"a parked group read should hold the lane guard"
	);

	group_read.abort();
}

#[tokio::test]
async fn raw_track_update_during_pending_group_still_reads_datagram() {
	let (_broadcast, track, consumer) = raw_track();

	let group_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.next_group().await }).await
	};

	consumer
		.update(MoqSubscription {
			priority: 10,
			max_delay: Some(Duration::from_millis(25)),
			ranges: Some(vec![MoqRange {
				start: MoqAnchor::Absolute { group: 0 },
				end: None,
			}]),
			order: None,
		})
		.unwrap();

	let sequence = track.append_datagram(datagram_frame(b"after-update")).unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for datagram after update")
		.unwrap()
		.expect("expected a datagram");
	assert_eq!(datagram.sequence, sequence);
	assert_eq!(datagram.payload, b"after-update".to_vec());

	track.write_frame(group_frame(b"group")).unwrap();
	tokio::time::timeout(TIMEOUT, group_read)
		.await
		.expect("timed out waiting for the parked group")
		.expect("group read panicked")
		.unwrap()
		.expect("expected a group");
}

#[tokio::test]
async fn raw_track_dropping_a_pending_group_read_does_not_cancel_datagrams() {
	let (_broadcast, track, consumer) = raw_track();

	let group_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.next_group().await }).await
	};
	group_read.abort();
	match group_read.await {
		Err(err) if err.is_cancelled() => {}
		Err(err) => panic!("aborted group read should join as cancelled, got {err}"),
		Ok(_) => panic!("aborting the call should cancel only that future"),
	}

	let sequence = track.append_datagram(datagram_frame(b"still-open")).unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for datagram after a cancelled group read")
		.unwrap()
		.expect("expected a datagram");
	assert_eq!(datagram.sequence, sequence);

	track.write_frame(group_frame(b"group")).unwrap();
	let group = tokio::time::timeout(TIMEOUT, consumer.next_group())
		.await
		.expect("timed out waiting for a group after the cancelled read")
		.unwrap()
		.expect("expected a group");
	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("timed out waiting for the group's frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, b"group".to_vec());
}

#[tokio::test]
async fn raw_track_dropping_a_pending_datagram_read_does_not_cancel_groups() {
	let (_broadcast, track, consumer) = raw_track();

	let datagram_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.recv_datagram().await }).await
	};
	datagram_read.abort();
	match datagram_read.await {
		Err(err) if err.is_cancelled() => {}
		Err(err) => panic!("aborted datagram read should join as cancelled, got {err}"),
		Ok(_) => panic!("aborting the call should cancel only that future"),
	}

	track.write_frame(group_frame(b"group")).unwrap();
	let group = tokio::time::timeout(TIMEOUT, consumer.next_group())
		.await
		.expect("timed out waiting for a group after a cancelled datagram read")
		.unwrap()
		.expect("expected a group");
	assert_eq!(group.sequence(), 0);

	let sequence = track.append_datagram(datagram_frame(b"later")).unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for a datagram after the cancelled read")
		.unwrap()
		.expect("expected a datagram");
	assert_eq!(datagram.sequence, sequence);
}

#[tokio::test]
async fn raw_track_handle_cancel_aborts_both_pending_lanes() {
	let (_broadcast, _track, consumer) = raw_track();

	let group_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.next_group().await }).await
	};
	let datagram_read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.recv_datagram().await }).await
	};

	consumer.cancel();

	match tokio::time::timeout(TIMEOUT, group_read)
		.await
		.expect("timed out waiting for cancelled group read")
		.expect("group read panicked")
	{
		Err(MoqError::Cancelled) => {}
		Err(err) => panic!("handle cancel should fail the group read, got {err:?}"),
		Ok(_) => panic!("handle cancel should fail the group read"),
	}

	match tokio::time::timeout(TIMEOUT, datagram_read)
		.await
		.expect("timed out waiting for cancelled datagram read")
		.expect("datagram read panicked")
	{
		Err(MoqError::Cancelled) => {}
		Err(err) => panic!("handle cancel should fail the datagram read, got {err:?}"),
		Ok(_) => panic!("handle cancel should fail the datagram read"),
	}
}

/// An empty completed group is not track EOF: `read_frame` waits for a later
/// group on a still-open track.
#[tokio::test]
async fn raw_read_frame_skips_empty_group_on_open_track() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	let empty = track.append_group().unwrap();
	empty.finish().unwrap();

	let read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.read_frame().await }).await
	};
	if read.is_finished() {
		match read.await {
			Ok(Ok(Some(_))) => panic!("empty group returned a frame"),
			Ok(Ok(None)) => panic!("empty group must not end an open track"),
			Ok(Err(err)) => panic!("read_frame errored on an empty group: {err:?}"),
			Err(err) => panic!("read task failed: {err:?}"),
		}
	}

	let payload = b"after-empty".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(1_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, read)
		.await
		.expect("timed out waiting for the frame after an empty group")
		.expect("read task panicked")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(1_000));
}

/// Empty groups already in the cursor are skipped, then the next populated
/// group's first frame is returned.
#[tokio::test]
async fn raw_read_frame_skips_empty_then_populated_groups() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	track.append_group().unwrap().finish().unwrap();
	track.append_group().unwrap().finish().unwrap();

	let payload = b"populated".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(2_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, consumer.read_frame())
		.await
		.expect("timed out skipping empty groups")
		.unwrap()
		.expect("expected the populated group's first frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(2_000));
}

/// A group a hop reset as expired is skipped like an empty one, as a locally expired
/// group is (`test_a_pending_timer_waits_for_advance` in py covers that).
#[tokio::test]
async fn raw_read_frame_skips_a_group_reset_as_old() {
	let broadcast = moq_net::broadcast::Info::default().produce();
	let track = broadcast
		.create_track(
			"status",
			Some(moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)),
		)
		.unwrap();
	let consumer = MoqTrackConsumer::new(track.subscribe(None), crate::ffi::Background::default());

	let expired = track.append_group().unwrap();
	// The read holds the group when the reset lands, as one arriving over the wire does.
	let read = spawn_parked(async move { consumer.read_frame().await }).await;
	expired
		.abort(moq_net::Error::Stream(moq_net::StreamError::Old))
		.unwrap();
	let mut later = track.append_group().unwrap();
	later
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"later".as_slice())
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, read)
		.await
		.expect("timed out skipping the expired group")
		.expect("read task panicked")
		.unwrap()
		.expect("expected the later group's first frame");
	assert_eq!(frame.payload, b"later");
}

/// A foreign cancel only stops polling a read; uniffi drops it later, at `rust_future_free`.
/// Until then the cancelled read must not take the frame the next read is waiting for.
#[tokio::test]
async fn raw_read_frame_cancelled_before_free_leaves_the_frame() {
	let (_broadcast, track, consumer) = raw_track();

	// Poll once, then stop without dropping, which is all `rust_future_cancel` does.
	let mut cancelled = Box::pin(consumer.read_frame());
	let first = std::future::poll_fn(|cx| std::task::Poll::Ready(cancelled.as_mut().poll(cx))).await;
	assert!(first.is_pending());

	track.write_frame(group_frame(b"next")).unwrap();
	// Give anything still driving the cancelled read the chance to take the frame.
	ffi_caught_up().await;

	let next = consumer.read_frame();
	drop(cancelled);
	let frame = tokio::time::timeout(TIMEOUT, next)
		.await
		.expect("the cancelled read took the frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, b"next".to_vec());
}

/// Cancelling `read_frame` after it has taken the next group must not drop that
/// group: the next read still returns its first frame.
#[tokio::test]
async fn raw_read_frame_keeps_group_across_cancelled_call() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	let group = track.append_group().unwrap();
	cancel_parked_read_frame(&consumer, &group).await;

	let payload = b"kept".to_vec();
	group
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(3_000),
		})
		.unwrap();
	group.finish().unwrap();
	track.finish().unwrap();

	let frame = tokio::time::timeout(TIMEOUT, consumer.read_frame())
		.await
		.expect("timed out reading the preserved group")
		.unwrap()
		.expect("cancelled read_frame must not lose the group's first frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(3_000));

	assert!(
		tokio::time::timeout(TIMEOUT, consumer.read_frame())
			.await
			.expect("timed out waiting for track EOF")
			.unwrap()
			.is_none()
	);
}

/// An error on a pending group's first frame is terminal for that group: the
/// next `read_frame` moves on rather than retrying the dead group.
#[tokio::test]
async fn raw_read_frame_drops_aborted_pending_group() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	let first = track.append_group().unwrap();
	let read = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.read_frame().await })
	};
	wait_group_acquired(&first).await;
	first.abort(409).unwrap();

	let err = tokio::time::timeout(TIMEOUT, read)
		.await
		.expect("timed out waiting for the aborted group")
		.expect("read task panicked");
	match err {
		Err(MoqError::Protocol { details: protocol }) => {
			assert_eq!(protocol.scope, crate::error::MoqErrorScope::Stream);
			assert_eq!(protocol.code, 64 + 409);
			assert_eq!(protocol.kind, crate::error::MoqProtocolKind::App);
		}
		Err(other) => panic!("expected Protocol App(409), got {other:?}"),
		Ok(Some(_)) => panic!("aborted group returned a frame"),
		Ok(None) => panic!("aborted group returned EOF"),
	}

	let payload = b"next".to_vec();
	track
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(4_000),
		})
		.unwrap();
	let frame = tokio::time::timeout(TIMEOUT, consumer.read_frame())
		.await
		.expect("timed out reading the group after an aborted pending group")
		.unwrap()
		.expect("an aborted pending group must not block later groups");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, Some(4_000));
}

/// `next_group` and `read_frame` share one ordered cursor. A cancelled
/// `read_frame` leaves its group for `next_group`; a group `next_group` has
/// already returned is not also read as a first frame.
#[tokio::test]
async fn raw_read_frame_and_next_group_share_the_cursor() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), Some(timed_info())).unwrap();
	let consumer = track.consume(None).unwrap();

	let first = track.append_group().unwrap();
	cancel_parked_read_frame(&consumer, &first).await;
	first
		.write_frame(MoqFrame {
			payload: b"first".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	first.finish().unwrap();

	let group = tokio::time::timeout(TIMEOUT, consumer.next_group())
		.await
		.expect("timed out taking the pending group")
		.unwrap()
		.expect("next_group should return the group read_frame acquired");
	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("timed out reading the handed-off group")
		.unwrap()
		.expect("expected the first group's frame");
	assert_eq!(frame.payload, b"first".to_vec());

	track
		.write_frame(MoqFrame {
			payload: b"second".to_vec(),
			timestamp_us: Some(0),
		})
		.unwrap();
	let frame = tokio::time::timeout(TIMEOUT, consumer.read_frame())
		.await
		.expect("timed out reading the next group")
		.unwrap()
		.expect("read_frame should skip the group next_group already returned");
	assert_eq!(frame.payload, b"second".to_vec());
}

/// Terminal cancel drops the pending group and releases subscription demand.
#[tokio::test]
async fn raw_read_frame_terminal_cancel_releases_demand() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let consumer = track.consume(None).unwrap();
	let demand = track.demand().unwrap();

	tokio::time::timeout(TIMEOUT, demand.until_wanted())
		.await
		.expect("timed out waiting for the subscriber")
		.unwrap();

	let open = track.append_group().unwrap();
	let read = {
		let consumer = consumer.clone();
		spawn_parked(async move { consumer.read_frame().await }).await
	};
	wait_group_acquired(&open).await;

	consumer.cancel();
	let err = tokio::time::timeout(TIMEOUT, read)
		.await
		.expect("timed out waiting for the cancelled read")
		.expect("read task panicked");
	match err {
		Err(MoqError::Cancelled) => {}
		Err(other) => panic!("unexpected error: {other:?}"),
		Ok(Some(_)) => panic!("cancelled read returned a frame"),
		Ok(None) => panic!("cancelled read returned EOF"),
	}

	tokio::time::timeout(TIMEOUT, demand.until_unwanted())
		.await
		.expect("timed out waiting for demand to drop")
		.unwrap();

	assert!(matches!(consumer.read_frame().await, Err(MoqError::Cancelled)));
}

#[tokio::test]
async fn dynamic_broadcast_request_can_reject() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "");
	let consumer = origin.consume();

	let request_broadcast = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.request_broadcast("missing".into()).await })
	};

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("timed out waiting for requested broadcast")
		.unwrap();
	assert_eq!(request.path().unwrap(), "missing");

	request.reject(404).unwrap();
	assert!(matches!(request.path(), Err(MoqError::Closed)));

	let result = tokio::time::timeout(TIMEOUT, request_broadcast)
		.await
		.expect("timed out waiting for rejected broadcast")
		.expect("request task panicked");
	assert!(result.is_err(), "request for a rejected broadcast should fail");
}

#[tokio::test]
async fn cancelling_dynamic_broadcasts_unregisters_the_handler() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "");
	let consumer = origin.consume();

	dynamic.cancel();
	assert!(matches!(dynamic.requested_broadcast().await, Err(MoqError::Closed)));

	let result = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("missing".into()))
		.await
		.expect("request stayed pending after the dynamic handler was cancelled");
	let err = match result {
		Err(err) => err,
		Ok(_) => panic!("expected Unroutable, got a broadcast"),
	};
	assert_protocol(
		&err,
		crate::error::MoqErrorScope::Stream,
		crate::error::MoqProtocolKind::Unroutable,
	);
}

#[test]
fn without_runtime() {
	std::thread::spawn(|| {
		let origin = MoqOriginProducer::new(MoqOriginConfig::default());
		let consumer = origin.consume();

		let broadcast = create_announced(&origin, "test");
		let init = opus_head();
		let media = MoqMediaTrackProducer::audio(
			&broadcast,
			MoqMediaTarget::Named { name: None },
			audio_init(MoqAudioFormat::Opus, init),
		)
		.unwrap();
		media
			.write_frame(MoqFrame {
				payload: b"hello".to_vec(),
				timestamp_us: Some(1000),
			})
			.unwrap();

		let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
		let announcement = match pollster::block_on(announced.next()).unwrap().unwrap() {
			MoqAnnounceEvent::Start { announce } => announce,
			other => panic!("expected an announcement, got {other:?}"),
		};
		assert_eq!(announcement.prefix, "test");
		let _bc = pollster::block_on(consumer.request_broadcast("test".into())).unwrap();

		let client = MoqClient::new(MoqClientConfig {
			tls: insecure_tls(),
			consume: Some(origin),
			..Default::default()
		})
		.unwrap();

		announced.cancel();
		client.cancel();
		media.finish().unwrap();
		broadcast.close().unwrap();
		drop(client);
		drop(consumer);
		drop(announcement);
		drop(announced);
	})
	.join()
	.expect("client thread panicked, FFI method missing runtime guard");
}

#[tokio::test]
async fn server_client_roundtrip() {
	// Server side: bind, set a publish origin, accept incoming sessions.
	let server_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let server = MoqServer::new(MoqServerConfig {
		versions: vec!["moq-lite-07-wip".into()],
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		publish: Some(server_origin.clone()),
		..Default::default()
	})
	.unwrap();

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}/test?foo=bar");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		assert_eq!(request.path(), "/test");
		assert_eq!(request.query().as_deref(), Some("foo=bar"));
		let core = format!("moq-ffi/{} {}", env!("CARGO_PKG_VERSION"), moq_net::IMPLEMENTATION);
		assert_eq!(request.peer_implementation(), Some(format!("app/1 wrapper/2 {core}")));
		request.accept(None, None).await.expect("handshake failed")
	});

	// Client side: connect, subscribe via a consume origin.
	let client_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let client = MoqClient::new(MoqClientConfig {
		versions: vec!["moq-lite-07-wip".into()],
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		consume: Some(client_origin.clone()),
		implementation: Some("app/1 wrapper/2".into()),
		..Default::default()
	})
	.unwrap();
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	// Publish a broadcast on the server side.
	let broadcast = create_announced(&server_origin, "hello");
	let init = opus_head();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	)
	.unwrap();

	// Receive the announcement on the client side via the consume origin.
	let consumer = client_origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;
	assert_eq!(announcement.prefix, "hello");

	// Subscribe to the audio track and verify a frame round-trips.
	let bc = await_announced(&consumer, "hello").await;
	let catalog_consumer = MoqMediaCatalogConsumer::subscribe(&bc).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	let media_consumer = MoqMediaTrackConsumer::subscribe(
		&bc,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Audio {
				id: track_name.clone(),
				config: audio.clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();

	let payload = b"hello over the wire".to_vec();
	media
		.write_frame(MoqFrame {
			payload: payload.clone(),
			timestamp_us: Some(1_000_000),
		})
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(&media_consumer))
		.await
		.expect("timed out waiting for frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 1_000_000);

	// Clean up. Exercise `shutdown()` on the client side and the underlying
	// `cancel(code)` on the server side, so both shutdown paths run.
	media.finish().unwrap();
	broadcast.close().unwrap();
	cs.shutdown();
	cs.closed().await.unwrap();
	server_session.cancel(0);
	server.cancel();
}

/// Subscribe to the first audio rendition in `broadcast`'s catalog.
async fn subscribe_audio(broadcast: &MoqBroadcastConsumer) -> Arc<MoqMediaTrackConsumer> {
	let catalogs = MoqMediaCatalogConsumer::subscribe(broadcast).await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalogs.next())
		.await
		.expect("timed out waiting for the catalog")
		.unwrap()
		.expect("expected a catalog");
	let (id, config) = catalog.audio.iter().next().expect("an audio rendition");
	MoqMediaTrackConsumer::subscribe(
		broadcast,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Audio {
				id: id.clone(),
				config: config.clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap()
}

/// Write `payload` and read through `track` until it arrives, past what a new subscription
/// starts with.
async fn roundtrip(media: &MoqMediaTrackProducer, track: &MoqMediaTrackConsumer, payload: &[u8], timestamp_us: u64) {
	media
		.write_frame(MoqFrame {
			payload: payload.to_vec(),
			timestamp_us: Some(timestamp_us),
		})
		.unwrap();
	loop {
		let frame = tokio::time::timeout(TIMEOUT, next_encoded_frame(track))
			.await
			.expect("timed out waiting for a frame")
			.unwrap()
			.expect("the track ended");
		if frame.payload == payload {
			return;
		}
	}
}

/// A replacement session from the same publisher re-announces every route while the old one
/// is still open, as a GOAWAY drain does. Without an epoch that is another instance: the
/// next `available()` hands it out, and reading it carries on past the old session's close.
#[tokio::test]
async fn announced_broadcast_follows_an_epochless_replacement() {
	let server_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		publish: Some(server_origin.clone()),
		..Default::default()
	})
	.unwrap();
	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");
	let accepting = server.clone();
	let accept = tokio::spawn(async move {
		let mut sessions = Vec::new();
		while let Ok(Some(request)) = accepting.accept().await {
			sessions.push(request.accept(None, None).await.expect("handshake failed"));
		}
		sessions
	});

	let broadcast = create_announced(&server_origin, "cam");
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, opus_head()),
	)
	.unwrap();

	let viewer = MoqOriginProducer::new(MoqOriginConfig::default());
	let connect = || async {
		let client = MoqClient::new(MoqClientConfig {
			tls: insecure_tls(),
			bind: Some("127.0.0.1:0".into()),
			consume: Some(viewer.clone()),
			..Default::default()
		})
		.unwrap();
		tokio::time::timeout(TIMEOUT, client.connect(url.clone()))
			.await
			.expect("connect timed out")
			.expect("connect failed")
	};

	let first = connect().await;
	let followed = viewer.consume().announced_broadcast("cam".into()).unwrap();
	let old = tokio::time::timeout(TIMEOUT, followed.available())
		.await
		.expect("timed out waiting for the broadcast")
		.unwrap();
	let old_track = subscribe_audio(&old).await;
	roundtrip(&media, &old_track, b"before", 1_000_000).await;

	let second = connect().await;
	let fresh = tokio::time::timeout(TIMEOUT, followed.available())
		.await
		.expect("the replacement was never handed out")
		.unwrap();
	first.shutdown();
	first.closed().await.unwrap();

	let fresh_track = subscribe_audio(&fresh).await;
	roundtrip(&media, &fresh_track, b"after", 2_000_000).await;

	media.finish().unwrap();
	broadcast.close().unwrap();
	second.shutdown();
	second.closed().await.unwrap();
	server.cancel();
	drop(accept.await.unwrap());
}

/// A foreign cancel only drops the `available()` future, so a caller retrying after a timeout
/// keeps the same handle. Dropped while the instance is covered but not yet served, the retry
/// must still hand that instance out rather than wait for another.
#[tokio::test]
async fn announced_broadcast_retries_a_dropped_wait() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "");
	let followed = origin.consume().announced_broadcast("live".into()).unwrap();

	let waiting = followed.clone();
	let dropped = tokio::spawn(async move { waiting.available().await });
	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("the wait never asked for the broadcast")
		.unwrap();
	dropped.abort();
	assert!(matches!(dropped.await, Err(err) if err.is_cancelled()));

	let retry = tokio::spawn(async move { followed.available().await });
	let served = request.prepare(moq_mint_epoch()).unwrap().publish().unwrap();
	tokio::time::timeout(TIMEOUT, retry)
		.await
		.expect("the retry waited for another instance")
		.unwrap()
		.unwrap();
	served.broadcast().close().unwrap();
}

/// Another instance taking over while `available()` resolves is the one it hands out, so the
/// restart that announced it must not hand it out again while the caller holds it. `dynamic_at`
/// is where the route the wait first resolves through covers the path: above it, the takeover's
/// restart lands while resolving; at it, the origin holds that restart for
/// the default [`moq_net::origin::Config::update_hold`].
async fn announced_broadcast_hands_out_a_takeover_once(dynamic_at: &str) {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, dynamic_at);
	let followed = origin.consume().announced_broadcast("live".into()).unwrap();

	let waiting = followed.clone();
	let first = tokio::spawn(async move { waiting.available().await });
	// The wait is on the dynamic route's answer when the exact path takes over without an epoch.
	let _unanswered = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("the wait never asked for the broadcast")
		.unwrap();
	let takeover = create_announced(&origin, "live");
	let _held = tokio::time::timeout(TIMEOUT, first)
		.await
		.expect("the takeover never resolved")
		.unwrap()
		.unwrap();

	let next = tokio::spawn(async move { followed.available().await });
	tokio::time::sleep(moq_net::origin::Config::default().update_hold * 2).await;
	assert!(!next.is_finished(), "the takeover was handed out twice");
	next.abort();
	takeover.close().unwrap();
}

#[tokio::test]
async fn announced_broadcast_hands_out_a_takeover_once_while_resolving() {
	announced_broadcast_hands_out_a_takeover_once("").await;
}

#[tokio::test]
async fn announced_broadcast_hands_out_a_held_takeover_once() {
	announced_broadcast_hands_out_a_takeover_once("live").await;
}

/// An end is a gap that ended every request on the old route, even when a claim above the path
/// answers under the same epoch. So the instance serving the path afterwards is handed out
/// again, though its epoch is the one the caller still holds.
#[tokio::test]
async fn announced_broadcast_hands_out_the_instance_again_after_an_end() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let followed = origin.consume().announced_broadcast("live".into()).unwrap();
	let route = MoqAnnounceRoute {
		epoch: Some(moq_mint_epoch()),
		..Default::default()
	};
	let publication = origin.create_broadcast("live".into()).unwrap();
	publication.announce(route.clone()).unwrap();
	let first = tokio::time::timeout(TIMEOUT, followed.available())
		.await
		.expect("timed out waiting for the broadcast")
		.unwrap();

	// A covering claim answering under the same epoch.
	let dynamic = origin.inner().inner.requests("").unwrap();
	publication.unannounce().unwrap();
	let again = tokio::spawn(async move { followed.available().await });
	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("the instance never asked the route above the path")
		.unwrap();
	let epoch = route.epoch.clone().unwrap().parse().unwrap();
	let served = request.prepare(epoch).unwrap().publish().unwrap();

	let again = tokio::time::timeout(TIMEOUT, again)
		.await
		.expect("the instance was never handed out again")
		.unwrap()
		.unwrap();
	assert_eq!(
		again.inner().info().epoch,
		first.inner().info().epoch,
		"the claim above did not answer the same instance"
	);
	drop((served, dynamic));
	publication.broadcast().close().unwrap();
}

/// The route a wait resolves through leaving, and another instance arriving before it resolves,
/// reach the follower as an end and a start after the wait hands the newcomer out. That start
/// must not hand it out again.
#[tokio::test]
async fn announced_broadcast_hands_out_a_gap_while_resolving_once() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "");
	let followed = origin.consume().announced_broadcast("live".into()).unwrap();

	let waiting = followed.clone();
	let first = tokio::spawn(async move { waiting.available().await });
	let _unanswered = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("the wait never asked for the broadcast")
		.unwrap();
	dynamic.cancel();
	tokio::time::sleep(moq_net::origin::Config::default().update_hold * 2).await;
	let arrival = create_announced(&origin, "live");
	let _held = tokio::time::timeout(TIMEOUT, first)
		.await
		.expect("the arrival never resolved")
		.unwrap()
		.unwrap();

	let next = tokio::spawn(async move { followed.available().await });
	tokio::time::sleep(moq_net::origin::Config::default().update_hold * 2).await;
	assert!(!next.is_finished(), "the arrival was handed out twice");
	next.abort();
	arrival.close().unwrap();
}

/// A handle the caller dropped no longer counts as handed out, so the restart that names it
/// hands it out again rather than leaving the caller with nothing.
#[tokio::test]
async fn announced_broadcast_hands_out_a_dropped_takeover_again() {
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let dynamic = serve(&origin, "live");
	let followed = origin.consume().announced_broadcast("live".into()).unwrap();

	let waiting = followed.clone();
	let first = tokio::spawn(async move { waiting.available().await });
	let _unanswered = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("the wait never asked for the broadcast")
		.unwrap();
	let takeover = create_announced(&origin, "live");
	drop(
		tokio::time::timeout(TIMEOUT, first)
			.await
			.expect("the takeover never resolved")
			.unwrap()
			.unwrap(),
	);

	tokio::time::timeout(TIMEOUT, followed.available())
		.await
		.expect("the dropped takeover was never handed out again")
		.unwrap();
	takeover.close().unwrap();
}

#[tokio::test]
async fn server_client_roundtrip_auto_origin() {
	// Same shape as `server_client_roundtrip` but the client config omits
	// origins: the auto-created origin sides on
	// `MoqClientSession` are what drive publishing and subscribing.
	let server_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		publish: Some(server_origin.clone()),
		..Default::default()
	})
	.unwrap();

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	// No configured origins, so this uses the auto-origin path.
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		..Default::default()
	})
	.unwrap();
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let publisher = cs.publish();
	let consumer = cs.consume();

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	// Server publishes; client receives via the auto consumer.
	let broadcast = create_announced(&server_origin, "hello");
	let init = opus_head();
	let media = MoqMediaTrackProducer::audio(
		&broadcast,
		MoqMediaTarget::Named { name: None },
		audio_init(MoqAudioFormat::Opus, init),
	)
	.unwrap();

	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;
	assert_eq!(announcement.prefix, "hello");

	// With neither side wired, both share one origin, so a broadcast announced on this
	// session's publisher is discoverable through its own consumer.
	let local_broadcast = create_announced(&publisher, "local-only");
	// Visibility is asynchronous, so wait for the announcement rather than requesting.
	let local_announced = consumer.announced_broadcast("local-only".into()).unwrap();
	tokio::time::timeout(TIMEOUT, local_announced.available())
		.await
		.expect("timed out waiting for the loopback broadcast")
		.expect("an auto-origin session should discover its own announcement");
	local_broadcast.close().unwrap();

	media.finish().unwrap();
	broadcast.close().unwrap();
	cs.shutdown();
	cs.closed().await.unwrap();
	server_session.cancel(0);
	server.cancel();
}

#[tokio::test]
async fn server_new_validates_the_config() {
	let server = |bind: &str| {
		MoqServer::new(MoqServerConfig {
			bind: Some(bind.into()),
			..Default::default()
		})
	};
	assert!(server("127.0.0.1:0").is_ok());
	assert!(server("[::]:443").is_ok());
	assert!(server("localhost:4443").is_ok());
	assert!(matches!(server("localhost:443:8443"), Err(MoqError::Config { .. })));
	assert!(matches!(server("not-an-address"), Err(MoqError::Config { .. })));

	let version = MoqServer::new(MoqServerConfig {
		versions: vec!["moq-lite-99".into()],
		..Default::default()
	});
	assert!(matches!(version, Err(MoqError::Config { .. })));

	// A build that cannot write qlog refuses the directory here, not later as a bind error.
	let qlog = MoqServer::new(MoqServerConfig {
		quic: MoqQuicConfig {
			qlog: Some(std::env::temp_dir().display().to_string()),
			..Default::default()
		},
		..Default::default()
	});
	if moq_qlog_supported() {
		assert!(qlog.is_ok());
	} else {
		assert!(matches!(qlog, Err(MoqError::Config { .. })));
	}
}

/// A settings UI shows these, so each must be what an unset knob actually dials with.
#[tokio::test]
async fn client_defaults_are_what_an_unset_config_dials_with() {
	let defaults = moq_client_defaults();

	let connect = moq_tokio::connect::Config::default().resolve();
	assert_eq!(defaults.timeout, Some(connect.timeout));
	assert_eq!(defaults.race, Some(connect.race));
	assert_eq!(defaults.resolution_delay, Some(connect.resolution_delay));

	// Reading the defaults back through the conversion `new` uses lands on the same values.
	let quic = moq_tokio::quic::Config::from(defaults.quic.clone()).resolve();
	let unset = moq_tokio::quic::Config::default().resolve();
	assert_eq!(quic.max_streams, unset.max_streams);
	assert_eq!(quic.idle_timeout, unset.idle_timeout);
	assert_eq!(quic.keep_alive, unset.keep_alive);
	assert_eq!(defaults.websocket.enabled, Some(true));
	let backoff = moq_tokio::connection::Backoff::default();
	assert_eq!(defaults.backoff.initial, Some(backoff.initial()));
	assert_eq!(defaults.backoff.max, Some(backoff.max()));
	assert_eq!(defaults.backoff.timeout, Some(backoff.timeout()));

	// The backend picks these, so there is no single value to report.
	assert_eq!(defaults.tls.system_roots, None);
	assert_eq!(defaults.quic.gso, None);
	assert_eq!(defaults.quic.mtu_discovery, None);
	assert_eq!(defaults.quic.congestion_control, None);
	assert_eq!(defaults.quic.qlog, None);
	assert_eq!(defaults.quic.keylog, None);

	assert!(MoqClient::new(defaults).is_ok());
}

/// Every listed version is one `versions` accepts.
#[tokio::test]
async fn versions_are_accepted_by_the_client() {
	let versions = moq_versions();
	assert!(!versions.is_empty());
	let client = MoqClient::new(MoqClientConfig {
		versions,
		..Default::default()
	});
	assert!(client.is_ok());
}

#[tokio::test]
async fn client_new_validates_the_connection_knobs() {
	let client = |tls: MoqClientTls, quic: MoqQuicConfig| {
		MoqClient::new(MoqClientConfig {
			tls,
			quic,
			..Default::default()
		})
	};

	let host_name = MoqClientTls {
		host_name: Some("not a host".into()),
		..Default::default()
	};
	assert!(matches!(
		client(host_name, MoqQuicConfig::default()),
		Err(MoqError::Config { .. })
	));

	// A build that cannot write qlog refuses the directory rather than dropping it.
	let qlog = MoqQuicConfig {
		qlog: Some(std::env::temp_dir().display().to_string()),
		..Default::default()
	};
	assert_eq!(client(MoqClientTls::default(), qlog).is_ok(), moq_qlog_supported());
}

/// A client with `quic.keylog` on, dialing `localhost` pinned to `fingerprints`.
fn keylog_client(fingerprints: Vec<String>) -> Result<Arc<MoqClient>, MoqError> {
	MoqClient::new(MoqClientConfig {
		tls: MoqClientTls {
			host_name: Some("localhost".into()),
			fingerprints,
			..Default::default()
		},
		quic: MoqQuicConfig {
			keylog: Some(true),
			..Default::default()
		},
		websocket: MoqWebSocketConfig {
			enabled: Some(false),
			delay: None,
		},
		..Default::default()
	})
}

/// `quic.keylog` reaches the backend, which refuses it without `SSLKEYLOGFILE`.
#[test]
fn client_keylog_requires_sslkeylogfile() {
	if !test_env::Env::without(&["SSLKEYLOGFILE"]).isolate() {
		return;
	}
	assert!(matches!(keylog_client(Vec::new()), Err(MoqError::Config { .. })));
}

/// `quic.keylog` writes the client's session secrets to the file `SSLKEYLOGFILE` names.
#[tokio::test]
async fn client_keylog_writes_session_keys() {
	let path = std::env::temp_dir().join(format!("moq-ffi-keylog-{}.log", std::process::id()));
	if !test_env::Env::default().set("SSLKEYLOGFILE", &path).isolate() {
		return;
	}
	// A child's process id, and so the path it would pick, is not its parent's.
	let path = std::env::var_os("SSLKEYLOGFILE").expect("isolate sets SSLKEYLOGFILE");
	let _ = std::fs::remove_file(&path);

	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	let client = keylog_client(server.cert_fingerprints().unwrap()).unwrap();
	let session = tokio::time::timeout(TIMEOUT, client.connect(format!("https://{addr}")))
		.await
		.expect("connect timed out")
		.expect("connect failed");
	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	let log = std::fs::read_to_string(&path).unwrap_or_default();
	let _ = std::fs::remove_file(&path);
	assert!(
		log.lines().any(|line| line.starts_with("CLIENT_TRAFFIC_SECRET_0 ")),
		"quic.keylog wrote no session keys: {log:?}"
	);

	session.cancel(0);
	server_session.cancel(0);
	client.cancel();
	server.cancel();
}

/// Every connection knob set at once still dials, so none of them is refused or misapplied.
#[tokio::test]
async fn client_connects_with_every_knob_set() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	// Pinned by fingerprint, with the SNI override naming the certificate's host.
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	let client = MoqClient::new(MoqClientConfig {
		bind: Some("127.0.0.1:0".into()),
		timeout: Some(Duration::from_secs(5)),
		race: Some(Duration::ZERO),
		resolution_delay: Some(Duration::ZERO),
		tls: MoqClientTls {
			host_name: Some("localhost".into()),
			fingerprints: server.cert_fingerprints().unwrap(),
			..Default::default()
		},
		quic: MoqQuicConfig {
			max_streams: Some(64),
			idle_timeout: Some(Duration::from_secs(5)),
			keep_alive: Some(Duration::ZERO),
			gso: Some(false),
			mtu_discovery: Some(true),
			congestion_control: Some(MoqCongestionControl::Loss),
			qlog: None,
			keylog: Some(false),
		},
		websocket: MoqWebSocketConfig {
			enabled: Some(false),
			delay: None,
		},
		..Default::default()
	})
	.unwrap();
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");
	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	session.cancel(0);
	server_session.cancel(0);
	client.cancel();
	server.cancel();
}

#[tokio::test]
async fn server_cert_fingerprints_available_after_listen() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	// Not available before listen().
	assert!(matches!(
		server.cert_fingerprints(),
		Err(crate::error::MoqError::Bind { .. })
	));

	tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");

	let fps = server.cert_fingerprints().expect("fingerprints available");
	assert_eq!(fps.len(), 1, "one generated cert => one fingerprint");
	// Hex-encoded SHA-256 is 64 chars.
	assert_eq!(fps[0].len(), 64, "fingerprint should be hex SHA-256");
	assert!(fps[0].chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn server_cert_fingerprints_rejected_after_cancel() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	server.cert_fingerprints().expect("fingerprints available");

	// Cancel is terminal the moment it returns. `Cancelled`, not `Bind`: the wrappers'
	// `is_shutdown` helpers read the variant to tell a teardown from a real bind failure.
	server.cancel();
	assert!(matches!(
		server.cert_fingerprints(),
		Err(crate::error::MoqError::Cancelled)
	));
}

/// Cancelling a listening server releases its socket before it returns, so the
/// same address binds again without a retry. The accept is parked first on this
/// thread, so cancel has to close the listener without the lock that accept holds.
#[tokio::test]
async fn server_cancel_releases_the_bound_port() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = server.listen().await.expect("listen failed");

	// Park an accept on the server lock, the state a live server is closed in.
	let accepting = server.clone();
	let accept = tokio::spawn(async move { accepting.accept().await });
	wait_for_config_error(
		|| server.cert_fingerprints().map(drop),
		|err| matches!(err, MoqError::Busy),
	)
	.await;

	server.cancel();

	// A raw bind from this thread races the teardown directly rather than
	// queueing behind it on the FFI runtime, so it only succeeds if cancel
	// released the socket before returning.
	std::net::UdpSocket::bind(&addr).expect("cancel should release the socket before it returns");

	// No retry: the socket is already closed, so this must succeed on the first try.
	let rebound = MoqServer::new(MoqServerConfig {
		bind: Some(addr.clone()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	rebound.listen().await.expect("the port should rebind immediately");

	let accept = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept task timed out")
		.expect("accept task panicked");
	assert!(matches!(accept, Err(MoqError::Cancelled)));

	rebound.cancel();
}

/// A cancel racing an in-flight listen still releases the port before it returns. The listen is
/// polled once and then left unpolled, as a foreign cancel leaves it until `rust_future_free`.
#[tokio::test]
async fn server_cancel_during_listen_releases_the_port() {
	let addr = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
	let server = MoqServer::new(MoqServerConfig {
		bind: Some(addr.to_string()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	let waker = std::task::Waker::noop();
	let mut listen = Box::pin(server.listen());
	let _ = listen.as_mut().poll(&mut std::task::Context::from_waker(waker));
	assert!(
		std::net::UdpSocket::bind(addr).is_err(),
		"the polled listen should have bound the port"
	);

	server.cancel();
	std::net::UdpSocket::bind(addr).expect("cancel should release the socket before it returns");
	drop(listen);
}

/// A foreign cancel only stops polling an accept; uniffi drops it later, at `rust_future_free`.
/// Until then the cancelled accept must not take the session the next accept is waiting for.
#[tokio::test]
async fn server_accept_cancelled_before_free_leaves_the_session() {
	struct Woken(tokio::sync::Notify);
	impl std::task::Wake for Woken {
		fn wake(self: Arc<Self>) {
			self.0.notify_one();
		}
	}

	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = server.listen().await.expect("listen failed");

	// Poll once, then stop without dropping, which is all `rust_future_cancel` does.
	let woken = Arc::new(Woken(tokio::sync::Notify::new()));
	let waker = std::task::Waker::from(woken.clone());
	let mut cancelled = Box::pin(server.accept());
	assert!(
		cancelled
			.as_mut()
			.poll(&mut std::task::Context::from_waker(&waker))
			.is_pending()
	);

	// Dial once: a redial would hand the next accept a fresh session and hide the lost one.
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();
	let connect = tokio::spawn(async move { client.connect(format!("https://{addr}")).await });

	// The wake means the session is ready for the cancelled accept, wherever it now sits.
	tokio::time::timeout(TIMEOUT, woken.0.notified())
		.await
		.expect("the session never reached the cancelled accept");
	drop(cancelled);

	let request = tokio::time::timeout(TIMEOUT, server.accept())
		.await
		.expect("the cancelled accept took the session")
		.expect("accept errored")
		.expect("accept returned None");
	let server_session = request.accept(None, None).await.expect("accept the session");
	let _client_session = tokio::time::timeout(TIMEOUT, connect)
		.await
		.expect("connect timed out")
		.expect("connect task panicked")
		.expect("connect failed");

	server_session.cancel(0);
	server.cancel();
}

#[tokio::test]
async fn request_double_respond_returns_already_responded() {
	use crate::error::MoqError;

	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = server.listen().await.expect("listen failed");

	let url = format!("https://{addr}");
	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");

		// Accept once, then try a second response. It must error.
		let session = request.accept(None, None).await.expect("first ok succeeds");
		let second_ok = request.accept(None, None).await;
		assert!(
			matches!(second_ok, Err(MoqError::AlreadyResponded)),
			"second ok() must fail"
		);
		let second_close = request.reject(403).await;
		assert!(
			matches!(second_close, Err(MoqError::AlreadyResponded)),
			"close after ok must fail"
		);
		session
	});

	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		..Default::default()
	})
	.unwrap();
	let _session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept timed out")
		.expect("accept task panicked");

	server_session.cancel(0);
	server.cancel();
}

#[tokio::test]
async fn request_per_session_publish_override() {
	// The server's publish origin is empty; a per-request override is used instead.
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	let addr = server.listen().await.expect("listen failed");
	let url = format!("https://{addr}");

	let override_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let override_for_task = override_origin.clone();

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		// Override publish on a per-request basis.
		request
			.accept(Some(override_for_task), None)
			.await
			.expect("ok succeeds")
	});

	let client_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		consume: Some(client_origin.clone()),
		..Default::default()
	})
	.unwrap();
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept timed out")
		.expect("accept task panicked");

	// Publishing on the override origin must reach the client.
	let broadcast = create_announced(&override_origin, "override-only");

	let consumer = client_origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;
	assert_eq!(announcement.prefix, "override-only");

	broadcast.close().unwrap();
	cs.cancel(0);
	server_session.cancel(0);
	server.cancel();
}

async fn wait_phase(
	session: &MoqSession,
	phase: MoqConnectionPhase,
) -> Result<crate::session::MoqConnectionState, MoqError> {
	let mut state = session.state();
	while state.phase != phase {
		state = session.state_changed(state.revision).await?;
	}
	Ok(state)
}

/// The session exposes repeated first-dial failures even after its Client is released.
#[tokio::test]
async fn session_state_names_a_failing_first_dial() {
	let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
	let addr = silent.local_addr().unwrap();
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		timeout: Some(Duration::from_millis(100)),
		websocket: MoqWebSocketConfig {
			enabled: Some(false),
			..Default::default()
		},
		backoff: fast_backoff(),
		..Default::default()
	})
	.unwrap();
	let session = client.connect(format!("https://{addr}")).await.unwrap();
	let mut state = session.state();
	assert_eq!(state.phase, MoqConnectionPhase::Connecting);
	assert_eq!(state.connects, 0);
	assert!(state.version.is_none());
	drop(client);
	for _ in 0..2 {
		state = tokio::time::timeout(TIMEOUT, session.state_changed(state.revision))
			.await
			.unwrap()
			.unwrap();
		assert_eq!(state.phase, MoqConnectionPhase::Connecting);
		assert_eq!(state.connects, 0);
		assert!(matches!(state.failure, Some(MoqError::Connect { .. })));
	}
	drop(silent);
	let server = MoqServer::new(MoqServerConfig {
		bind: Some(addr.to_string()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	server.listen().await.unwrap();
	let request = server.accept().await.unwrap().unwrap();
	let accepted = request.accept(None, None).await.unwrap();
	let connected = tokio::time::timeout(TIMEOUT, wait_phase(&session, MoqConnectionPhase::Connected))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(connected.connects, 1);
	assert!(connected.failure.is_none());
	assert!(connected.version.is_some());
	let initial_accepted = accepted.state();
	assert_eq!(initial_accepted.phase, MoqConnectionPhase::Connected);
	assert_eq!(initial_accepted.connects, 1);
	let ending = accepted.state_changed(initial_accepted.revision);
	tokio::pin!(ending);
	assert!(
		std::future::poll_fn(|cx| std::task::Poll::Ready(ending.as_mut().poll(cx)))
			.await
			.is_pending()
	);
	accepted.cancel(0);
	let terminal = tokio::time::timeout(TIMEOUT, ending).await.unwrap().unwrap();
	assert_eq!(terminal.phase, MoqConnectionPhase::Closed);
	assert!(terminal.version.is_none());
	assert!(terminal.failure.is_some());
	assert_eq!(accepted.state().revision, terminal.revision);
	assert!(accepted.state_changed(terminal.revision).await.is_err());
	session.cancel(0);
	session.closed().await.unwrap();
	assert_eq!(session.state().phase, MoqConnectionPhase::Closed);
	accepted.cancel(0);
	server.cancel();
}

/// The #2609 regression: a client session must ride out a transport drop on its
/// own. The server kills the first session; after the automatic redial, a
/// broadcast published on the server still reaches the client's consume origin.
/// With the old one-shot dial this stalled silently forever.
#[tokio::test]
async fn client_reconnects_and_resumes_announcements() {
	let server_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		publish: Some(server_origin.clone()),
		..Default::default()
	})
	.unwrap();

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	// Hand the first session back to the test body (so the kill happens only after
	// the client observed the connect), and gate the second accept so the
	// disconnected state is observable: until the gate opens, the client's redial
	// has no session to complete against.
	let (first_tx, first_rx) = tokio::sync::oneshot::channel();
	let (regate_tx, regate_rx) = tokio::sync::oneshot::channel::<()>();
	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let first = accept_server
			.accept()
			.await
			.expect("first accept errored")
			.expect("first accept returned None");
		let first = first.accept(None, None).await.expect("first handshake failed");
		if first_tx.send(first).is_err() {
			panic!("test body gone");
		}
		regate_rx.await.expect("regate dropped");

		let second = accept_server
			.accept()
			.await
			.expect("second accept errored")
			.expect("second accept returned None");
		second.accept(None, None).await.expect("second handshake failed")
	});

	let client_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		consume: Some(client_origin.clone()),
		// Fast retries so the test doesn't wait out the default 1s backoff.
		backoff: fast_backoff(),
		..Default::default()
	})
	.unwrap();

	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	// Observe the first successful connect through the same state as its initial dial.
	let status = tokio::time::timeout(TIMEOUT, wait_phase(&cs, MoqConnectionPhase::Connected))
		.await
		.expect("status timed out")
		.expect("status errored");
	assert_eq!(status.phase, MoqConnectionPhase::Connected);
	assert_eq!(status.connects, 1);
	assert!(status.failure.is_none(), "nothing has failed yet");
	let version = status.version.expect("a live connection reports its version");
	crate::session::parse_versions(&[version]).expect("the version is spelled like the config's");

	// Kill the transport under the client, simulating a relay restart.
	// Nothing accepts the redial until the gate opens.
	let first = tokio::time::timeout(TIMEOUT, first_rx)
		.await
		.expect("first session timed out")
		.expect("accept task gone");
	first.cancel(0);

	let status = tokio::time::timeout(TIMEOUT, wait_phase(&cs, MoqConnectionPhase::Disconnected))
		.await
		.expect("disconnect status timed out")
		.expect("disconnect status errored");
	assert_eq!(status.phase, MoqConnectionPhase::Disconnected);
	// Recorded before the status moved, so the watcher woken by the drop finds why.
	assert!(status.failure.is_some(), "the drop must report its reason");
	assert!(status.version.is_none());
	assert!(cs.stats().version.is_none(), "nothing is live to report a version");

	// Open the gate; the redial completes.
	regate_tx.send(()).expect("accept task gone");
	let status = tokio::time::timeout(TIMEOUT, wait_phase(&cs, MoqConnectionPhase::Connected))
		.await
		.expect("reconnect status timed out")
		.expect("reconnect status errored");
	assert_eq!(status.phase, MoqConnectionPhase::Connected);
	assert!(status.failure.is_none(), "the reconnect must clear the failure");
	assert!(status.version.is_some());
	assert!(cs.stats().version.is_some(), "the reconnect reports its version");

	assert_eq!(status.connects, 2, "phase and count come from one observation");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	// A broadcast published only after the reconnect must reach the client.
	let broadcast = create_announced(&server_origin, "after-reconnect");

	let consumer = client_origin.consume();
	let announced = consumer.announced(MoqAnnounceConfig::default()).unwrap();
	let announcement = next_announced(&announced).await;
	assert_eq!(announcement.prefix, "after-reconnect");

	broadcast.close().unwrap();
	cs.cancel(0);
	server_session.cancel(0);
	server.cancel();
}

/// With reconnecting disabled the old contract holds: the transport's close ends
/// the session, surfacing through `closed()` instead of a redial.
#[tokio::test]
async fn one_shot_client_close_surfaces_through_closed() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();

	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	server_session.cancel(7);
	tokio::time::timeout(TIMEOUT, cs.closed())
		.await
		.expect("closed timed out")
		.expect_err("a severed one-shot session must surface as an error");

	server.cancel();
}

/// A rejection at the MoQ layer reaches the client as an untyped transport
/// close, which the reconnect loop retries like any other drop. One-shot mode
/// is how a caller observes the rejection directly; mirrors py
/// test_server_request_close, which drives the same path through the bindings.
#[tokio::test]
async fn rejected_session_surfaces_through_closed() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let reject = tokio::spawn(async move {
		loop {
			let Ok(Some(request)) = accept_server.accept().await else {
				return;
			};
			request.reject(403).await.expect("reject failed");
		}
	});

	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();

	// Either the dial fails outright, or the optimistic connect resolves and the
	// rejection lands as the session's terminal close. Both must surface within
	// the timeout.
	if let Ok(cs) = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect neither resolved nor failed")
	{
		tokio::time::timeout(TIMEOUT, cs.closed())
			.await
			.expect("closed timed out")
			.expect_err("a rejected session must surface as an error");
	}

	reject.abort();
	server.cancel();
}

/// `MoqClient::cancel` must abort connects even when called first, reconnect
/// loop or not; the kt BindingsSmokeTest relies on this to fail fast.
#[tokio::test]
async fn cancel_before_connect_fails_fast() {
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		..Default::default()
	})
	.unwrap();
	client.cancel();
	let result = tokio::time::timeout(
		Duration::from_secs(5),
		client.connect("https://localhost:0/test".into()),
	)
	.await
	.expect("connect did not fail fast");
	let Err(err) = result else {
		panic!("connect must fail after cancel");
	};
	assert!(matches!(err, MoqError::Cancelled), "unexpected error: {err}");
}

/// A caller that stops waiting must not swallow the event it gave up on.
/// Dropping the future returned by a `Task::run` call used to leave the spawned
/// closure detached and still holding the state lock, so it consumed the next
/// transition into its own cursor and the retry blocked behind it, missing the
/// edge. Every repeatable read on the bindings (`status`, `next`, `read_frame`,
/// `recv_datagram`) sits on that path; `status` is just the easiest to drive.
#[tokio::test]
async fn cancelled_state_wait_does_not_swallow_the_next_transition() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	// Accept exactly once. Nothing serves the redial, so `Disconnected` is the
	// only transition left after the kill: if it gets eaten, nothing replaces it.
	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		backoff: fast_backoff(),
		..Default::default()
	})
	.unwrap();

	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	let status = tokio::time::timeout(TIMEOUT, wait_phase(&cs, MoqConnectionPhase::Connected))
		.await
		.expect("status timed out")
		.expect("status errored");
	assert_eq!(status.phase, MoqConnectionPhase::Connected);

	// Give up on a status that isn't coming. The window is generous on purpose:
	// the abandoned call has to actually reach its await for this to prove
	// anything, and a spawn that never got there would pass either way.
	assert!(
		tokio::time::timeout(Duration::from_millis(200), cs.state_changed(status.revision))
			.await
			.is_err(),
		"no transition was pending, so this must be the caller giving up",
	);

	// The transition the abandoned call would have eaten.
	server_session.cancel(0);

	let status = tokio::time::timeout(TIMEOUT, cs.state_changed(status.revision))
		.await
		.expect("the cancelled waiter swallowed the disconnect")
		.expect("status errored");
	assert_eq!(status.phase, MoqConnectionPhase::Disconnected);

	cs.cancel(0);
	server.cancel();
}

/// The built-in encoder's applied bitrate follows a shrinking grant.
#[cfg(feature = "video")]
#[tokio::test]
async fn video_encoder_follows_a_shrinking_grant() {
	use crate::bandwidth::MoqAllocator;
	use crate::video::*;

	let estimate = moq_net::bandwidth::Producer::new();
	let bandwidth = std::sync::Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::new(
		estimate.consume(),
	)));

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input: MoqVideoEncoderInput {
				format: MoqVideoPixelFormat::Rgba,
				width: 320,
				height: 240,
				framerate: crate::video::MoqVideoFrameRate {
					numerator: 30,
					denominator: 1,
				},
			},
			output: MoqVideoEncoderOutput {
				codec: MoqVideoCodec::H264,
				bitrate: Some(4_000_000),
				gop: None,
				kind: MoqVideoEncoderKind::Software,
			},
			track: Some("camera".into()),
			bandwidth: Some(bandwidth),
			lower: None,
		},
	)
	.unwrap();

	let reservation = video.reservation().expect("published against an allocator");
	assert_eq!(reservation.rate(), None, "no estimate yet");

	let consumer = broadcast.consume().unwrap();
	let _sub = consumer.subscribe_track("camera".into(), None).await.unwrap();

	estimate
		.set(Some(moq_net::bandwidth::Rate::from_bps(4_000_000)))
		.unwrap();
	assert_eq!(reservation.rate(), Some(4_000_000));

	estimate
		.set(Some(moq_net::bandwidth::Rate::from_bps(1_000_000)))
		.unwrap();
	assert_eq!(reservation.rate(), Some(1_000_000));

	let deadline = moq_clock::Instant::now() + TIMEOUT;
	loop {
		if video.applied_bitrate() == 1_000_000 {
			break;
		}
		assert!(
			moq_clock::Instant::now() < deadline,
			"encoder did not follow the shrinking grant, last applied {}",
			video.applied_bitrate()
		);
		tokio::time::sleep(Duration::from_millis(10)).await;
	}

	video.finish().unwrap();
	broadcast.close().unwrap();
}

#[cfg(feature = "video")]
fn software_camera(bitrate: u64) -> (crate::video::MoqVideoEncoderInput, crate::video::MoqVideoEncoderOutput) {
	use crate::video::*;
	(
		MoqVideoEncoderInput {
			format: MoqVideoPixelFormat::Rgba,
			width: 320,
			height: 240,
			framerate: crate::video::MoqVideoFrameRate {
				numerator: 30,
				denominator: 1,
			},
		},
		MoqVideoEncoderOutput {
			codec: MoqVideoCodec::H264,
			bitrate: Some(bitrate),
			gop: None,
			kind: MoqVideoEncoderKind::Software,
		},
	)
}

/// Dropping a producer parked in `changed()` must stop the follow thread, not
/// leak it until the session allocator dies.
#[cfg(feature = "video")]
#[tokio::test]
async fn dropping_a_video_producer_stops_the_rate_follower() {
	use crate::bandwidth::MoqAllocator;

	let estimate = moq_net::bandwidth::Producer::new();
	let bandwidth = std::sync::Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::new(
		estimate.consume(),
	)));
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let (input, output) = software_camera(4_000_000);
	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input,
			output,
			track: Some("camera".into()),
			bandwidth: Some(bandwidth),
			lower: None,
		},
	)
	.unwrap();

	tokio::time::timeout(TIMEOUT, tokio::task::spawn_blocking(move || drop(video)))
		.await
		.expect("rate follower did not stop")
		.expect("drop panicked");
}

/// `set_bitrate` is the manual ceiling: a later grant cannot retune above it.
#[cfg(feature = "video")]
#[tokio::test]
async fn set_bitrate_caps_a_later_bandwidth_grant() {
	use crate::bandwidth::MoqAllocator;

	let estimate = moq_net::bandwidth::Producer::new();
	let bandwidth = std::sync::Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::new(
		estimate.consume(),
	)));
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let (input, output) = software_camera(4_000_000);
	let video = crate::video::MoqVideoProducer::new(
		&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
		crate::video::MoqVideoProducerOptions {
			input,
			output,
			track: Some("camera".into()),
			bandwidth: Some(bandwidth),
			lower: None,
		},
	)
	.unwrap();
	let reservation = video.reservation().expect("published against an allocator");

	let consumer = broadcast.consume().unwrap();
	let _sub = consumer.subscribe_track("camera".into(), None).await.unwrap();
	estimate
		.set(Some(moq_net::bandwidth::Rate::from_bps(4_000_000)))
		.unwrap();
	assert_eq!(reservation.rate(), Some(4_000_000));

	video.set_bitrate(1_000_000).unwrap();
	assert_eq!(reservation.rate(), Some(1_000_000));
	assert_eq!(video.applied_bitrate(), 1_000_000);

	estimate
		.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
		.unwrap();
	assert_eq!(reservation.rate(), Some(1_000_000));
	assert_eq!(video.applied_bitrate(), 1_000_000);

	video.finish().unwrap();
	broadcast.close().unwrap();
}

async fn one_shot_peers() -> (Arc<MoqSession>, Arc<MoqSession>, Arc<MoqServer>) {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();
	let client_session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");
	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");
	(client_session, server_session, server)
}

fn protocol(err: MoqError) -> crate::error::MoqProtocolError {
	match err {
		MoqError::Protocol { details: protocol } => protocol,
		other => panic!("expected Protocol, got {other:?}"),
	}
}

/// A peer's session code survives the FFI: known, application, and unknown.
#[tokio::test]
async fn session_protocol_codes_cross_the_ffi() {
	use crate::error::{MoqErrorScope, MoqProtocolKind};

	for (code, kind) in [
		(0x2, MoqProtocolKind::Unauthorized),
		(64 + 404, MoqProtocolKind::App),
		(0x1f, MoqProtocolKind::Unknown),
	] {
		let (client, server_session, server) = one_shot_peers().await;
		server_session.cancel(code);
		let err = match tokio::time::timeout(TIMEOUT, client.closed())
			.await
			.expect("closed timed out")
		{
			Err(err) => err,
			Ok(()) => panic!("a session close with a code is an error"),
		};
		let protocol = protocol(err);
		assert_eq!(protocol.scope, MoqErrorScope::Session, "code {code:#x}");
		assert_eq!(protocol.code, code, "code {code:#x}");
		assert_eq!(protocol.kind, kind, "code {code:#x}");
		server.cancel();
	}
}
#[tokio::test]
async fn cancelling_a_session_aborts_its_first_dial() {
	let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
	let addr = silent.local_addr().unwrap();
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		once: true,
		websocket: MoqWebSocketConfig {
			enabled: Some(false),
			delay: None,
		},
		..Default::default()
	})
	.unwrap();
	let session = client.connect(format!("https://{addr}")).await.unwrap();
	let revision = session.state().revision;
	client.cancel();
	session.cancel(0);
	tokio::time::timeout(TIMEOUT, session.closed()).await.unwrap().unwrap();
	let state = session.state_changed(revision).await.unwrap();
	assert_eq!(state.phase, MoqConnectionPhase::Closed);
	assert_eq!(state.connects, 0);
	assert!(matches!(
		session.state_changed(state.revision).await,
		Err(MoqError::Closed)
	));
}

#[tokio::test]
async fn server_cert_fingerprints_busy_during_accept_and_cancelled_after() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	server.listen().await.expect("listen failed");
	server
		.cert_fingerprints()
		.expect("fingerprints available between accepts");

	let accepting = server.clone();
	let accept = tokio::spawn(async move { accepting.accept().await });

	wait_for_config_error(
		|| server.cert_fingerprints().map(drop),
		|err| matches!(err, MoqError::Busy),
	)
	.await;

	server.cancel();
	assert!(matches!(server.cert_fingerprints(), Err(MoqError::Cancelled)));

	let accept_err = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept task timed out")
		.expect("accept task panicked");
	assert!(matches!(accept_err, Err(MoqError::Cancelled)));
}

// Queue accept behind another operation so the origin arguments must survive the
// caller releasing its handles before the handshake starts on the FFI runtime.
#[tokio::test]
async fn request_accept_inherits_overrides_and_captures_origins() {
	for (override_publish, override_consume) in [(false, false), (true, false), (false, true), (true, true)] {
		let default_publish = MoqOriginProducer::new(MoqOriginConfig::default());
		let default_consume = MoqOriginProducer::new(MoqOriginConfig::default());
		let fresh = MoqOriginProducer::new(MoqOriginConfig::default());
		let publish = override_publish.then(|| fresh.clone());
		let consume = override_consume.then(|| fresh.clone());
		let expected_publish = publish.as_ref().unwrap_or(&default_publish).inner().hop();
		let expected_consume = consume.as_ref().unwrap_or(&default_consume).consume();
		let captured = Arc::downgrade(&fresh);
		let server = MoqServer::new(MoqServerConfig {
			bind: Some("127.0.0.1:0".into()),
			tls: localhost_tls(),
			publish: Some(default_publish),
			consume: Some(default_consume),
			..Default::default()
		})
		.unwrap();
		let addr = server.listen().await.unwrap();
		let client = MoqClient::new(MoqClientConfig {
			tls: insecure_tls(),
			bind: Some("127.0.0.1:0".into()),
			once: true,
			..Default::default()
		})
		.unwrap();
		let connecting = client.clone();
		let connect = tokio::spawn(async move { connecting.connect(format!("https://{addr}")).await });
		let request = tokio::time::timeout(TIMEOUT, server.accept())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		let (held_tx, held_rx) = tokio::sync::oneshot::channel();
		let (release_tx, release_rx) = tokio::sync::oneshot::channel();
		let holding = request.clone();
		let hold = tokio::spawn(async move {
			holding
				.hold_lock(|| async move {
					let _ = held_tx.send(());
					let _ = release_rx.await;
				})
				.await
		});
		tokio::time::timeout(TIMEOUT, held_rx).await.unwrap().unwrap();
		let accepting = request.clone();
		let accept = spawn_parked(async move { accepting.accept(publish, consume).await }).await;
		drop(fresh);
		if override_publish || override_consume {
			assert!(
				captured.upgrade().is_some(),
				"accept must own the origin arguments while queued"
			);
		}
		release_tx.send(()).unwrap();
		tokio::time::timeout(TIMEOUT, hold).await.unwrap().unwrap().unwrap();
		let session = tokio::time::timeout(TIMEOUT, accept).await.unwrap().unwrap().unwrap();
		let client_session = tokio::time::timeout(TIMEOUT, connect).await.unwrap().unwrap().unwrap();
		assert_eq!(session.publish().inner().hop(), expected_publish);

		// Incoming broadcasts reach the selected consume origin, independently
		// of which publish side was selected.
		let announced = expected_consume.announced(MoqAnnounceConfig::default()).unwrap();
		let broadcast = create_announced(&client_session.publish(), "incoming");
		assert_eq!(next_announced(&announced).await.prefix, "incoming");
		if override_publish && override_consume {
			// Both explicit arguments referred to one fresh origin, so the
			// publish handle must see broadcasts arriving on the consume side.
			let shared = session
				.publish()
				.consume()
				.announced(MoqAnnounceConfig::default())
				.unwrap();
			assert_eq!(next_announced(&shared).await.prefix, "incoming");
		}
		assert!(matches!(
			request.accept(None, None).await,
			Err(MoqError::AlreadyResponded)
		));
		broadcast.close().unwrap();
		client_session.cancel(0);
		session.cancel(0);
		server.cancel();
	}
}

#[tokio::test]
async fn request_accept_cancelled_after_cancel() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = server.listen().await.expect("listen failed");

	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();

	let connecting = client.clone();
	let connect = tokio::spawn(async move { connecting.connect(format!("https://{addr}")).await });

	let request = tokio::time::timeout(TIMEOUT, server.accept())
		.await
		.expect("accept timed out")
		.expect("accept errored")
		.expect("accept returned None");

	let (held_tx, held_rx) = tokio::sync::oneshot::channel();
	let (_release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
	let holding = request.clone();
	let hold = tokio::spawn(async move {
		holding
			.hold_lock(|| async move {
				let _ = held_tx.send(());
				let _ = release_rx.await;
			})
			.await
	});
	tokio::time::timeout(TIMEOUT, held_rx).await.unwrap().unwrap();
	let origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let captured = Arc::downgrade(&origin);
	let accepting = request.clone();
	let accept = spawn_parked(async move { accepting.accept(Some(origin), None).await }).await;
	assert!(captured.upgrade().is_some());

	request.cancel();
	assert!(matches!(request.accept(None, None).await, Err(MoqError::Cancelled)));
	assert!(matches!(request.reject(403).await, Err(MoqError::Cancelled)));

	let result = tokio::time::timeout(TIMEOUT, accept).await.unwrap().unwrap();
	assert!(matches!(result, Err(MoqError::Cancelled)));
	let result = tokio::time::timeout(TIMEOUT, hold).await.unwrap().unwrap();
	assert!(matches!(result, Err(MoqError::Cancelled)));
	assert!(
		captured.upgrade().is_none(),
		"cancel must release the queued origin arguments"
	);

	client.cancel();
	let _ = tokio::time::timeout(TIMEOUT, connect).await;
	server.cancel();
}

/// Stopping the runtime is process-wide, so a shutdown scenario runs in a child test process.
///
/// Returns true in the child. In the parent, re-runs this binary filtered to `test` with a
/// marker set, judges its exit, and returns false.
fn shutdown_child(test: &str) -> bool {
	const MARKER: &str = "MOQ_FFI_TEST_SHUTDOWN_CHILD";
	if std::env::var_os(MARKER).is_some() {
		return true;
	}
	let status = std::process::Command::new(std::env::current_exe().unwrap())
		.args(["--exact", &format!("test::{test}")])
		.env(MARKER, "1")
		.status()
		.expect("failed to spawn the child test process");
	assert!(status.success(), "child test process failed: {status}");
	false
}

#[tokio::test]
async fn shutdown_cancels_and_drops_cleanly() {
	if !shutdown_child("shutdown_cancels_and_drops_cleanly") {
		return;
	}

	let server_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		publish: Some(server_origin.clone()),
		..Default::default()
	})
	.unwrap();
	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server.accept().await.unwrap().expect("accept returned None");
		request.accept(None, None).await.expect("handshake failed")
	});

	let client_origin = MoqOriginProducer::new(MoqOriginConfig::default());
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		consume: Some(client_origin.clone()),
		..Default::default()
	})
	.unwrap();
	let session = tokio::time::timeout(TIMEOUT, client.connect(format!("https://{addr}")))
		.await
		.expect("connect timed out")
		.expect("connect failed");
	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept timed out")
		.expect("accept task panicked");

	// A track with no frames, so the read parks on the runtime.
	let broadcast = create_announced(&server_origin, "hello");
	let track = broadcast.publish_track("data".into(), None).unwrap();
	let consumer = client_origin.consume();
	let bc = await_announced(&consumer, "hello").await;
	let subscriber = tokio::time::timeout(TIMEOUT, bc.subscribe_track("data".into(), None))
		.await
		.expect("subscribe timed out")
		.expect("subscribe failed");
	let parked = spawn_parked({
		let subscriber = subscriber.clone();
		async move { subscriber.read_frame().await }
	})
	.await;

	crate::moq_ffi_shutdown();
	// Idempotent: the thread is already gone.
	crate::moq_ffi_shutdown();

	let parked = tokio::time::timeout(TIMEOUT, parked)
		.await
		.expect("the parked read never resolved")
		.expect("the read task panicked");
	assert!(matches!(parked, Err(MoqError::Cancelled)));
	assert!(matches!(subscriber.read_frame().await, Err(MoqError::Cancelled)));
	let closed = session.closed().await;
	assert!(matches!(closed, Err(MoqError::Cancelled)), "{closed:?}");

	// The interpreter frees these in an arbitrary order during finalization. The session's
	// transport close spawns onto the dead runtime, which must be dropped rather than a panic.
	drop(track);
	drop(broadcast);
	drop(subscriber);
	drop(bc);
	drop(consumer);
	drop(session);
	drop(server_session);
	drop(client);
	drop(server);
	drop(client_origin);
	drop(server_origin);
}

/// A `Task::run` polled in place on a host thread must finish its poll before shutdown stops
/// the drivers: a tokio timer polled after its driver is gone panics.
#[test]
fn shutdown_waits_for_an_in_place_poll() {
	if !shutdown_child("shutdown_waits_for_an_in_place_poll") {
		return;
	}

	let (entered, entered_rx) = std::sync::mpsc::channel();
	let (release, release_rx) = std::sync::mpsc::channel::<()>();
	let poller = std::thread::spawn(move || {
		let task = crate::ffi::Task::new(());
		let run = task.run(|_| async move {
			entered.send(()).unwrap();
			// Holds this poll open while shutdown starts on another thread.
			release_rx.recv().unwrap();
			tokio::time::sleep(Duration::from_millis(1)).await;
			// Even if the sleep is already due, only the shutdown may finish this call.
			std::future::pending::<Result<(), MoqError>>().await
		});
		tokio::runtime::Builder::new_current_thread()
			.build()
			.unwrap()
			.block_on(run)
	});
	entered_rx.recv().unwrap();

	let shutdown = std::thread::spawn(|| crate::moq_ffi_shutdown());

	// A fresh call resolves `Cancelled` on its first poll once shutdown has begun.
	let probe = crate::ffi::Task::new(());
	loop {
		let mut call = std::pin::pin!(probe.run(|_| std::future::pending::<Result<(), MoqError>>()));
		let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
		match call.as_mut().poll(&mut cx) {
			std::task::Poll::Ready(result) => {
				assert!(matches!(result, Err(MoqError::Cancelled)), "{result:?}");
				break;
			}
			std::task::Poll::Pending => std::thread::yield_now(),
		}
	}

	// Shutdown has begun: the held poll now polls its timer.
	release.send(()).unwrap();
	let result = poller.join().expect("the in-place poll panicked");
	assert!(matches!(result, Err(MoqError::Cancelled)), "{result:?}");
	shutdown.join().unwrap();
}

/// The broadcast's current catalog, read on the publish side.
fn published_catalog(
	broadcast: &MoqBroadcastProducer,
) -> moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra> {
	broadcast.with_state(|state| Ok(state.catalog.snapshot())).unwrap()
}

/// JSON tracks are advertised in the catalog (no `set_catalog_section` needed) and retired on finish.
#[tokio::test]
async fn json_tracks_are_advertised_in_the_catalog() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let snapshot = MoqJsonSnapshotProducer::new(
		&broadcast,
		&broadcast.publish_track("status".into(), None).unwrap(),
		MoqJsonSnapshotConfig { deltas: true },
	)
	.unwrap();
	let stream =
		MoqJsonStreamProducer::new(&broadcast, &broadcast.publish_track("events".into(), None).unwrap()).unwrap();

	let catalog = published_catalog(&broadcast);
	let entry = catalog.json.tracks.get("status").expect("snapshot track advertised");
	assert_eq!(entry.mode, hang::catalog::Mode::Snapshot);
	let entry = catalog.json.tracks.get("events").expect("stream track advertised");
	assert_eq!(entry.mode, hang::catalog::Mode::Stream);

	snapshot.finish().unwrap();
	let catalog = published_catalog(&broadcast);
	assert!(
		!catalog.json.tracks.contains_key("status"),
		"finished track still advertised"
	);
	assert!(catalog.json.tracks.contains_key("events"));
	stream.finish().unwrap();
	assert!(published_catalog(&broadcast).json.tracks.is_empty());
}

/// A second data track under a name the catalog already carries is refused, leaving the first.
///
/// The broadcast refuses a duplicate track itself, so the collision comes from a track created on
/// another broadcast. The refused track's handle stays open for the caller.
#[tokio::test]
async fn data_track_names_cannot_collide() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let first = MoqJsonSnapshotProducer::new(
		&broadcast,
		&broadcast.publish_track("state".into(), None).unwrap(),
		MoqJsonSnapshotConfig { deltas: false },
	)
	.unwrap();
	let other = MoqBroadcastProducer::new().unwrap();
	let track = other.publish_track("state".into(), None).unwrap();
	assert!(
		MoqJsonStreamProducer::new(&broadcast, &track).is_err(),
		"a duplicate data track name should fail"
	);
	assert_eq!(track.demand().unwrap().name(), "state");
	assert_eq!(
		published_catalog(&broadcast)
			.json
			.tracks
			.get("state")
			.map(|e| e.mode.clone()),
		Some(hang::catalog::Mode::Snapshot)
	);
	first.finish().unwrap();
}

/// In-memory sessions whose protocol and origin drivers use the paused test clock.
async fn memory_shutdown_pair() -> (Arc<MoqSession>, Arc<MoqSession>) {
	fn origin() -> crate::origin::Origin {
		let mut config = moq_net::origin::Config::default();
		config.pool = moq_net::cache::Pool::unbounded();
		let (inner, driver) = moq_net::origin::Producer::new(config);
		tokio::spawn(moq_net::time::run(driver));
		crate::origin::Origin {
			inner,
			driver: Default::default(),
		}
	}
	let a = origin();
	let b = origin();
	let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
	let mut transport = moq_net::transport::memory::Config::default();
	transport.protocol = Some(version.alpn().into());
	transport.stream_buffer = 1024;
	let (client, server) = moq_net::transport::memory::pair(transport);
	let connect = async {
		let (session, driver) = moq_net::Client::new()
			.with_origin(a.inner.clone())
			.with_versions(version.into())
			.connect(moq_clock::Instant::now(), client)
			.await
			.unwrap();
		tokio::spawn(moq_net::time::run(driver));
		Arc::new(MoqSession::accepted(session, a.clone(), a))
	};
	let accept = async {
		let (session, driver) = moq_net::Server::new()
			.with_publisher(&b.inner)
			.with_subscriber(b.inner.clone())
			.with_versions(version.into())
			.accept(moq_clock::Instant::now(), server)
			.await
			.unwrap();
		tokio::spawn(moq_net::time::run(driver));
		Arc::new(MoqSession::accepted(session, b.clone(), b))
	};
	tokio::join!(connect, accept)
}

#[tokio::test(start_paused = true)]
async fn shutdown_without_waiter_survives_session_disposal() {
	let (publisher, subscriber) = memory_shutdown_pair().await;
	let broadcast = create_announced(&publisher.publish(), "tail");
	let track = broadcast.publish_track("data".into(), Some(timed_info())).unwrap();
	let remote = await_announced(&subscriber.consume(), "tail").await;
	let reader = remote.subscribe_track("data".into(), None).await.unwrap();
	shutdown_ready(&track, &reader).await;
	let group = track.append_group().unwrap();
	group
		.write_frame(MoqFrame {
			timestamp_us: Some(1),
			payload: vec![42; 64 * 1024],
		})
		.unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	// The foreign caller neither polls the result nor retains the session handle.
	publisher.shutdown();
	drop(publisher);
	let frame = reader.read_frame().await.expect("finished tail").expect("last frame");
	assert_eq!(frame.payload, vec![42; 64 * 1024]);
	assert!(reader.read_frame().await.expect("clean end").is_none());
	subscriber.cancel(0);
}

#[tokio::test(start_paused = true)]
async fn shutdown_failure_survives_a_discarded_waiter() {
	let (publisher, subscriber) = memory_shutdown_pair().await;
	let broadcast = create_announced(&publisher.publish(), "live");
	let track = broadcast.publish_track("data".into(), Some(timed_info())).unwrap();
	let remote = await_announced(&subscriber.consume(), "live").await;
	let reader = remote.subscribe_track("data".into(), None).await.unwrap();
	shutdown_ready(&track, &reader).await;
	publisher.shutdown();
	publisher.shutdown();
	let mut waiter = Box::pin(publisher.closed());
	assert!(
		waiter
			.as_mut()
			.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
			.is_pending()
	);
	drop(waiter);
	for _ in 0..2 {
		let error = publisher.closed().await.expect_err("unfinished track must time out");
		assert!(matches!(error, MoqError::Protocol { details } if details.kind == MoqProtocolKind::DeliveryTimeout));
	}
	subscriber.cancel(0);
}

/// Real sockets exercise the FFI runtime and both accepted/client session handles.
/// Only lite-07 acknowledges the received tail; default lite-06 closes after transport ACKs.
async fn shutdown_pair() -> (Arc<MoqServer>, Arc<MoqSession>, Arc<MoqSession>) {
	session_pair("moq-lite-07-wip").await
}

/// A server, a client session connected to it on `version`, and the session it accepted.
async fn session_pair(version: &str) -> (Arc<MoqServer>, Arc<MoqSession>, Arc<MoqSession>) {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		versions: vec![version.into()],
		..Default::default()
	})
	.unwrap();
	let addr = server.listen().await.unwrap();
	let accepting = server.clone();
	let accepted = tokio::spawn(async move {
		accepting
			.accept()
			.await
			.unwrap()
			.unwrap()
			.accept(None, None)
			.await
			.unwrap()
	});
	let client = MoqClient::new(MoqClientConfig {
		tls: insecure_tls(),
		versions: vec![version.into()],
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();
	let connected = client.connect(format!("https://{addr}")).await.unwrap();
	(server, connected, accepted.await.unwrap())
}

/// TRACK_INFO holds demand before SUBSCRIBE; receiving a frame proves live delivery started.
async fn shutdown_ready(track: &MoqTrackProducer, reader: &MoqTrackConsumer) {
	let group = track.append_group().unwrap();
	group
		.write_frame(MoqFrame {
			timestamp_us: Some(0),
			payload: b"ready".to_vec(),
		})
		.unwrap();
	group.finish().unwrap();
	let ready = tokio::time::timeout(TIMEOUT, reader.read_frame())
		.await
		.expect("subscription ready")
		.expect("priming read")
		.expect("priming frame");
	assert_eq!(ready.payload, b"ready");
}

#[tokio::test]
async fn shutdown_delivers_the_finished_track() {
	for client_publishes in [false, true] {
		let (server, client, accepted) = tokio::time::timeout(TIMEOUT, shutdown_pair()).await.expect("pair");
		let (publisher, subscriber) = if client_publishes {
			(&client, &accepted)
		} else {
			(&accepted, &client)
		};
		let broadcast = create_announced(&publisher.publish(), "tail");
		let track = broadcast.publish_track("data".into(), Some(timed_info())).unwrap();
		let remote = await_announced(&subscriber.consume(), "tail").await;
		let reader = tokio::time::timeout(TIMEOUT, remote.subscribe_track("data".into(), None))
			.await
			.expect("subscribe")
			.unwrap();
		shutdown_ready(&track, &reader).await;
		let receiving = tokio::spawn(async move {
			let frame = reader.read_frame().await?;
			let end = reader.read_frame().await?;
			Ok::<_, MoqError>((frame, end))
		});
		let closing = publisher.clone();
		tokio::time::timeout(
			TIMEOUT,
			crate::ffi::detached(async move {
				let group = track.append_group()?;
				group.write_frame(MoqFrame {
					timestamp_us: Some(0),
					payload: b"last".to_vec(),
				})?;
				group.finish()?;
				track.finish()?;
				closing.shutdown();
				closing.closed().await?;
				Ok::<(), MoqError>(())
			}),
		)
		.await
		.expect("shutdown")
		.unwrap();
		let (frame, end) = tokio::time::timeout(TIMEOUT, receiving)
			.await
			.expect("read")
			.unwrap()
			.unwrap_or_else(|err| panic!("client_publishes={client_publishes}: tail/end: {err:?}"));
		assert_eq!(frame.unwrap().payload, b"last");
		assert!(end.is_none());
		client.cancel(0);
		accepted.cancel(0);
		server.cancel();
	}
}

#[tokio::test]
async fn shutdown_times_out_on_an_unfinished_track() {
	for client_publishes in [false, true] {
		let (server, client, accepted) = tokio::time::timeout(TIMEOUT, shutdown_pair()).await.expect("pair");
		let (publisher, subscriber) = if client_publishes {
			(&client, &accepted)
		} else {
			(&accepted, &client)
		};
		let broadcast = create_announced(&publisher.publish(), "live");
		let track = broadcast.publish_track("data".into(), Some(timed_info())).unwrap();
		let remote = await_announced(&subscriber.consume(), "live").await;
		let reader = tokio::time::timeout(TIMEOUT, remote.subscribe_track("data".into(), None))
			.await
			.expect("subscribe")
			.unwrap();
		shutdown_ready(&track, &reader).await;
		let receiving = tokio::spawn(async move { reader.read_frame().await });
		let start = moq_clock::Instant::now();
		let mut closed = Box::pin(publisher.closed());
		assert!(
			closed
				.as_mut()
				.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
				.is_pending()
		);
		publisher.shutdown();
		let err = tokio::time::timeout(TIMEOUT, closed)
			.await
			.expect("shutdown")
			.expect_err("unfinished track must time out");
		// The FFI runtime owns real sockets and cannot share a paused test clock.
		// A lower bound still rejects an immediate bail without depending on scheduler latency.
		assert!(
			start.elapsed() >= Duration::from_secs(1),
			"gave up draining after {:?}",
			start.elapsed()
		);
		assert!(
			matches!(err, MoqError::Protocol { details } if details.kind == crate::error::MoqProtocolKind::DeliveryTimeout)
		);
		let _ = tokio::time::timeout(TIMEOUT, receiving).await.unwrap().unwrap();
		client.cancel(0);
		accepted.cancel(0);
		server.cancel();
	}
}

/// An ascending subscription reaching back over a cached backlog reads it oldest first,
/// and the order and ranges reach moq-net as asked.
#[tokio::test]
async fn ascending_subscription_reads_a_cached_backlog_oldest_first() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), Some(timed_info())).unwrap();
	for timestamp_us in [0, 1_000, 2_000] {
		let group = track.append_group().unwrap();
		group
			.write_frame(MoqFrame {
				payload: b"x".to_vec(),
				timestamp_us: Some(timestamp_us),
			})
			.unwrap();
		group.finish().unwrap();
	}

	let subscription = MoqSubscription {
		priority: 0,
		max_delay: Some(Duration::from_secs(10)),
		ranges: Some(vec![MoqRange {
			start: MoqAnchor::Relative { groups: 2 },
			end: None,
		}]),
		order: Some(MoqOrder::Ascending),
	};
	let converted = moq_net::track::Subscription::try_from(subscription.clone()).unwrap();
	assert_eq!(converted.order, moq_net::track::Order::Ascending);
	assert_eq!(
		converted.ranges,
		vec![moq_net::track::Range {
			start: moq_net::track::Anchor::Relative(2),
			end: None,
		}]
	);

	let consumer = track.consume(Some(subscription)).unwrap();
	let mut sequences = Vec::new();
	for _ in 0..3 {
		let group = tokio::time::timeout(TIMEOUT, consumer.recv_group())
			.await
			.expect("timed out waiting for a cached group")
			.unwrap()
			.expect("expected a group");
		sequences.push(group.sequence());
	}
	assert_eq!(sequences, vec![0, 1, 2]);
}

/// Wait until `session` grants `want`, following its changes.
async fn await_grant(session: &MoqSession, want: crate::auth::MoqGrant) {
	// A fresh consumer starts from the grant as it is now, so `changed` follows from there.
	let grants = session.grants();
	let mut current = grants.grant();
	tokio::time::timeout(TIMEOUT, async {
		while current.as_ref() != Some(&want) {
			current = grants.changed().await.expect("grant watch ended");
		}
	})
	.await
	.unwrap_or_else(|_| panic!("timed out waiting for {want:?}, last {current:?}"));
}

fn grant(publish: &[&str], subscribe: &[&str]) -> crate::auth::MoqGrant {
	let patterns = |patterns: &[&str]| patterns.iter().map(|p| p.to_string()).collect();
	crate::auth::MoqGrant {
		publish: patterns(publish),
		subscribe: patterns(subscribe),
		expires: None,
	}
}

/// The peer's answer to the tokens set last.
async fn answered(session: &MoqSession) -> Result<(), MoqError> {
	let tokens = session.tokens().expect("a client session has tokens");
	tokio::time::timeout(TIMEOUT, tokens.answered())
		.await
		.expect("the tokens were never answered")
}

/// Both sides of a session read the grant their peer gave them, on lite-07 and on moq-transport
/// with the Auth extension, and a peer that verifies no tokens of its own refuses another one as
/// unsupported.
#[tokio::test]
async fn auth_reads_the_peer_grant() {
	for version in ["moq-lite-07-wip", "moq-transport-18"] {
		auth_reads_the_peer_grant_on(version).await;
	}
}

async fn auth_reads_the_peer_grant_on(version: &str) {
	let (server, client, accepted) = tokio::time::timeout(TIMEOUT, session_pair(version))
		.await
		.expect("pair");

	// Each side's origins span the root, so its peer grants it everything.
	await_grant(&client, grant(&["**"], &["**"])).await;
	await_grant(&accepted, grant(&["**"], &["**"])).await;
	assert!(accepted.tokens().is_none(), "an accepted session presents no tokens");

	// The live session was dialed without a credential, so this one rides AUTH.
	let tokens = client.tokens().unwrap();
	tokens.set(vec!["extra".into()]).unwrap();
	let err = answered(&client)
		.await
		.expect_err("the default acceptor refuses a token");
	assert!(matches!(err, MoqError::Unsupported), "refusal: {err:?}");
	assert_eq!(
		client.grants().grant(),
		Some(grant(&["**"], &["**"])),
		"a refusal leaves the union"
	);

	client.cancel(0);
	accepted.cancel(0);
	server.cancel();
}

/// A live session without AUTH (moq-lite before 07) has nothing to carry a new token on, so the
/// answer is that the session cannot carry it.
#[tokio::test]
async fn auth_refuses_a_token_nothing_carries() {
	let (server, client, accepted) = tokio::time::timeout(TIMEOUT, session_pair("moq-lite-06"))
		.await
		.expect("pair");
	client.tokens().unwrap().set(vec!["extra".into()]).unwrap();
	let err = answered(&client).await.expect_err("a token nothing carries is refused");
	assert!(matches!(err, MoqError::Unsupported), "refusal: {err:?}");
	for session in [&client, &accepted] {
		assert_eq!(session.grants().grant(), None, "no AUTH, no grant");
	}

	client.cancel(0);
	accepted.cancel(0);
	server.cancel();
}

/// A replacement token joins the union once answered, leaves it when replaced, and a
/// refusal carries the peer's code. Each grant consumer follows the changes on its own.
#[tokio::test]
async fn auth_tokens_join_and_leave_the_union() {
	let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
	config.version = vec![version];
	let mut listener = config.init(Default::default()).unwrap().listen().await.unwrap();
	let addr = listener.tcp_local_addr().unwrap();

	let publish = |pattern: &str| moq_net::auth::Grant {
		publish: pattern.parse::<moq_net::Pattern>().unwrap().into(),
		..Default::default()
	};
	let serving = tokio::spawn(async move {
		let mut request = listener.accept().await.expect("accept");
		let mut requests = request.requests();
		let session = request.ok().await.unwrap();
		let mut issued = Vec::new();
		while let Some(request) = requests.next().await {
			match request.token().as_ref() {
				b"" => issued.push(request.accept(publish("setup/**"))),
				b"extra" => issued.push(request.accept(publish("extra/**"))),
				_ => request.reject(moq_net::SessionError::Unauthorized, "unknown token"),
			}
		}
		drop((session, issued));
	});

	let client = MoqClient::new(MoqClientConfig {
		versions: vec!["moq-lite-07-wip".into()],
		bind: Some("127.0.0.1:0".into()),
		once: true,
		..Default::default()
	})
	.unwrap();
	let session = tokio::time::timeout(TIMEOUT, client.connect(format!("tcp://{addr}")))
		.await
		.expect("connect timed out")
		.unwrap();
	await_grant(&session, grant(&["setup/**"], &[])).await;
	// A second consumer watches on its own and starts from the grant it was made with.
	let watcher = session.grants();
	// The session was dialed without a credential, so the empty set is already answered.
	answered(&session).await.expect("no tokens to refuse");

	let tokens = session.tokens().unwrap();
	tokens.set(vec!["extra".into()]).unwrap();
	answered(&session).await.expect("the extra token was refused");
	// Patterns are kept sorted.
	await_grant(&session, grant(&["extra/**", "setup/**"], &[])).await;
	let seen = tokio::time::timeout(TIMEOUT, watcher.changed())
		.await
		.expect("the second consumer sees the change")
		.unwrap();
	assert_eq!(seen, Some(grant(&["extra/**", "setup/**"], &[])));

	// A refused replacement still withdraws the token it replaces.
	tokens.set(vec!["bogus".into()]).unwrap();
	let refused = answered(&session).await.expect_err("refused");
	assert_protocol(
		&refused,
		crate::error::MoqErrorScope::Session,
		MoqProtocolKind::Unauthorized,
	);
	await_grant(&session, grant(&["setup/**"], &[])).await;

	session.cancel(0);
	tokio::time::timeout(TIMEOUT, serving)
		.await
		.expect("server ended")
		.unwrap();
}

/// A time anchor starts at the group containing it, and an untimed track refuses one.
#[tokio::test]
async fn time_anchors_start_at_the_containing_group() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), Some(timed_info())).unwrap();
	for timestamp_us in [0, 1_000, 2_000] {
		let group = track.append_group().unwrap();
		group
			.write_frame(MoqFrame {
				payload: b"x".to_vec(),
				timestamp_us: Some(timestamp_us),
			})
			.unwrap();
		group.finish().unwrap();
	}

	let since = |timestamp_us| MoqSubscription {
		priority: 0,
		max_delay: Some(Duration::from_secs(10)),
		ranges: Some(vec![MoqRange {
			start: MoqAnchor::Time { timestamp_us },
			end: None,
		}]),
		order: Some(MoqOrder::Ascending),
	};
	let consumer = track.consume(Some(since(1_500))).unwrap();
	let mut sequences = Vec::new();
	for _ in 0..2 {
		let group = tokio::time::timeout(TIMEOUT, consumer.recv_group())
			.await
			.expect("timed out waiting for a cached group")
			.unwrap()
			.expect("expected a group");
		sequences.push(group.sequence());
	}
	assert_eq!(sequences, vec![1, 2]);

	let untimed = broadcast
		.publish_track(
			"status".into(),
			Some(MoqTrackInfo {
				timescale: None,
				..timed_info()
			}),
		)
		.unwrap();
	let consumer = untimed.consume(None).unwrap();
	assert!(matches!(
		consumer.update(since(0)),
		Err(MoqError::Protocol {
			details: MoqProtocolError {
				kind: MoqProtocolKind::TimestampMismatch,
				..
			}
		})
	));
}

// Frame-only controls keep checking payloads independently of the breaks between frames.
async fn next_encoded_frame(consumer: &MoqMediaTrackConsumer) -> Result<Option<MoqMediaFrame>, MoqError> {
	loop {
		match consumer.next().await? {
			Some(MoqMediaRead::Frame { frame }) => return Ok(Some(frame)),
			Some(MoqMediaRead::Gap { .. } | MoqMediaRead::End { .. }) => continue,
			None => return Ok(None),
		}
	}
}
