use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;
use std::time::Duration;

use serde_json::Value;

use crate::credential::{Credential, Domain};
use crate::error::Error;
use crate::key::TrackKey;
use crate::limits::{MAX_GROUPED_PAYLOAD, MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, MAX_U32, MAX_U53, PROFILE, TAG_LEN};
use crate::{Publication, datagram_payload_limit, nonce, open, protect};

const VECTORS: &str = include_str!("../../../drafts/moq-e2ee-01.json");

fn unhex(value: &str) -> Vec<u8> {
	hex::decode(value).unwrap_or_else(|_| panic!("hex: {value}"))
}

fn credential_from(value: &Value) -> std::result::Result<Credential, Error> {
	let profile = value["profile"].as_str().unwrap_or("");
	let context = unhex(value["context"].as_str().unwrap_or(""));
	let generation = value["generation"].as_u64().ok_or(Error::Identity)?;
	let kid = value["kid"].as_u64().ok_or(Error::Identity)?;
	let secret = unhex(value["secret"].as_str().unwrap_or(""));
	Credential::new(profile, context, generation, kid, secret)
}

fn identity_u64(value: &Value) -> std::result::Result<u64, Error> {
	match value {
		Value::String(s) => match s.as_str() {
			"NaN" | "Infinity" | "-Infinity" => Err(Error::Identity),
			other => other
				.parse::<i128>()
				.ok()
				.and_then(|n| u64::try_from(n).ok())
				.ok_or(Error::Identity),
		},
		Value::Number(n) => n.as_u64().ok_or(Error::Identity),
		_ => Err(Error::Identity),
	}
}

fn vectors() -> Value {
	serde_json::from_str(VECTORS).unwrap()
}

#[test]
fn constants_match_vectors() {
	let v = vectors();
	let c = &v["constants"];
	assert_eq!(c["salt"], hex::encode(crate::SALT));
	assert_eq!(c["name_label"], hex::encode(crate::NAME_LABEL));
	assert_eq!(c["key_label"], hex::encode(crate::KEY_LABEL));
	assert_eq!(c["domain_group"], crate::DOMAIN_GROUP);
	assert_eq!(c["domain_datagram"], crate::DOMAIN_DATAGRAM);
	assert_eq!(c["tag_len"], TAG_LEN);
	assert_eq!(c["key_len"], crate::KEY_LEN);
	assert_eq!(c["name_len"], crate::NAME_LEN);
	assert_eq!(c["max_u53"], MAX_U53);
	assert_eq!(c["max_u32"], MAX_U32);
	assert_eq!(c["max_invocations"], MAX_INVOCATIONS);
	assert_eq!(c["max_plaintext_bytes"], MAX_PLAINTEXT_BYTES);
	assert_eq!(c["max_grouped_payload"], MAX_GROUPED_PAYLOAD);
	assert_eq!(c["max_grouped_plaintext"], crate::MAX_GROUPED_PLAINTEXT);
	assert_eq!(c["max_datagram_body"], crate::MAX_DATAGRAM_BODY);
}

#[test]
fn derivation_vectors() {
	let v = vectors();
	for row in v["derivation"].as_array().unwrap() {
		let cred = credential_from(row).unwrap();
		assert_eq!(
			hex::encode(cred.prk_bytes()),
			row["prk"].as_str().unwrap(),
			"{}",
			row["id"]
		);
		let semantic = unhex(row["semantic_name_bytes"].as_str().unwrap());
		assert_eq!(
			hex::encode(cred.name_info(&semantic).unwrap()),
			row["name_info"].as_str().unwrap()
		);
		let physical = cred.physical_name(&semantic).unwrap();
		assert_eq!(physical.as_str(), row["physical_name"].as_str().unwrap());
		let mut material = [0u8; 16];
		let n = base64::Engine::decode_slice(
			&base64::engine::general_purpose::URL_SAFE_NO_PAD,
			physical.as_bytes(),
			&mut material,
		)
		.unwrap();
		assert_eq!(n, 16);
		assert_eq!(hex::encode(material), row["name_material"].as_str().unwrap());
		let domain = Domain::from_u8(row["domain"].as_u64().unwrap() as u8).unwrap();
		assert_eq!(
			hex::encode(cred.key_info(&physical, domain).unwrap()),
			row["key_info"].as_str().unwrap()
		);
		assert_eq!(
			hex::encode(cred.key_bytes(&physical, domain).unwrap()),
			row["key"].as_str().unwrap()
		);
	}
}

