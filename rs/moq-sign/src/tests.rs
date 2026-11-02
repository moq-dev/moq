use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::scope::Scope;
use crate::{Error, PublicKey, SecretKey, broadcast, group, track};

const VECTORS: &str = include_str!("../../../drafts/moq-sign-00.json");

fn vectors() -> Value {
	serde_json::from_str(VECTORS).unwrap()
}

fn hex(value: &Value) -> Vec<u8> {
	hex::decode(value.as_str().unwrap()).unwrap()
}

fn seed(value: &Value) -> SecretKey {
	SecretKey::from_seed(Zeroizing::new(hex(value).try_into().unwrap()))
}

fn public(value: &Value) -> PublicKey {
	value.as_str().unwrap().parse().unwrap()
}

/// A vector row's identity: its scope, group, frame, and timestamp (timescale 0 when untimed).
fn identity(row: &Value) -> (Arc<Scope>, u64, u64, Option<moq_net::Timestamp>) {
	let path = moq_net::Path::new(row["path"].as_str().unwrap()).unwrap();
	let epoch = row["epoch"].as_str().unwrap().parse().unwrap();
	let scope = Scope::new(&path, &epoch, row["track"].as_str().unwrap());
	let timestamp = match row["timescale"].as_u64().unwrap() {
		0 => None,
		scale => Some(moq_net::Timestamp::from_scale(row["timestamp"].as_u64().unwrap(), scale).unwrap()),
	};
	(
		scope,
		row["group"].as_u64().unwrap(),
		row["frame"].as_u64().unwrap(),
		timestamp,
	)
}

#[test]
fn vector_keys() {
	for row in vectors()["keys"].as_array().unwrap() {
		let id = &row["id"];
		let endpoint = public(&row["endpoint"]);
		assert_eq!(
			endpoint,
			PublicKey::from_bytes(hex(&row["public"]).try_into().unwrap()),
			"{id}"
		);
		assert_eq!(endpoint.to_string(), row["endpoint"].as_str().unwrap(), "{id}");
		if !row["seed"].is_null() {
			assert_eq!(seed(&row["seed"]).public(), endpoint, "{id}");
		}
	}
}

#[test]
fn vector_frames() {
	for row in vectors()["frames"].as_array().unwrap() {
		let id = &row["id"];
		let (scope, group, frame, timestamp) = identity(row);
		let payload = hex(&row["payload"]);
		assert_eq!(
			scope.message(group, frame, timestamp, &payload),
			hex(&row["message"]),
			"{id}"
		);
		let body = scope.sign(&seed(&row["seed"]), group, frame, timestamp, &payload);
		assert_eq!(body.as_ref(), hex(&row["body"]), "{id}");
		let opened = scope
			.verify(&public(&row["endpoint"]), group, frame, timestamp, body)
			.unwrap();
		assert_eq!(opened.as_ref(), payload, "{id}");
	}
}

#[test]
fn vector_negative() {
	for row in vectors()["negative"].as_array().unwrap() {
		let (scope, group, frame, timestamp) = identity(row);
		let body = Bytes::from(hex(&row["body"]));
		let result = scope.verify(&public(&row["endpoint"]), group, frame, timestamp, body);
		assert_eq!(row["error"], "signature");
		assert!(matches!(result, Err(Error::Signature)), "{}", row["id"]);
	}
}

#[test]
fn vector_invalid_ids() {
	for row in vectors()["invalid_ids"].as_array().unwrap() {
		assert!(
			row["text"].as_str().unwrap().parse::<PublicKey>().is_err(),
			"{}",
			row["id"]
		);
	}
}

#[test]
fn key_file_persists_and_never_rotates() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("endpoint.key");

	let created = SecretKey::load_or_generate(&path).unwrap();
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
	}
	let loaded = SecretKey::load_or_generate(&path).unwrap();
	assert_eq!(created.public(), loaded.public());
	// The seed as 52 z-base-32 characters and a newline.
	let text = std::fs::read_to_string(&path).unwrap();
	let encoded = text.strip_suffix('\n').unwrap();
	assert_eq!(encoded.len(), 52);
	assert!(
		encoded
			.bytes()
			.all(|b| b"ybndrfg8ejkmcpqxot1uwisza345h769".contains(&b)),
		"{text}"
	);

	// A damaged file is an error, never a reason to mint a new identity.
	std::fs::write(&path, "not a key\n").unwrap();
	let err = SecretKey::load_or_generate(&path).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
	assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a key\n");
}

