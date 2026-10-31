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

/// A vector row's identity: its scope, group, and frame.
fn identity(row: &Value) -> (Arc<Scope>, u64, u64) {
	let path = moq_net::Path::new(row["path"].as_str().unwrap()).unwrap();
	let epoch = row["epoch"].as_str().unwrap().parse().unwrap();
	let scope = Scope::new(&path, &epoch, row["track"].as_str().unwrap());
	(scope, row["group"].as_u64().unwrap(), row["frame"].as_u64().unwrap())
}

#[test]
fn vector_keys() {
	for row in vectors()["keys"].as_array().unwrap() {
		let key = seed(&row["seed"]);
		assert_eq!(
			key.public().to_string(),
			row["public"].as_str().unwrap(),
			"{}",
			row["id"]
		);
		assert_eq!(key.public(), public(&row["public"]));
	}
}

#[test]
fn vector_frames() {
	for row in vectors()["frames"].as_array().unwrap() {
		let id = &row["id"];
		let (scope, group, frame) = identity(row);
		let payload = hex(&row["payload"]);
		assert_eq!(scope.message(group, frame, &payload), hex(&row["message"]), "{id}");
		let body = scope.sign(&seed(&row["seed"]), group, frame, &payload);
		assert_eq!(body.as_ref(), hex(&row["body"]), "{id}");
		let opened = scope.verify(&public(&row["public"]), group, frame, body).unwrap();
		assert_eq!(opened.as_ref(), payload, "{id}");
	}
}

#[test]
fn vector_negative() {
	for row in vectors()["negative"].as_array().unwrap() {
		let (scope, group, frame) = identity(row);
		let body = Bytes::from(hex(&row["body"]));
		let result = scope.verify(&public(&row["public"]), group, frame, body);
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
		(track::Producer::new(net.clone(), key.clone(), scope), net)
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
	group
		.write_frame(moq_net::Timestamp::from_millis(2).unwrap(), b"")
		.unwrap();
	group.finish().unwrap();

	let mut received = recv_group(&mut track).await;
	let first = received.read_frame().await.unwrap().unwrap();
	assert_eq!(first.payload, &b"keyframe"[..]);
	assert_eq!(first.timestamp, Some(moq_net::Timestamp::from_millis(1).unwrap()));
	assert_eq!(read(&mut received).await.unwrap().unwrap(), &b""[..]);
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

	let mut next = signed.append_group().unwrap();
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

/// Rust-signed groups verify in JS and a relocated one does not; JS-signed groups verify in Rust.
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
	// A group's bodies as Rust signs them at `signed_as`, delivered at `group`.
	let sign_group = |group: u64, signed_as: u64| {
		let scope = Scope::new(&path, &epoch, "video");
		let bodies: Vec<String> = payloads
			.iter()
			.enumerate()
			.map(|(frame, payload)| hex::encode(scope.sign(&key, signed_as, frame as u64, payload)))
			.collect();
		serde_json::json!({ "track": "video", "group": group, "bodies": bodies })
	};

	let output = bun(serde_json::json!({
		"seed": hex::encode(seed.as_ref()),
		"path": path.as_str(),
		"epoch": epoch.as_str(),
		"rust": [sign_group(7, 7), sign_group(8, 7)],
		"payloads": [expected, ["6a73"]],
	}));

	assert_eq!(output["public"], key.public().to_string());
	assert_eq!(output["verified"][0], serde_json::json!(expected));
	assert_eq!(output["verified"][1], "signature");

	let js_epoch = output["epoch"].as_str().unwrap().parse().unwrap();
	let groups = output["js"].as_array().unwrap();
	assert_eq!(groups.len(), 2);
	for (row, payloads) in groups.iter().zip([expected, vec!["6a73".to_string()]]) {
		let scope = Scope::new(&path, &js_epoch, row["track"].as_str().unwrap());
		let group = row["group"].as_u64().unwrap();
		let bodies = row["bodies"].as_array().unwrap();
		let opened: Vec<String> = bodies
			.iter()
			.enumerate()
			.map(|(frame, body)| {
				let opened = scope.verify(&key.public(), group, frame as u64, Bytes::from(hex(body)));
				hex::encode(opened.unwrap())
			})
			.collect();
		assert_eq!(opened, payloads);
		// The same body does not verify at the neighboring group.
		let relocated = scope.verify(&key.public(), group + 1, 0, Bytes::from(hex(&bodies[0])));
		assert!(matches!(relocated, Err(Error::Signature)));
	}
}