#[test]
fn naming_vectors() {
	let v = vectors();
	let cred = credential_from(&v["credential"]).unwrap();
	for row in v["naming"].as_array().unwrap() {
		let physical = cred.physical_name(row["semantic_name"].as_str().unwrap()).unwrap();
		assert_eq!(physical.as_str(), row["physical_name"].as_str().unwrap());
	}
}

fn payload_row(row: &Value) {
	let cred = credential_from(row).unwrap();
	let physical = cred.physical_name(row["semantic_name"].as_str().unwrap()).unwrap();
	assert_eq!(physical.as_str(), row["physical_name"].as_str().unwrap());
	let domain = Domain::from_u8(row["domain"].as_u64().unwrap() as u8).unwrap();
	let key = cred.key_bytes(&physical, domain).unwrap();
	assert_eq!(hex::encode(key), row["key"].as_str().unwrap());
	let group = row["group"].as_u64().unwrap();
	let frame = row["frame"].as_u64().unwrap();
	assert_eq!(
		hex::encode(nonce(group, frame).unwrap()),
		row["nonce"].as_str().unwrap()
	);
	let plaintext = unhex(row["plaintext"].as_str().unwrap());
	let limit = row["payload_limit"].as_u64().unwrap() as usize;
	let payload = protect(&key, group, frame, &plaintext, limit).unwrap();
	assert_eq!(hex::encode(&payload), row["payload"].as_str().unwrap(), "{}", row["id"]);
	let opened = open(&key, group, frame, &payload, limit).unwrap();
	assert_eq!(&opened[..], &plaintext[..]);
}

#[test]
fn group_vectors() {
	for row in vectors()["groups"].as_array().unwrap() {
		payload_row(row);
	}
}

#[test]
fn datagram_vectors() {
	for row in vectors()["datagrams"].as_array().unwrap() {
		payload_row(row);
		let header = unhex(row["header"].as_str().unwrap());
		let subscribe = row["subscribe_id"].as_u64().unwrap();
		let group = row["group"].as_u64().unwrap();
		let timestamp = row["timestamp"].as_u64().unwrap();
		assert_eq!(
			datagram_payload_limit(subscribe, group, timestamp).unwrap(),
			row["payload_limit"].as_u64().unwrap() as usize
		);
		assert_eq!(
			header.len(),
			crate::varint_len(subscribe) + crate::varint_len(group) + crate::varint_len(timestamp)
		);
	}
}

#[test]
fn catalog_vectors() {
	for row in vectors()["catalog"].as_array().unwrap() {
		payload_row(row);
	}
}

#[test]
fn catalog_deflate_roundtrip() {
	let cred = credential_from(&vectors()["credential"]).unwrap();
	let json = br#"{"video":{"renditions":{"vi1-O53ixrfzaEYq5XbPZg":{"codec":"avc1"}}}}"#;
	let payload = crate::catalog::protect_deflate(&cred, json).unwrap();
	let opened = crate::catalog::open_deflate(&cred, &payload).unwrap();
	assert_eq!(&opened[..], json);
}