#[cfg(unix)]
#[test]
fn key_file_readable_by_others_is_refused() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("endpoint.key");
	SecretKey::load_or_generate(&path).unwrap();
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
	let err = SecretKey::load_or_generate(&path).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn debug_redacts_the_seed() {
	let key = SecretKey::from_seed(Zeroizing::new([0x42; 32]));
	let debug = format!("{key:?}");
	assert!(!debug.contains(&"42".repeat(32)), "{debug}");
	assert!(debug.contains("<redacted>"), "{debug}");
	assert!(debug.contains(&key.public().to_string()));
}

/// Let the origin's driver serve what a call started.
async fn settle() {
	for _ in 0..10 {
		tokio::task::yield_now().await;
	}
}

fn subscribe_all() -> moq_net::track::Subscription {
	moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(60))
}

/// A signed broadcast published at `path` on its own origin, announced under its epoch.
struct Published {
	origin: moq_net::origin::Producer,
	_publication: moq_net::origin::Publication,
	signed: broadcast::Producer,
	path: moq_net::Path,
}

fn publish(key: &SecretKey, path: &str) -> Published {
	let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
	tokio::spawn(moq_net::time::run(driver));
	let path = moq_net::Path::new(path).unwrap();
	let publication = origin.create_broadcast(&path).unwrap();
	let signed = broadcast::Producer::new(publication.broadcast().clone(), key.clone(), path.clone());
	publication
		.announce(moq_net::origin::Route::default().with_epoch(signed.epoch().clone()))
		.unwrap();
	Published {
		origin,
		_publication: publication,
		signed,
		path,
	}
}

impl Published {
	/// The broadcast as a subscriber resolves it, verifying against `key`.
	async fn consume(&self, key: PublicKey) -> broadcast::Consumer {
		settle().await;
		let inner = self.origin.consume().request(&self.path, None).await.unwrap();
		broadcast::Consumer::new(inner, key, self.path.clone()).unwrap()
	}

	/// A signed track over a net track the test can also write forged frames to.
	fn track(&self, key: &SecretKey, name: &str) -> (track::Producer, moq_net::track::Producer) {
		let net = self._publication.broadcast().create_track(name, None).unwrap();
		let scope = Scope::new(&self.path, self.signed.epoch(), name);
		(
			track::Producer::new(net.clone(), key.clone(), scope, Arc::default()),
			net,
		)
	}
}

async fn subscribe(consumer: &broadcast::Consumer, name: &str) -> track::Consumer {
	consumer.subscribe(name, subscribe_all()).unwrap().await.unwrap()
}

async fn recv_group(consumer: &mut track::Consumer) -> group::Consumer {
	settle().await;
	consumer.recv_group().await.unwrap().expect("a group")
}

async fn read(group: &mut group::Consumer) -> Result<Option<Bytes>, Error> {
	settle().await;
	Ok(group.read_frame().await?.map(|frame| frame.payload))
}

