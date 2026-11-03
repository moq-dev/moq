//! The pre-encoded FFI publisher follows real BBR3 grants through a closing and reopening uplink.

use super::*;
use crate::bandwidth::{MoqRateControl, MoqRatePolicy, MoqRateTarget};

const LOW: u64 = 250_000;
const HIGH: u64 = 1_000_000;
const BOUNDARY: u64 = (HIGH + 2 * LOW) / 3;
const NARROW: Duration = Duration::from_secs(8);
const BELOW_LOWEST: Duration = Duration::from_secs(20);
const RECOVER: Duration = Duration::from_secs(32);
const END: Duration = Duration::from_secs(110);
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 30);

#[tokio::test]
#[ignore = "real sockets and BBR3 capacity cycles; run nightly by `just rs rung-disable`"]
async fn shaped_uplink_disables_and_resumes() {
	let server = MoqServer::new(MoqServerConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: localhost_tls(),
		..Default::default()
	})
	.unwrap();
	let addr = server.listen().await.unwrap().parse().unwrap();
	let mut setup = moq_shaper::Preset::load("cellular-step")
		.unwrap()
		.setup("127.0.0.1:0".parse().unwrap(), addr);
	// Leave a full BBR3 max-bandwidth window at each bottleneck. The second drop
	// takes even the lowest rendition below its ceiling; only probing can recover.
	setup.config.up.jitter = Duration::ZERO;
	setup.up.steps = [(NARROW, 500_000), (BELOW_LOWEST, 100_000), (RECOVER, 2_000_000)]
		.into_iter()
		.map(|(at, bits_per_second)| moq_shaper::Step {
			at,
			bits_per_second: Some(bits_per_second),
			..Default::default()
		})
		.collect();
	let start = kio::clock::now();
	let shaper = moq_shaper::Shaper::bind(setup).await.unwrap();
	let client = MoqClient::new(MoqClientConfig {
		bind: Some("127.0.0.1:0".into()),
		tls: insecure_tls(),
		once: true,
		quic: MoqQuicConfig {
			congestion_control: Some(MoqCongestionControl::Delay),
			..Default::default()
		},
		websocket: MoqWebSocketConfig {
			enabled: Some(false),
			..Default::default()
		},
		..Default::default()
	})
	.unwrap();
	let (publisher, receiver) = tokio::join!(client.connect(format!("https://{}/", shaper.addr())), async {
		server.accept().await.unwrap().unwrap().accept(None, None).await
	});
	let publisher = publisher.unwrap();
	let receiver = receiver.unwrap();
	let broadcast = create_announced(&publisher.publish(), "ladder");
	let allocator = publisher.allocator();
	let mut tracks = Vec::new();
	let mut reservations = Vec::new();
	let mut controls = Vec::new();
	for (name, max, lower) in [("low", LOW, None), ("high", HIGH, Some(LOW))] {
		let track = MoqMediaTrackProducer::video(
			&broadcast,
			MoqMediaTarget::Named {
				name: Some(name.into()),
			},
			MoqVideoInit {
				hint: Some(MoqVideoHint {
					bitrate: Some(max),
					..Default::default()
				}),
				lower: tracks.first().cloned(),
				..video_init(MoqVideoFormat::Avc3, h264_init())
			},
		)
		.unwrap();
		reservations.push(allocator.reserve(&track.demand().unwrap(), max));
		controls.push(MoqRateControl::new(MoqRatePolicy {
			max_bps: max,
			lower_bps: lower,
		}));
		tracks.push(track);
	}
	let remote = await_announced(&receiver.consume(), "ladder").await;
	let catalog = MoqMediaCatalogConsumer::subscribe(&remote).await.unwrap();
	let first = catalog.next().await.unwrap().expect("catalog ended");
	let low = MoqMediaTrackConsumer::subscribe(
		&remote,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Video {
				id: "low".into(),
				config: first.video["low"].clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();
	let high = MoqMediaTrackConsumer::subscribe(
		&remote,
		MoqMediaTrackConfig {
			rendition: MoqRendition::Video {
				id: "high".into(),
				config: first.video["high"].clone(),
			},
			subscription: None,
		},
	)
	.await
	.unwrap();
	let mut ticker = kio::clock::interval(FRAME);
	ticker.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Skip);
	let mut remote_enabled = Some(first.video["high"].enabled);
	let mut initial = false;
	let mut disabled = false;
	let mut below_lowest = false;
	let mut resumed = false;
	let mut received = [0u64; 2];
	let mut sent = [0u64; 2];
	let mut keyframe = [true; 2];
	let mut resumed_frame = false;
	let mut stopped_at = None;
	let mut resumed_at = None;
	let mut previous_second = None;
	loop {
		tokio::select! {
			_ = kio::clock::sleep_until(start + END) => break,
			_ = ticker.tick() => {
				let elapsed = start.elapsed();
				let timestamp = elapsed.as_micros() as u64;
				for (i, ((track, reservation), control)) in tracks.iter().zip(&reservations).zip(&controls).enumerate() {
					let was_disabled = control.target() == MoqRateTarget::Disabled;
					if let Some(target) = control.update(reservation) {
						println!("{elapsed:?} rung {i}: {target:?}, grant {:?}", reservation.rate());
						let enabled = matches!(target, MoqRateTarget::Enabled { .. });
						track.set_enabled(enabled).unwrap();
						if !enabled {
							assert_eq!(i, 1, "the lowest rung must never disable");
							track.discontinuity().unwrap();
							keyframe[i] = true;
							if elapsed >= NARROW && stopped_at.is_none() {
								stopped_at = Some(timestamp);
							}
						} else if i == 1 && was_disabled && stopped_at.is_some() {
							assert!(elapsed >= RECOVER, "resumed before the path reopened");
							assert_eq!(target, MoqRateTarget::Enabled { bitrate_bps: BOUNDARY },
								"resume must start at the band boundary before ramping");
							resumed_at.get_or_insert(timestamp);
						}
					}
					if let MoqRateTarget::Enabled { bitrate_bps } = control.target() {
						// Two-second GOPs, with an IDR on resume. Pad access units to the
						// target budget, keeping codec rate accuracy out of this transport test.
						let idr = keyframe[i] || sent[i].is_multiple_of(60);
						let mut payload = if idr { h264_init() } else { Vec::new() };
						payload.extend_from_slice(&[0, 0, 0, 1, if idr { 0x65 } else { 0x41 }, 0x88, 0x84, 0x21]);
						payload.resize((bitrate_bps / 8 / 30) as usize, 0xff);
						track.write_frame(MoqFrame { payload, timestamp: Some(us(timestamp)) }).unwrap();
						keyframe[i] = false;
						sent[i] += 1;
					}
				}
				if elapsed < NARROW && remote_enabled == Some(true) && received.iter().all(|count| *count > 0) {
					initial = true;
				}
				if elapsed >= NARROW && elapsed < BELOW_LOWEST && remote_enabled == Some(false) {
					disabled = true;
				}
				if elapsed >= BELOW_LOWEST && elapsed < RECOVER && reservations[0].rate().is_some_and(|rate| rate < LOW) {
					assert_eq!(controls[1].target(), MoqRateTarget::Disabled);
					assert_eq!(remote_enabled, Some(false));
					below_lowest = true;
				}
				if elapsed >= RECOVER && resumed_at.is_some() && remote_enabled == Some(true) {
					resumed = true;
				}
				if previous_second != Some(elapsed.as_secs()) {
					previous_second = Some(elapsed.as_secs());
					println!("{elapsed:?}: estimate {:?}, grants {:?}, received {received:?}, remote {remote_enabled:?}",
						publisher.stats().estimated_send_rate_bps,
						reservations.iter().map(|r| r.rate()).collect::<Vec<_>>());
				}
			},
			update = catalog.next() => {
				let update = update.unwrap().expect("catalog ended");
				assert!(update.video["low"].enabled);
				assert_eq!(update.video["low"].bitrate, Some(LOW));
				assert_eq!(update.video["high"].bitrate, Some(HIGH));
				remote_enabled = Some(update.video["high"].enabled);
				println!("{:?}: remote enabled {remote_enabled:?}", start.elapsed());
			},
			frame = low.next() => {
				if let MoqMediaRead::Frame { .. } = frame.unwrap().expect("lowest track ended") {
					received[0] += 1;
				}
			},
			frame = high.next() => {
				let read = frame.unwrap().expect("top track ended");
				// Frames queued before disabling may still arrive, but none may have
				// been produced in the disabled interval. Decode the container so its
				// discontinuity marker isn't mistaken for media.
				if let MoqMediaRead::Frame { frame } = read {
					let timestamp = micros(frame.timestamp);
					if let Some(stopped) = stopped_at {
						assert!(timestamp < stopped || resumed_at.is_some_and(|at| timestamp >= at),
							"media was produced while disabled: {timestamp}");
					}
					if !resumed_frame && resumed_at.is_some_and(|at| timestamp >= at) {
						assert!(frame.keyframe, "the resumed rendition must start with a keyframe");
						resumed_frame = true;
					}
					received[1] += 1;
				}
			},
		}
		if initial && disabled && below_lowest && resumed && resumed_frame {
			break;
		}
	}
	let stats = shaper.verify().unwrap();
	println!("shaper: {stats}");
	assert!(stats.up.overflowed > 0, "the finite queue never overflowed");
	assert!(
		initial,
		"both renditions never delivered before the bottleneck narrowed"
	);
	assert!(
		disabled,
		"the top rendition never advertised disabled on the narrow uplink"
	);
	assert!(
		below_lowest,
		"the grant never fell below the lowest rung while the top stayed disabled"
	);
	assert!(
		resumed && resumed_frame,
		"the top rendition never advertised and delivered its resume"
	);
	for track in tracks {
		track.finish().unwrap();
	}
	broadcast.close().unwrap();
	publisher.cancel(0);
	receiver.cancel(0);
	client.cancel();
	server.cancel();
}