#[test]
fn negative_vectors() {
	let v = vectors();
	for row in v["negative"].as_array().unwrap() {
		let id = row["id"].as_str().unwrap();
		let expected = row["error"].as_str().unwrap();
		let got = match row["operation"].as_str().unwrap() {
			"open" => negative_open(row),
			"identity" => {
				let group = identity_u64(&row["group"]);
				let frame = identity_u64(&row["frame"]);
				match (group, frame) {
					(Ok(g), Ok(f)) => nonce(g, f).err().unwrap_or_else(|| panic!("{id} should fail")),
					(Err(e), _) | (_, Err(e)) => e,
				}
			}
			"credential" => credential_from(&row["credential"]).unwrap_err(),
			"bytes" => {
				let field = row["field"].as_str().unwrap();
				let len = row["length"].as_u64().unwrap() as usize;
				let data = vec![0u8; len];
				let cred = credential_from(&v["credential"]).unwrap();
				match field {
					"context" => Credential::new(PROFILE, data, 1, 7, cred_secret()).unwrap_err(),
					"semantic_name" => cred.physical_name(&data).unwrap_err(),
					other => panic!("unknown bytes field {other}"),
				}
			}
			"protect" => {
				let key = unhex(row["key"].as_str().unwrap());
				let group = row["group"].as_u64().unwrap();
				let frame = row["frame"].as_u64().unwrap();
				let len = row["plaintext_len"].as_u64().unwrap() as usize;
				let limit = row["payload_limit"].as_u64().unwrap() as usize;
				let plaintext = vec![0u8; len];
				protect(&key, group, frame, &plaintext, limit).unwrap_err()
			}
			other => panic!("unknown operation {other}"),
		};
		assert_eq!(got.code(), expected, "{id}");
	}
}

fn cred_secret() -> Vec<u8> {
	unhex(vectors()["credential"]["secret"].as_str().unwrap())
}

fn negative_open(row: &Value) -> Error {
	let cred = credential_from(&row["credential"]).unwrap();
	let physical = crate::PhysicalName::parse(row["physical_name"].as_str().unwrap()).unwrap();
	let domain = Domain::from_u8(row["domain"].as_u64().unwrap() as u8).unwrap();
	let key = cred.key_bytes(&physical, domain).unwrap();
	assert_eq!(hex::encode(key), row["key"].as_str().unwrap());
	let group = row["group"].as_u64().unwrap();
	let frame = row["frame"].as_u64().unwrap();
	let payload = unhex(row["payload"].as_str().unwrap());
	let limit = row["payload_limit"].as_u64().unwrap() as usize;
	open(&key, group, frame, &payload, limit).unwrap_err()
}

fn unique_context() -> Vec<u8> {
	static N: AtomicU64 = AtomicU64::new(0);
	format!("test-{}", N.fetch_add(1, Ordering::Relaxed)).into_bytes()
}

fn test_secret() -> [u8; 32] {
	*b"moq-e2ee-01 test secret!!!!!!!!!"
}

fn test_credential() -> Credential {
	Credential::new(PROFILE, unique_context(), 1, 7, test_secret()).unwrap()
}

fn subscribe_all() -> moq_net::track::Subscription {
	moq_net::track::Subscription::default().with_max_age(Duration::from_secs(60))
}

fn pair(semantic: &str) -> (Publication, crate::track::Producer, crate::track::Consumer) {
	let cred = test_credential();
	let publication = cred.publish().unwrap();
	let physical = cred.physical_name(semantic).unwrap();
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(physical.as_str(), None)
		.unwrap();
	let subscriber = net.subscribe(subscribe_all());
	let producer = publication.track(net, semantic).unwrap();
	let consumer = crate::track::Consumer::new(&cred, subscriber, Some(&cred.pin())).unwrap();
	(publication, producer, consumer)
}

fn drain_frame(consumer: &mut crate::group::Consumer) -> crate::Frame {
	let waiter = kio::Waiter::noop();
	match consumer.poll_read_frame(&waiter) {
		Poll::Ready(Ok(Some(frame))) => frame,
		other => panic!("expected frame, got {other:?}"),
	}
}

#[test]
fn grouped_roundtrip() {
	let (_pub, mut producer, mut consumer) = pair("video");
	let mut group = producer.append_group().unwrap();
	assert_eq!(group.sequence(), 0);
	group
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"frame-zero")
		.unwrap();
	group
		.write_frame(moq_net::Timestamp::from_millis(2).unwrap(), b"frame-one")
		.unwrap();
	group.finish().unwrap();

	let waiter = kio::Waiter::noop();
	let mut group = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(group))) => group,
		other => panic!("{other:?}"),
	};
	assert_eq!(group.sequence(), 0);
	assert_eq!(drain_frame(&mut group).plaintext, &b"frame-zero"[..]);
	assert_eq!(drain_frame(&mut group).plaintext, &b"frame-one"[..]);
	assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
}