#[tokio::test(start_paused = true)]
async fn roundtrip_through_an_origin() {
	let key = SecretKey::generate();
	let published = publish(&key, "mesh/node/ssh");
	let video = published
		.signed
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	let consumer = published.consume(key.public()).await;
	assert_eq!(consumer.epoch(), published.signed.epoch());
	let mut track = subscribe(&consumer, "video").await;

	let mut group = video.append_group().unwrap();
	group
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"keyframe")
		.unwrap();
	// Signed at the track's scale, which the net group converts it to.
	group
		.write_frame(moq_net::Timestamp::from_micros(2_400).unwrap(), b"")
		.unwrap();
	group.finish().unwrap();

	let mut received = recv_group(&mut track).await;
	let first = received.read_frame().await.unwrap().unwrap();
	assert_eq!(first.payload, &b"keyframe"[..]);
	assert_eq!(first.timestamp, Some(moq_net::Timestamp::from_millis(1).unwrap()));
	let second = received.read_frame().await.unwrap().unwrap();
	assert_eq!(second.payload, &b""[..]);
	assert_eq!(second.timestamp, Some(moq_net::Timestamp::from_millis(2).unwrap()));
	assert_eq!(read(&mut received).await.unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn forged_frame_delivers_nothing_and_ends_only_its_group() {
	let key = SecretKey::generate();
	let published = publish(&key, "mesh/node/ssh");
	let (signed, net) = published.track(&key, "stream");
	let consumer = published.consume(key.public()).await;
	let mut track = subscribe(&consumer, "stream").await;

	// A relay's forgery: an unsigned frame the length of a signed one.
	let mut forged = net.append_group().unwrap();
	forged.write_frame(None, vec![0u8; 80]).unwrap();
	forged.write_frame(None, vec![0u8; 80]).unwrap();
	forged.finish().unwrap();

	let mut group = recv_group(&mut track).await;
	assert!(matches!(read(&mut group).await, Err(Error::Signature)));
	// The group stays ended, even though a later frame is buffered.
	assert!(matches!(read(&mut group).await, Err(Error::Signature)));

	let mut next = signed.create_group(1).unwrap();
	next.write_frame(None, b"after").unwrap();
	next.finish().unwrap();
	let mut group = recv_group(&mut track).await;
	assert_eq!(read(&mut group).await.unwrap().unwrap(), &b"after"[..]);
}

/// Copy the raw body of frame 0 of the next group out of `net`'s own subscription.
async fn raw_frame(net: &mut moq_net::track::Subscriber) -> Bytes {
	let mut group = net.recv_group().await.unwrap().unwrap();
	group.read_frame().await.unwrap().unwrap().payload
}

#[tokio::test(start_paused = true)]
async fn frame_replayed_from_another_group_or_track_is_refused() {
	let key = SecretKey::generate();
	let published = publish(&key, "mesh/node/ssh");
	let (signed, net) = published.track(&key, "a");
	let (_other, other_net) = published.track(&key, "b");
	let mut raw = net.subscribe(subscribe_all());
	let consumer = published.consume(key.public()).await;
	let mut a = subscribe(&consumer, "a").await;
	let mut b = subscribe(&consumer, "b").await;

	let mut group = signed.append_group().unwrap();
	group.write_frame(None, b"original").unwrap();
	group.finish().unwrap();
	let body = raw_frame(&mut raw).await;
	assert_eq!(
		read(&mut recv_group(&mut a).await).await.unwrap().unwrap(),
		&b"original"[..]
	);

	// The same bytes as group 1 of the same track.
	let mut replay = net.append_group().unwrap();
	replay.write_frame(None, body.clone()).unwrap();
	replay.finish().unwrap();
	assert!(matches!(
		read(&mut recv_group(&mut a).await).await,
		Err(Error::Signature)
	));

	// The same bytes at the same group and index of another track.
	let mut moved = other_net.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
	moved.write_frame(None, body.clone()).unwrap();
	moved.finish().unwrap();
	assert!(matches!(
		read(&mut recv_group(&mut b).await).await,
		Err(Error::Signature)
	));

	// The same bytes at frame 1 of a group at the original sequence's neighbor.
	let mut shifted = net.append_group().unwrap();
	shifted.write_frame(None, b"filler".as_slice()).unwrap();
	shifted.write_frame(None, body).unwrap();
	shifted.finish().unwrap();
	assert!(matches!(
		read(&mut recv_group(&mut a).await).await,
		Err(Error::Signature)
	));
}

#[tokio::test(start_paused = true)]
async fn broadcast_under_another_key_is_refused() {
	let impostor = SecretKey::generate();
	let victim = SecretKey::generate();
	// The impostor publishes at the victim's path, signing with its own key.
	let published = publish(&impostor, &format!("mesh/{}/ssh", victim.public()));
	let track = published.signed.create_track("stream", None).unwrap();
	let consumer = published.consume(victim.public()).await;
	let mut subscribed = subscribe(&consumer, "stream").await;

	let mut group = track.append_group().unwrap();
	group.write_frame(None, b"hello").unwrap();
	group.finish().unwrap();
	assert!(matches!(
		read(&mut recv_group(&mut subscribed).await).await,
		Err(Error::Signature)
	));
}

#[tokio::test(start_paused = true)]
async fn frame_from_an_earlier_instance_is_refused() {
	let key = SecretKey::generate();
	let old = publish(&key, "mesh/node/ssh");
	let (old_track, old_net) = old.track(&key, "stream");
	let mut raw = old_net.subscribe(subscribe_all());
	let mut group = old_track.append_group().unwrap();
	group.write_frame(None, b"stale").unwrap();
	group.finish().unwrap();
	let body = raw_frame(&mut raw).await;

	// A restart mints a new epoch, so the old instance's group 0 does not verify as the new one's.
	let new = publish(&key, "mesh/node/ssh");
	assert_ne!(new.signed.epoch(), old.signed.epoch());
	let (_new_track, new_net) = new.track(&key, "stream");
	let consumer = new.consume(key.public()).await;
	let mut subscribed = subscribe(&consumer, "stream").await;
	let mut replay = new_net.append_group().unwrap();
	replay.write_frame(None, body).unwrap();
	replay.finish().unwrap();
	assert!(matches!(
		read(&mut recv_group(&mut subscribed).await).await,
		Err(Error::Signature)
	));
}

#[test]
fn group_sequence_is_never_signed_twice() {
	let key = SecretKey::generate();
	let net = moq_net::broadcast::Info::new().produce();
	let signed = broadcast::Producer::new(net, key, moq_net::Path::new("mesh/node/ssh").unwrap());
	let track = signed.create_track("stream", None).unwrap();
	let mut group = track.append_group().unwrap();
	group.write_frame(None, b"first").unwrap();
	group.abort(moq_net::Error::Cancel).unwrap();

	// Net would serve the aborted sequence again, but frames of both incarnations would verify,
	// so a relay holding both could splice them into one group.
	assert!(matches!(
		track.create_group(0),
		Err(Error::Net(moq_net::Error::Duplicate))
	));
	assert_eq!(track.append_group().unwrap().sequence(), 1);
	assert!(matches!(
		track.create_group(1),
		Err(Error::Net(moq_net::Error::Duplicate))
	));
	assert_eq!(track.create_group(5).unwrap().sequence(), 5);
	assert!(matches!(
		track.create_group(3),
		Err(Error::Net(moq_net::Error::Duplicate))
	));

	// A replacement track continues past its predecessor's sequences under the same epoch.
	track.abort(moq_net::Error::Cancel).unwrap();
	let replacement = signed.create_track("stream", None).unwrap();
	assert!(matches!(
		replacement.create_group(0),
		Err(Error::Net(moq_net::Error::Duplicate))
	));
	assert_eq!(replacement.append_group().unwrap().sequence(), 6);
}

#[tokio::test(start_paused = true)]
async fn group_resumed_above_frame_zero_verifies_at_its_index() {
	let key = SecretKey::generate();
	let path = moq_net::Path::new("mesh/node/ssh").unwrap();
	let epoch = moq_net::Epoch::mint();
	let scope = Scope::new(&path, &epoch, "stream");
	let broadcast = moq_net::broadcast::Info::new().produce();
	let track = broadcast.create_track("stream", None).unwrap();
	let net = track.append_group().unwrap();
	let raw = net.consume();
	let mut signed = group::Producer::new(net, key.clone(), Arc::clone(&scope));
	signed.write_frame(None, b"zero").unwrap();
	signed.write_frame(None, b"one").unwrap();
	signed.finish().unwrap();

	let mut resumed = group::Consumer::new(raw.with_range(1..), key.public(), scope);
	assert_eq!(read(&mut resumed).await.unwrap().unwrap(), &b"one"[..]);
	assert_eq!(read(&mut resumed).await.unwrap(), None);
}

#[test]
fn consumer_without_an_epoch_is_refused() {
	let inner = moq_net::broadcast::Info::new().produce().consume();
	let path = moq_net::Path::new("mesh/node/ssh").unwrap();
	let result = broadcast::Consumer::new(inner, SecretKey::generate().public(), path);
	assert!(matches!(result, Err(Error::Epoch)));
}

/// Runs `test/interop/sign.ts` with a JSON argument, returning its JSON stdout.
fn bun(input: Value) -> Value {
	let output = std::process::Command::new("bun")
		.arg(format!("{}/../../test/interop/sign.ts", env!("CARGO_MANIFEST_DIR")))
		.arg(input.to_string())
		.output()
		.expect("Bun must be installed for the interop tests");
	assert!(
		output.status.success(),
		"sign.ts failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	serde_json::from_slice(&output.stdout).unwrap()
}

/// Rust-signed groups verify in JS and a relocated or retimed one does not; JS-signed groups
/// verify in Rust, timestamps included.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn sign_interop() {
	let mut seed = Zeroizing::new([0u8; 32]);
	aws_lc_rs::rand::fill(seed.as_mut()).unwrap();
	let key = SecretKey::from_seed(seed.clone());
	let path = moq_net::Path::new(&format!("mesh/{}/agent", key.public())).unwrap();
	let epoch = moq_net::Epoch::mint();

	let payloads: [&[u8]; 3] = [b"zero", b"", &[0xff; 300]];
	let expected: Vec<String> = payloads.iter().map(hex::encode).collect();
	let millis = |value: u64| moq_net::Timestamp::from_millis(value).unwrap();
	// A group's frames as Rust signs them at `signed_as` and `timestamps` (in milliseconds on a
	// timed track), delivered at `group` and `delivered`.
	let sign_group = |group: u64, signed_as: u64, timestamps: Option<([u64; 3], [u64; 3])>| {
		let track = if timestamps.is_some() { "video" } else { "data" };
		let scope = Scope::new(&path, &epoch, track);
		let frames: Vec<Value> = payloads
			.iter()
			.enumerate()
			.map(|(frame, payload)| {
				let (signed, delivered) = match timestamps {
					Some((signed, delivered)) => (Some(millis(signed[frame])), Some(delivered[frame])),
					None => (None, None),
				};
				let body = scope.sign(&key, signed_as, frame as u64, signed, payload);
				serde_json::json!({ "body": hex::encode(body), "timestamp": delivered })
			})
			.collect();
		let timescale = timestamps.map(|_| 1000);
		serde_json::json!({ "track": track, "group": group, "timescale": timescale, "frames": frames })
	};
	let write = |track: &str, micros: Option<[u64; 3]>| {
		let frames: Vec<Value> = expected
			.iter()
			.enumerate()
			.map(|(frame, payload)| serde_json::json!({ "payload": payload, "micros": micros.map(|m| m[frame]) }))
			.collect();
		serde_json::json!({ "track": track, "timescale": micros.map(|_| 1000), "frames": frames })
	};

	let stamps = [10, 20, 30];
	let output = bun(serde_json::json!({
		"seed": hex::encode(seed.as_ref()),
		"path": path.as_str(),
		"epoch": epoch.as_str(),
		"rust": [
			sign_group(7, 7, None),
			sign_group(8, 7, None),
			sign_group(9, 9, Some((stamps, stamps))),
			sign_group(10, 10, Some((stamps, [11, 20, 30]))),
		],
		// Microseconds on a millisecond track: JS signs the rounded value the wire carries.
		"js": [write("js", None), write("js", None), write("js-video", Some([2_400, 2_600, 3_000]))],
	}));

	assert_eq!(output["public"], key.public().to_string());
	assert_eq!(output["verified"][0], serde_json::json!(expected));
	assert_eq!(output["verified"][1], "signature");
	assert_eq!(output["verified"][2], serde_json::json!(expected));
	assert_eq!(output["verified"][3], "signature");

	let js_epoch = output["epoch"].as_str().unwrap().parse().unwrap();
	let groups = output["js"].as_array().unwrap();
	assert_eq!(groups.len(), 3);
	for row in groups {
		let scope = Scope::new(&path, &js_epoch, row["track"].as_str().unwrap());
		let group = row["group"].as_u64().unwrap();
		let timestamp = |frame: &Value| {
			let scale = row["timescale"].as_u64()?;
			Some(moq_net::Timestamp::from_scale(frame["timestamp"].as_u64().unwrap(), scale).unwrap())
		};
		let frames = row["frames"].as_array().unwrap();
		let opened: Vec<String> = frames
			.iter()
			.enumerate()
			.map(|(index, frame)| {
				let body = Bytes::from(hex(&frame["body"]));
				let opened = scope.verify(&key.public(), group, index as u64, timestamp(frame), body);
				hex::encode(opened.unwrap())
			})
			.collect();
		assert_eq!(opened, expected);
		// The same body does not verify at the neighboring group.
		let body = Bytes::from(hex(&frames[0]["body"]));
		let relocated = scope.verify(&key.public(), group + 1, 0, timestamp(&frames[0]), body);
		assert!(matches!(relocated, Err(Error::Signature)));
	}
	let timed: Vec<u64> = groups[2]["frames"]
		.as_array()
		.unwrap()
		.iter()
		.map(|frame| frame["timestamp"].as_u64().unwrap())
		.collect();
	assert_eq!(timed, [2, 3, 3]);
}