#[test]
fn datagram_roundtrip_and_retransmit() {
	let (_pub, mut producer, mut consumer) = pair("audio");
	let ts = moq_net::Timestamp::from_millis(5).unwrap();
	let seq = producer.append_datagram(ts, b"opus").unwrap();
	assert_eq!(seq, 0);
	let first = producer.datagram_ciphertext(0).unwrap().clone();
	assert_eq!(producer.datagram_invocations(), 1);
	producer.retransmit_datagram(0).unwrap();
	assert_eq!(producer.datagram_invocations(), 1);
	assert_eq!(producer.datagram_ciphertext(0).unwrap(), &first);

	let waiter = kio::Waiter::noop();
	match consumer.poll_recv_datagram(&waiter) {
		Poll::Ready(Ok(Some(crate::DatagramEvent::Datagram(d)))) => {
			assert_eq!(d.sequence, 0);
			assert_eq!(&d.plaintext[..], b"opus");
		}
		other => panic!("{other:?}"),
	}
}

#[test]
fn sequence_reuse_refused_before_encrypt() {
	let (_pub, mut producer, _consumer) = pair("video");
	producer.create_group(0).unwrap().finish().unwrap();
	assert_eq!(producer.create_group(0).map(|_| ()).unwrap_err().code(), "reuse");
	assert_eq!(
		producer
			.insert_datagram(0, moq_net::Timestamp::ZERO, b"x")
			.map(|_| ())
			.unwrap_err()
			.code(),
		"reuse"
	);
}

#[test]
fn same_generation_restart_refused() {
	let cred = test_credential();
	let first = cred.publish().unwrap();
	drop(first);
	assert_eq!(cred.publish().map(|_| ()).unwrap_err().code(), "reuse");
	let rotated = Credential::new(PROFILE, cred.context().to_vec(), 2, 7, test_secret()).unwrap();
	rotated.publish().unwrap();
}

#[test]
fn pin_mismatch() {
	let cred = test_credential();
	let physical = cred.physical_name("video").unwrap();
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(physical.as_str(), None)
		.unwrap();
	let subscriber = net.subscribe(subscribe_all());
	let pin = crate::Pin::new(9, 9).unwrap();
	assert_eq!(
		crate::track::Consumer::new(&cred, subscriber, Some(&pin))
			.map(|_| ())
			.unwrap_err()
			.code(),
		"pinned_mismatch"
	);
}

#[test]
fn invocation_exhaustion() {
	let cred = test_credential();
	let physical = cred.physical_name("video").unwrap();
	let mut key = TrackKey::derive(&cred, &physical, Domain::Group).unwrap();
	key.set_usage(MAX_INVOCATIONS - 1, 0);
	key.protect(0, 0, b"last", MAX_GROUPED_PAYLOAD).unwrap();
	assert_eq!(
		key.protect(0, 1, b"nope", MAX_GROUPED_PAYLOAD).unwrap_err().code(),
		"exhausted"
	);
}

#[test]
fn plaintext_byte_exhaustion() {
	let cred = test_credential();
	let physical = cred.physical_name("video").unwrap();
	let mut key = TrackKey::derive(&cred, &physical, Domain::Group).unwrap();
	key.set_usage(0, MAX_PLAINTEXT_BYTES - 3);
	key.protect(0, 0, b"ab", MAX_GROUPED_PAYLOAD).unwrap();
	assert_eq!(
		key.protect(0, 1, b"abcd", MAX_GROUPED_PAYLOAD).unwrap_err().code(),
		"exhausted"
	);
}

#[test]
fn grouped_authentication_ends_track() {
	let cred = test_credential();
	let publication = cred.publish().unwrap();
	let physical = cred.physical_name("video").unwrap();
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(physical.as_str(), None)
		.unwrap();
	let subscriber = net.subscribe(subscribe_all());
	let mut producer = publication.track(net.clone(), "video").unwrap();
	let mut consumer = crate::track::Consumer::new(&cred, subscriber, None).unwrap();

	let mut group = producer.append_group().unwrap();
	group
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"ok")
		.unwrap();
	group.finish().unwrap();

	// Tamper with a second group by writing ciphertext directly.
	let mut bad = net.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
	let mut payload = producer_ciphertext_for(&cred, "video", 1, 0, b"ok").to_vec();
	payload[0] ^= 1;
	bad.write_frame(moq_net::Timestamp::from_millis(2).unwrap(), payload)
		.unwrap();
	bad.finish().unwrap();

	let waiter = kio::Waiter::noop();
	let mut first = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(group))) => group,
		other => panic!("{other:?}"),
	};
	assert_eq!(drain_frame(&mut first).plaintext, &b"ok"[..]);

	let mut second = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(group))) => group,
		other => panic!("{other:?}"),
	};
	assert_eq!(
		second.poll_read_frame(&waiter).map(|r| r.err().map(|e| e.code())),
		Poll::Ready(Some("authentication"))
	);
	assert_eq!(
		consumer.poll_recv_group(&waiter).map(|r| r.err().map(|e| e.code())),
		Poll::Ready(Some("authentication"))
	);
}

fn producer_ciphertext_for(
	cred: &Credential,
	semantic: &str,
	group: u64,
	frame: u64,
	plaintext: &[u8],
) -> bytes::Bytes {
	let physical = cred.physical_name(semantic).unwrap();
	let key = cred.key_bytes(&physical, Domain::Group).unwrap();
	protect(&key, group, frame, plaintext, MAX_GROUPED_PAYLOAD).unwrap()
}

#[test]
fn bad_datagram_is_event() {
	let cred = test_credential();
	let publication = cred.publish().unwrap();
	let physical = cred.physical_name("audio").unwrap();
	let mut net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(physical.as_str(), None)
		.unwrap();
	let subscriber = net.subscribe(subscribe_all());
	let mut producer = publication.track(net.clone(), "audio").unwrap();
	let mut consumer = crate::track::Consumer::new(&cred, subscriber, None).unwrap();

	producer
		.append_datagram(moq_net::Timestamp::from_millis(1).unwrap(), b"one")
		.unwrap();
	let mut payload = {
		let key = cred.key_bytes(&physical, Domain::Datagram).unwrap();
		protect(&key, 1, 0, b"two", 1196).unwrap().to_vec()
	};
	payload[0] ^= 1;
	let payload = bytes::Bytes::from(payload);
	net.insert_datagram(1, moq_net::Timestamp::from_millis(2).unwrap(), payload)
		.unwrap();
	producer
		.insert_datagram(2, moq_net::Timestamp::from_millis(3).unwrap(), b"three")
		.unwrap();

	let waiter = kio::Waiter::noop();
	assert!(matches!(
		consumer.poll_recv_datagram(&waiter),
		Poll::Ready(Ok(Some(crate::DatagramEvent::Datagram(_))))
	));
	assert!(matches!(
		consumer.poll_recv_datagram(&waiter),
		Poll::Ready(Ok(Some(crate::DatagramEvent::Authentication { sequence: 1 })))
	));
	match consumer.poll_recv_datagram(&waiter) {
		Poll::Ready(Ok(Some(crate::DatagramEvent::Datagram(d)))) => assert_eq!(&d.plaintext[..], b"three"),
		other => panic!("{other:?}"),
	}
}

#[test]
fn duplicate_datagram_event() {
	let (_pub, mut producer, mut consumer) = pair("audio");
	producer
		.append_datagram(moq_net::Timestamp::from_millis(1).unwrap(), b"once")
		.unwrap();
	producer.retransmit_datagram(0).unwrap();
	let waiter = kio::Waiter::noop();
	assert!(matches!(
		consumer.poll_recv_datagram(&waiter),
		Poll::Ready(Ok(Some(crate::DatagramEvent::Datagram(_))))
	));
	assert!(matches!(
		consumer.poll_recv_datagram(&waiter),
		Poll::Ready(Ok(Some(crate::DatagramEvent::Duplicate { sequence: 0 })))
	));
}

#[test]
fn group_replacement() {
	let (_pub, mut producer, mut consumer) = pair("video");
	let mut g0 = producer.append_group().unwrap();
	g0.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"old")
		.unwrap();
	g0.finish().unwrap();
	let mut g1 = producer.append_group().unwrap();
	g1.write_frame(moq_net::Timestamp::from_millis(2).unwrap(), b"new")
		.unwrap();
	g1.finish().unwrap();

	let waiter = kio::Waiter::noop();
	let mut first = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(g))) => g,
		other => panic!("{other:?}"),
	};
	assert_eq!(first.sequence(), 0);
	assert_eq!(drain_frame(&mut first).plaintext, &b"old"[..]);
	let mut second = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(g))) => g,
		other => panic!("{other:?}"),
	};
	assert_eq!(second.sequence(), 1);
	assert_eq!(drain_frame(&mut second).plaintext, &b"new"[..]);
}

#[test]
fn cancellation_ends_consumer() {
	let (_pub, mut producer, mut consumer) = pair("video");
	let mut group = producer.append_group().unwrap();
	group
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"x")
		.unwrap();
	group.finish().unwrap();
	producer.finish().unwrap();
	let waiter = kio::Waiter::noop();
	let mut group = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(g))) => g,
		other => panic!("{other:?}"),
	};
	drain_frame(&mut group);
	assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
	assert!(matches!(consumer.poll_recv_group(&waiter), Poll::Ready(Ok(None))));
}

#[test]
fn secrets_stay_out_of_debug() {
	let cred = test_credential();
	let text = format!("{cred:?}");
	assert!(text.contains("<redacted>"));
	assert!(!text.contains(&hex::encode(test_secret())));
	let physical = cred.physical_name("video").unwrap();
	let key = TrackKey::derive(&cred, &physical, Domain::Group).unwrap();
	let key_dbg = format!("{key:?}");
	assert!(key_dbg.contains("<redacted>"));
	assert!(!key_dbg.contains(&hex::encode(key.key_bytes())));
}

#[test]
fn wrong_track_name_is_identity() {
	let cred = test_credential();
	let publication = cred.publish().unwrap();
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track("not-a-physical-name", None)
		.unwrap();
	assert_eq!(
		publication.track(net, "video").map(|_| ()).unwrap_err().code(),
		"identity"
	);
}

#[test]
fn forged_datagram_does_not_burn_identity() {
	let cred = test_credential();
	let publication = cred.publish().unwrap();
	let physical = cred.physical_name("audio").unwrap();
	let mut net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(physical.as_str(), None)
		.unwrap();
	let subscriber = net.subscribe(subscribe_all());
	let mut producer = publication.track(net.clone(), "audio").unwrap();
	let mut consumer = crate::track::Consumer::new(&cred, subscriber, None).unwrap();

	producer
		.append_datagram(moq_net::Timestamp::from_millis(1).unwrap(), b"one")
		.unwrap();
	// Forged seq 1 first: flips a bit so AEAD fails.
	let mut forged = {
		let key = cred.key_bytes(&physical, Domain::Datagram).unwrap();
		protect(&key, 1, 0, b"two", 1196).unwrap().to_vec()
	};
	forged[0] ^= 1;
	net.insert_datagram(
		1,
		moq_net::Timestamp::from_millis(2).unwrap(),
		bytes::Bytes::from(forged),
	)
	.unwrap();
	// Real seq 1 after the forgery.
	producer
		.insert_datagram(1, moq_net::Timestamp::from_millis(2).unwrap(), b"two")
		.unwrap();

	let waiter = kio::Waiter::noop();
	assert!(matches!(
		consumer.poll_recv_datagram(&waiter),
		Poll::Ready(Ok(Some(crate::DatagramEvent::Datagram(_))))
	));
	assert!(matches!(
		consumer.poll_recv_datagram(&waiter),
		Poll::Ready(Ok(Some(crate::DatagramEvent::Authentication { sequence: 1 })))
	));
	match consumer.poll_recv_datagram(&waiter) {
		Poll::Ready(Ok(Some(crate::DatagramEvent::Datagram(d)))) => {
			assert_eq!(d.sequence, 1);
			assert_eq!(&d.plaintext[..], b"two");
		}
		other => panic!("forged must not burn the identity, got {other:?}"),
	}
}

#[test]
fn datagram_retention_evicts_outside_window() {
	let (_pub, mut producer, _consumer) = pair("audio");
	let window = crate::DATAGRAM_DUPLICATE_WINDOW as u64;
	for seq in 0..=window {
		producer
			.append_datagram(moq_net::Timestamp::from_millis(seq).unwrap(), b"x")
			.unwrap();
	}
	assert!(producer.datagram_ciphertext(0).is_none());
	assert!(producer.datagram_ciphertext(window).is_some());
	assert_eq!(producer.retransmit_datagram(0).map(|_| ()).unwrap_err().code(), "reuse");
	producer.retransmit_datagram(window).unwrap();
}

#[test]
fn failed_group_write_does_not_burn_nonce() {
	let (_pub, mut producer, mut consumer) = pair("video");
	let mut group = producer.append_group().unwrap();
	// (2^62-1) seconds overflows conversion to the milli-scale track.
	let huge = moq_net::Timestamp::from_secs((1 << 62) - 1).unwrap();
	assert!(group.write_frame(huge, b"bad").is_err());
	assert_eq!(group.invocations(), 0);
	group
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"ok")
		.unwrap();
	assert_eq!(group.invocations(), 1);
	assert_eq!(group.next_frame(), 1);
	group.finish().unwrap();

	let waiter = kio::Waiter::noop();
	let mut group = match consumer.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(group))) => group,
		other => panic!("{other:?}"),
	};
	assert_eq!(drain_frame(&mut group).plaintext, &b"ok"[..]);
}

#[test]
fn resumed_group_opens_at_transport_index() {
	use std::sync::{Arc, Mutex, atomic::AtomicBool};

	let cred = test_credential();
	let publication = cred.publish().unwrap();
	let physical = cred.physical_name("video").unwrap();
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(physical.as_str(), None)
		.unwrap();
	let mut subscriber = net.subscribe(subscribe_all());
	let mut producer = publication.track(net, "video").unwrap();

	let mut group = producer.append_group().unwrap();
	group
		.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"zero")
		.unwrap();
	group
		.write_frame(moq_net::Timestamp::from_millis(2).unwrap(), b"one")
		.unwrap();
	group.finish().unwrap();

	// Simulate a resume at object 1: skip frame 0 on the net cursor before wrapping.
	let waiter = kio::Waiter::noop();
	let mut net_group = match subscriber.poll_recv_group(&waiter) {
		Poll::Ready(Ok(Some(group))) => group,
		Poll::Ready(Ok(None)) => panic!("net group ended"),
		Poll::Ready(Err(_)) => panic!("net group error"),
		Poll::Pending => panic!("net group pending"),
	};
	net_group.skip_to(1);
	let key = Arc::new(Mutex::new(TrackKey::derive(&cred, &physical, Domain::Group).unwrap()));
	let window = Arc::new(Mutex::new(crate::window::GroupWindow::default()));
	// GroupWindow is pub(crate); construct via a fresh track consumer window is not
	// accessible, so use the track consumer path instead by skipping through e2ee.
	// Fall back to direct construction: window starts empty, which is fine for one frame.
	let auth_failed = Arc::new(AtomicBool::new(false));
	let mut group = crate::group::Consumer::new(net_group, key, window, auth_failed);
	assert_eq!(drain_frame(&mut group).plaintext, &b"one"[..]);
}
