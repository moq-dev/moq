use std::task::Poll;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use serde_json::Value;

use crate::credential::{Config, Credential};
use crate::datagram::Event;
use crate::error::Error;
use crate::generation::{Domain, Generation};
use crate::limits::{
	DATAGRAM_WINDOW, KEY_LABEL, KEY_LEN, MAX_DATAGRAM_BODY, MAX_DATAGRAM_HEADER, MAX_DATAGRAM_PAYLOAD,
	MAX_DATAGRAM_PLAINTEXT, MAX_GROUPED_PAYLOAD, MAX_GROUPED_PLAINTEXT, MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, MAX_U53,
	NAME_LABEL, NAME_LEN, PATH_LABEL, PROFILE, SECRET_LEN, TAG_LEN,
};
use crate::name::Name;
use crate::protect::{nonce, open, protect};
use crate::{Epoch, group, track};

const VECTORS: &str = include_str!("../../../drafts/moq-e2ee-00.json");

fn vectors() -> Value {
	serde_json::from_str(VECTORS).unwrap()
}

fn unhex(value: &str) -> Vec<u8> {
	hex::decode(value).unwrap_or_else(|_| panic!("hex: {value}"))
}

fn code(err: &Error) -> String {
	err.to_string()
}

/// Build a credential from a vector row; a wrong-length secret is refused at the type boundary.
fn credential_from(row: &Value) -> Result<Credential, String> {
	let context = Bytes::from(unhex(row["context"].as_str().unwrap_or("")));
	let kid = row["kid"].as_u64().ok_or("identity")?;
	let secret =
		<[u8; SECRET_LEN]>::try_from(unhex(row["secret"].as_str().unwrap_or(""))).map_err(|_| "invalid_secret")?;
	Credential::new(Config { context, kid, secret }).map_err(|err| code(&err))
}

fn generation_from(row: &Value) -> Result<Generation, String> {
	let credential = credential_from(row)?;
	let epoch: Epoch = row["epoch"]
		.as_str()
		.unwrap_or("")
		.parse()
		.map_err(|err: Error| code(&err))?;
	Ok(credential.generation(epoch))
}

fn domain_from(row: &Value) -> Domain {
	match row["domain"].as_u64().unwrap() {
		0 => Domain::Group,
		1 => Domain::Datagram,
		other => panic!("domain {other}"),
	}
}

fn identity_u64(value: &Value) -> Result<u64, Error> {
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

fn decode_material(text: &str) -> [u8; NAME_LEN] {
	let mut material = [0u8; NAME_LEN];
	let n = URL_SAFE_NO_PAD.decode_slice(text.as_bytes(), &mut material).unwrap();
	assert_eq!(n, NAME_LEN);
	material
}

#[test]
fn constants_match_vectors() {
	let v = vectors();
	assert_eq!(v["profile"], PROFILE);
	let c = &v["constants"];
	assert_eq!(c["salt"], hex::encode(PROFILE));
	assert_eq!(c["path_label"], hex::encode(PATH_LABEL));
	assert_eq!(c["name_label"], hex::encode(NAME_LABEL));
	assert_eq!(c["key_label"], hex::encode(KEY_LABEL));
	assert_eq!(c["domain_group"], Domain::Group as u8);
	assert_eq!(c["domain_datagram"], Domain::Datagram as u8);
	assert_eq!(c["tag_len"], TAG_LEN);
	assert_eq!(c["key_len"], KEY_LEN);
	assert_eq!(c["name_len"], NAME_LEN);
	assert_eq!(c["max_u53"], MAX_U53);
	assert_eq!(c["max_u32"], u32::MAX);
	assert_eq!(c["max_invocations"], MAX_INVOCATIONS);
	assert_eq!(c["max_plaintext_bytes"], MAX_PLAINTEXT_BYTES);
	assert_eq!(c["max_grouped_payload"], MAX_GROUPED_PAYLOAD);
	assert_eq!(c["max_grouped_plaintext"], MAX_GROUPED_PLAINTEXT);
	assert_eq!(c["max_datagram_body"], MAX_DATAGRAM_BODY);
	assert_eq!(c["max_datagram_header"], MAX_DATAGRAM_HEADER);
	assert_eq!(c["max_datagram_payload"], MAX_DATAGRAM_PAYLOAD);
	assert_eq!(c["max_datagram_plaintext"], MAX_DATAGRAM_PLAINTEXT);
}

#[test]
fn path_vectors() {
	for row in vectors()["paths"].as_array().unwrap() {
		let cred = credential_from(row).unwrap();
		let semantic = row["semantic_name"].as_str().unwrap();
		assert_eq!(unhex(row["semantic_name_bytes"].as_str().unwrap()), semantic.as_bytes());
		assert_eq!(hex::encode(cred.prk_bytes()), row["prk"], "{}", row["id"]);
		assert_eq!(hex::encode(cred.path_info(semantic).unwrap()), row["path_info"]);
		let path = cred.path(semantic).unwrap();
		assert_eq!(path.as_str(), row["opaque"]);
		assert_eq!(hex::encode(decode_material(path.as_str())), row["path_material"]);
	}
}

#[test]
fn derivation_vectors() {
	for row in vectors()["derivation"].as_array().unwrap() {
		let generation = generation_from(row).unwrap();
		let semantic = row["semantic_name"].as_str().unwrap();
		assert_eq!(unhex(row["semantic_name_bytes"].as_str().unwrap()), semantic.as_bytes());
		assert_eq!(
			hex::encode(generation.credential().prk_bytes()),
			row["prk"],
			"{}",
			row["id"]
		);
		assert_eq!(hex::encode(generation.name_info(semantic).unwrap()), row["name_info"]);
		let name = generation.name(semantic).unwrap();
		assert_eq!(name.as_str(), row["physical_name"]);
		assert_eq!(hex::encode(decode_material(name.as_str())), row["name_material"]);
		let domain = domain_from(row);
		assert_eq!(
			hex::encode(generation.key_info(&name, domain).unwrap()),
			row["key_info"]
		);
		assert_eq!(hex::encode(generation.key_bytes(&name, domain).unwrap()), row["key"]);
	}
}

#[test]
fn naming_vectors() {
	let v = vectors();
	let cred = credential_from(&v["generation"]).unwrap();
	for row in v["naming"].as_array().unwrap() {
		let epoch: Epoch = row["epoch"].as_str().unwrap().parse().unwrap();
		let name = cred
			.generation(epoch)
			.name(row["semantic_name"].as_str().unwrap())
			.unwrap();
		assert_eq!(name.as_str(), row["physical_name"], "{}", row["id"]);
	}
}

fn payload_row(row: &Value) {
	let generation = generation_from(row).unwrap();
	let name = generation.name(row["semantic_name"].as_str().unwrap()).unwrap();
	assert_eq!(name.as_str(), row["physical_name"]);
	let key = generation.key_bytes(&name, domain_from(row)).unwrap();
	assert_eq!(hex::encode(key), row["key"]);
	let group = row["group"].as_u64().unwrap();
	let frame = row["frame"].as_u64().unwrap();
	assert_eq!(hex::encode(nonce(group, frame).unwrap()), row["nonce"]);
	let plaintext = unhex(row["plaintext"].as_str().unwrap());
	let limit = row["payload_limit"].as_u64().unwrap() as usize;
	let payload = protect(&key, group, frame, &plaintext, limit).unwrap();
	assert_eq!(hex::encode(&payload), row["payload"], "{}", row["id"]);
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
		assert_eq!(row["payload_limit"], MAX_DATAGRAM_PAYLOAD);
		payload_row(row);
	}
}

#[test]
fn negative_vectors() {
	let v = vectors();
	let secret = <[u8; SECRET_LEN]>::try_from(unhex(v["generation"]["secret"].as_str().unwrap())).unwrap();
	let cred = credential_from(&v["generation"]).unwrap();
	for row in v["negative"].as_array().unwrap() {
		let id = row["id"].as_str().unwrap();
		let expected = row["error"].as_str().unwrap();
		let got = match row["operation"].as_str().unwrap() {
			"open" => {
				let generation = generation_from(&row["generation"]).unwrap();
				let name: Name = row["physical_name"].as_str().unwrap().parse().unwrap();
				let key = generation.key_bytes(&name, domain_from(row)).unwrap();
				assert_eq!(hex::encode(key), row["key"]);
				let group = row["group"].as_u64().unwrap();
				let frame = row["frame"].as_u64().unwrap();
				let payload = unhex(row["payload"].as_str().unwrap());
				let limit = row["payload_limit"].as_u64().unwrap() as usize;
				code(&open(&key, group, frame, &payload, limit).unwrap_err())
			}
			"identity" => match (identity_u64(&row["group"]), identity_u64(&row["frame"])) {
				(Ok(g), Ok(f)) => code(&nonce(g, f).err().unwrap_or_else(|| panic!("{id} should fail"))),
				(Err(e), _) | (_, Err(e)) => code(&e),
			},
			"generation" => generation_from(&row["generation"]).map(|_| ()).unwrap_err(),
			"key" => code(&row["physical_name"].as_str().unwrap().parse::<Name>().unwrap_err()),
			"bytes" => {
				let len = row["length"].as_u64().unwrap() as usize;
				let data = vec![b'a'; len];
				match row["field"].as_str().unwrap() {
					"context" => Credential::new(Config {
						context: data.into(),
						kid: 7,
						secret,
					})
					.map(|_| ())
					.map_err(|err| code(&err))
					.unwrap_err(),
					"epoch" => code(&String::from_utf8(data).unwrap().parse::<Epoch>().unwrap_err()),
					"semantic_name" => {
						let semantic = String::from_utf8(data).unwrap();
						let generation = cred.generation(Epoch::mint());
						let name = code(&generation.name(&semantic).unwrap_err());
						assert_eq!(code(&cred.path(&semantic).unwrap_err()), name);
						name
					}
					other => panic!("unknown bytes field {other}"),
				}
			}
			"protect" => {
				let key = <[u8; KEY_LEN]>::try_from(unhex(row["key"].as_str().unwrap())).unwrap();
				let group = row["group"].as_u64().unwrap();
				let frame = row["frame"].as_u64().unwrap();
				let plaintext = vec![0u8; row["plaintext_len"].as_u64().unwrap() as usize];
				let limit = row["payload_limit"].as_u64().unwrap() as usize;
				code(&protect(&key, group, frame, &plaintext, limit).unwrap_err())
			}
			other => panic!("unknown operation {other}"),
		};
		assert_eq!(got, expected, "{id}");
	}
}

fn test_secret() -> [u8; SECRET_LEN] {
	*b"moq-e2ee-00 test secret!!!!!!!!!"
}

fn test_credential() -> Credential {
	Credential::new(Config {
		context: Bytes::from_static(b"example.com/meeting-123"),
		kid: 7,
		secret: test_secret(),
	})
	.unwrap()
}

fn test_generation() -> Generation {
	test_credential().generation(Epoch::mint())
}

fn subscribe_all() -> moq_net::track::Subscription {
	moq_net::track::Subscription::default().with_max_age(Duration::from_secs(60))
}

fn net_track(name: &Name) -> moq_net::track::Producer {
	moq_net::broadcast::Info::new()
		.produce()
		.create_track(name.as_str(), None)
		.unwrap()
}

struct Pair {
	generation: Generation,
	net: moq_net::track::Producer,
	producer: track::Producer,
	consumer: track::Consumer,
}

fn pair(semantic: &str) -> Pair {
	let generation = test_generation();
	let net = net_track(&generation.name(semantic).unwrap());
	let producer = generation.produce(net.clone()).unwrap();
	let consumer = generation.consume(net.subscribe(subscribe_all())).unwrap();
	Pair {
		generation,
		net,
		producer,
		consumer,
	}
}

fn ms(value: u64) -> moq_net::Timestamp {
	moq_net::Timestamp::from_millis(value).unwrap()
}

fn recv_group(consumer: &mut track::Consumer) -> group::Consumer {
	match consumer.poll_recv_group(&kio::Waiter::noop()) {
		Poll::Ready(Ok(Some(group))) => group,
		other => panic!("expected group, got {other:?}"),
	}
}

fn read_frame(consumer: &mut group::Consumer) -> group::Frame {
	match consumer.poll_read_frame(&kio::Waiter::noop()) {
		Poll::Ready(Ok(Some(frame))) => frame,
		other => panic!("expected frame, got {other:?}"),
	}
}

fn recv_datagram(consumer: &mut track::Consumer) -> Event {
	match consumer.poll_recv_datagram(&kio::Waiter::noop()) {
		Poll::Ready(Ok(Some(event))) => event,
		other => panic!("expected datagram event, got {other:?}"),
	}
}

fn datagram_ciphertext(generation: &Generation, name: &Name, sequence: u64, plaintext: &[u8]) -> Bytes {
	let key = generation.key_bytes(name, Domain::Datagram).unwrap();
	protect(&key, sequence, 0, plaintext, MAX_DATAGRAM_PAYLOAD).unwrap()
}

#[test]
fn epoch_mint_is_lowercase_uuid_v7() {
	let epoch = Epoch::mint();
	let text = epoch.as_str();
	assert_eq!(text.len(), 36);
	assert_eq!(text.as_bytes()[14], b'7');
	assert!(text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-')));
	assert_eq!(text.parse::<Epoch>().unwrap(), epoch);
	assert!(Epoch::mint() > epoch, "newer epochs sort greatest");
}

#[test]
fn epoch_rejects_malformed() {
	assert_eq!(code(&"".parse::<Epoch>().unwrap_err()), "identity");
	assert_eq!(code(&"2026/09".parse::<Epoch>().unwrap_err()), "identity");
	"anything-else".parse::<Epoch>().unwrap();
}

#[test]
fn path_joins_epoch() {
	let cred = test_credential();
	let epoch = Epoch::mint();
	let path = cred.path("meeting.hang").unwrap();
	assert_eq!(path.as_str().len(), 22);
	let full = path.join(epoch.as_str());
	let (opaque, rest) = full.next_part().unwrap();
	assert_eq!(opaque, path.as_str());
	assert_eq!(rest.as_str().parse::<Epoch>().unwrap(), epoch);
}

#[test]
fn grouped_roundtrip() {
	let mut pair = pair("video");
	let mut group = pair.producer.append_group().unwrap();
	assert_eq!(group.sequence(), 0);
	group.write_frame(ms(1), b"frame-zero").unwrap();
	group.write_frame(ms(2), b"frame-one").unwrap();
	assert_eq!(group.next_frame(), 2);
	group.finish().unwrap();

	let mut group = recv_group(&mut pair.consumer);
	assert_eq!(group.sequence(), 0);
	assert_eq!(read_frame(&mut group).plaintext, &b"frame-zero"[..]);
	assert_eq!(read_frame(&mut group).plaintext, &b"frame-one"[..]);
	assert!(matches!(
		group.poll_read_frame(&kio::Waiter::noop()),
		Poll::Ready(Ok(None))
	));
}

#[test]
fn datagram_roundtrip() {
	let mut pair = pair("audio");
	assert_eq!(pair.producer.append_datagram(ms(5), b"opus").unwrap(), 0);
	assert_eq!(pair.producer.datagram_invocations(), 1);
	match recv_datagram(&mut pair.consumer) {
		Event::Datagram(d) => {
			assert_eq!(d.sequence, 0);
			assert_eq!(d.timestamp, ms(5));
			assert_eq!(&d.plaintext[..], b"opus");
		}
		other => panic!("{other:?}"),
	}
}

#[test]
fn sequences_allocate_monotonically() {
	let mut pair = pair("video");
	assert_eq!(pair.producer.append_group().unwrap().sequence(), 0);
	assert_eq!(pair.producer.create_group(5).unwrap().sequence(), 5);
	assert_eq!(pair.producer.append_datagram(ms(1), b"x").unwrap(), 6);
	assert_eq!(pair.producer.append_group().unwrap().sequence(), 7);
	assert_eq!(
		pair.producer.create_group(7).map(|_| ()).unwrap_err().to_string(),
		"reuse"
	);
	assert_eq!(
		pair.producer.create_group(3).map(|_| ()).unwrap_err().to_string(),
		"reuse"
	);
	assert_eq!(
		pair.producer.insert_datagram(6, ms(1), b"x").unwrap_err().to_string(),
		"reuse"
	);
	assert_eq!(
		pair.producer.datagram_invocations(),
		1,
		"reuse is refused before encryption"
	);
	assert_eq!(
		pair.producer
			.create_group(MAX_U53 + 1)
			.map(|_| ())
			.unwrap_err()
			.to_string(),
		"identity"
	);
}

#[test]
fn oversize_datagram_keeps_the_sequence() {
	let mut pair = pair("audio");
	let big = vec![0u8; MAX_DATAGRAM_PLAINTEXT + 1];
	assert_eq!(
		pair.producer.append_datagram(ms(1), &big).unwrap_err().to_string(),
		"oversize"
	);
	assert_eq!(pair.producer.datagram_invocations(), 0);
	let max = vec![0u8; MAX_DATAGRAM_PLAINTEXT];
	assert_eq!(pair.producer.append_datagram(ms(1), &max).unwrap(), 0);
}

#[test]
fn track_produced_once_per_generation() {
	let generation = test_generation();
	let name = generation.name("video").unwrap();
	let net = net_track(&name);
	let producer = generation.produce(net.clone()).unwrap();
	assert_eq!(producer.name(), name.as_str());
	drop(producer);
	let again = generation.clone();
	assert_eq!(again.produce(net.clone()).map(|_| ()).unwrap_err().to_string(), "reuse");
	// A new epoch is a new publisher instance.
	let fresh = generation.credential().generation(Epoch::mint());
	fresh.produce(net_track(&fresh.name("video").unwrap())).unwrap();
	// Consuming never claims.
	generation.consume(net.subscribe(subscribe_all())).unwrap();
	generation.consume(net.subscribe(subscribe_all())).unwrap();
}

#[test]
fn wrong_track_name_is_identity() {
	let generation = test_generation();
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track("not-a-physical-name", None)
		.unwrap();
	assert_eq!(
		generation.produce(net.clone()).map(|_| ()).unwrap_err().to_string(),
		"identity"
	);
	assert_eq!(
		generation
			.consume(net.subscribe(subscribe_all()))
			.map(|_| ())
			.unwrap_err()
			.to_string(),
		"identity"
	);
}

#[test]
fn new_epoch_authenticates_old_does_not() {
	let cred = test_credential();
	let old = cred.generation("0199b7f4-3c2a-7d1e-9f0b-2b6c1a9d8e7f".parse().unwrap());
	let new = cred.generation("0199b7f4-9e10-7f42-8a3c-5d7e2b1c0f9a".parse().unwrap());
	assert_ne!(old.name("video").unwrap(), new.name("video").unwrap());

	let net = net_track(&new.name("video").unwrap());
	let mut producer = new.produce(net.clone()).unwrap();
	let mut group = producer.append_group().unwrap();
	group.write_frame(ms(1), b"fresh").unwrap();
	group.finish().unwrap();

	let mut consumer = new.clone().consume(net.subscribe(subscribe_all())).unwrap();
	assert_eq!(read_frame(&mut recv_group(&mut consumer)).plaintext, &b"fresh"[..]);

	// The old generation derives a different key for the same physical name.
	let mut stale = old.consume(net.subscribe(subscribe_all())).unwrap();
	let mut group = recv_group(&mut stale);
	assert_eq!(
		group
			.poll_read_frame(&kio::Waiter::noop())
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);
}

#[test]
fn invocation_exhaustion() {
	let generation = test_generation();
	let mut key = generation
		.key(&generation.name("video").unwrap(), Domain::Group)
		.unwrap();
	key.set_usage(MAX_INVOCATIONS - 1, 0);
	key.protect(0, 0, b"last", MAX_GROUPED_PAYLOAD).unwrap();
	assert_eq!(
		key.protect(0, 1, b"nope", MAX_GROUPED_PAYLOAD).unwrap_err().to_string(),
		"exhausted"
	);
}

#[test]
fn plaintext_byte_exhaustion() {
	let generation = test_generation();
	let mut key = generation
		.key(&generation.name("video").unwrap(), Domain::Group)
		.unwrap();
	key.set_usage(0, MAX_PLAINTEXT_BYTES - 3);
	key.protect(0, 0, b"ab", MAX_GROUPED_PAYLOAD).unwrap();
	assert_eq!(
		key.protect(0, 1, b"abcd", MAX_GROUPED_PAYLOAD).unwrap_err().to_string(),
		"exhausted"
	);
}

#[test]
fn failed_opens_spend_the_key() {
	let generation = test_generation();
	let name = generation.name("video").unwrap();
	let mut key = generation.key(&name, Domain::Group).unwrap();
	let mut good = key.protect(0, 0, b"ok", MAX_GROUPED_PAYLOAD).unwrap().to_vec();
	let mut forged = good.clone();
	forged[0] ^= 1;
	key.set_usage(MAX_INVOCATIONS - 1, 0);
	assert_eq!(
		key.open(0, 0, &forged, MAX_GROUPED_PAYLOAD).unwrap_err().to_string(),
		"authentication"
	);
	assert_eq!(key.invocations(), MAX_INVOCATIONS);
	assert_eq!(
		key.open(0, 0, &good, MAX_GROUPED_PAYLOAD).unwrap_err().to_string(),
		"exhausted"
	);
	// Refusals before AEAD spend nothing.
	let mut key = generation.key(&name, Domain::Group).unwrap();
	assert_eq!(
		key.open(0, 0, &good[..3], MAX_GROUPED_PAYLOAD).unwrap_err().to_string(),
		"oversize"
	);
	assert_eq!(
		key.open(0, u64::from(u32::MAX) + 1, &good, MAX_GROUPED_PAYLOAD)
			.unwrap_err()
			.to_string(),
		"identity"
	);
	assert_eq!(key.invocations(), 0);
	good.clear();
}

#[test]
fn grouped_authentication_ends_track() {
	let mut pair = pair("video");
	let mut group = pair.producer.append_group().unwrap();
	group.write_frame(ms(1), b"ok").unwrap();
	group.finish().unwrap();

	// Tamper with a second group by writing ciphertext directly.
	let name: Name = pair.net.name().parse().unwrap();
	let key = pair.generation.key_bytes(&name, Domain::Group).unwrap();
	let mut payload = protect(&key, 1, 0, b"ok", MAX_GROUPED_PAYLOAD).unwrap().to_vec();
	payload[0] ^= 1;
	let mut bad = pair.net.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
	bad.write_frame(ms(2), payload).unwrap();
	bad.finish().unwrap();

	let waiter = kio::Waiter::noop();
	let mut first = recv_group(&mut pair.consumer);
	assert_eq!(read_frame(&mut first).plaintext, &b"ok"[..]);
	let mut second = recv_group(&mut pair.consumer);
	assert_eq!(
		second.poll_read_frame(&waiter).map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);
	assert_eq!(
		pair.consumer
			.poll_recv_group(&waiter)
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);
	assert_eq!(
		pair.consumer
			.poll_recv_datagram(&waiter)
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);
}

#[test]
fn bad_datagram_is_event() {
	let mut pair = pair("audio");
	let name: Name = pair.net.name().parse().unwrap();
	pair.producer.append_datagram(ms(1), b"one").unwrap();
	let mut forged = datagram_ciphertext(&pair.generation, &name, 1, b"two").to_vec();
	forged[0] ^= 1;
	pair.net.insert_datagram(1, ms(2), forged).unwrap();
	pair.producer.insert_datagram(2, ms(3), b"three").unwrap();

	assert!(matches!(recv_datagram(&mut pair.consumer), Event::Datagram(_)));
	assert_eq!(recv_datagram(&mut pair.consumer), Event::Authentication { sequence: 1 });
	match recv_datagram(&mut pair.consumer) {
		Event::Datagram(d) => assert_eq!(&d.plaintext[..], b"three"),
		other => panic!("{other:?}"),
	}
}

#[test]
fn forged_datagram_does_not_burn_identity() {
	let mut pair = pair("audio");
	let name: Name = pair.net.name().parse().unwrap();
	pair.producer.append_datagram(ms(1), b"one").unwrap();
	let mut forged = datagram_ciphertext(&pair.generation, &name, 1, b"two").to_vec();
	forged[0] ^= 1;
	pair.net.insert_datagram(1, ms(2), forged).unwrap();
	pair.producer.insert_datagram(1, ms(2), b"two").unwrap();

	assert!(matches!(recv_datagram(&mut pair.consumer), Event::Datagram(_)));
	assert_eq!(recv_datagram(&mut pair.consumer), Event::Authentication { sequence: 1 });
	match recv_datagram(&mut pair.consumer) {
		Event::Datagram(d) => {
			assert_eq!(d.sequence, 1);
			assert_eq!(&d.plaintext[..], b"two");
		}
		other => panic!("forged must not burn the identity, got {other:?}"),
	}
}

#[test]
fn duplicate_datagram_is_bounded() {
	let mut pair = pair("audio");
	let name: Name = pair.net.name().parse().unwrap();
	let replay = datagram_ciphertext(&pair.generation, &name, 0, b"once");
	pair.net.insert_datagram(0, ms(1), replay.clone()).unwrap();
	pair.net.insert_datagram(0, ms(1), replay.clone()).unwrap();
	assert!(matches!(recv_datagram(&mut pair.consumer), Event::Datagram(_)));
	assert_eq!(recv_datagram(&mut pair.consumer), Event::Duplicate { sequence: 0 });

	// Once the window slides past it, the same ciphertext opens again.
	for sequence in 1..=DATAGRAM_WINDOW {
		pair.net
			.insert_datagram(
				sequence,
				ms(sequence),
				datagram_ciphertext(&pair.generation, &name, sequence, b"x"),
			)
			.unwrap();
		assert!(matches!(recv_datagram(&mut pair.consumer), Event::Datagram(_)));
	}
	pair.net.insert_datagram(0, ms(1), replay).unwrap();
	match recv_datagram(&mut pair.consumer) {
		Event::Datagram(d) => assert_eq!(d.sequence, 0),
		other => panic!("{other:?}"),
	}
}

#[test]
fn cancellation_ends_consumer() {
	let mut pair = pair("video");
	let mut group = pair.producer.append_group().unwrap();
	group.write_frame(ms(1), b"x").unwrap();
	group.finish().unwrap();
	pair.producer.finish().unwrap();
	let waiter = kio::Waiter::noop();
	let mut group = recv_group(&mut pair.consumer);
	read_frame(&mut group);
	assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
	assert!(matches!(pair.consumer.poll_recv_group(&waiter), Poll::Ready(Ok(None))));
}

#[test]
fn secrets_stay_out_of_debug() {
	let generation = test_generation();
	let text = format!("{generation:?}");
	assert!(text.contains("<redacted>"));
	assert!(!text.contains(&hex::encode(test_secret())));
	assert!(!text.contains(std::str::from_utf8(&test_secret()).unwrap()));
	let key = generation
		.key(&generation.name("video").unwrap(), Domain::Group)
		.unwrap();
	assert!(format!("{key:?}").contains("<redacted>"));
}

#[test]
fn failed_group_write_does_not_burn_nonce() {
	let mut pair = pair("video");
	let mut group = pair.producer.append_group().unwrap();
	// (2^62-1) seconds overflows conversion to the milli-scale track.
	let huge = moq_net::Timestamp::from_secs((1 << 62) - 1).unwrap();
	assert!(group.write_frame(huge, b"bad").is_err());
	assert_eq!(group.invocations(), 0);
	let big = vec![0u8; MAX_GROUPED_PLAINTEXT + 1];
	assert_eq!(group.write_frame(ms(1), &big).unwrap_err().to_string(), "oversize");
	assert_eq!(group.invocations(), 0);
	group.write_frame(ms(1), b"ok").unwrap();
	assert_eq!(group.invocations(), 1);
	assert_eq!(group.next_frame(), 1);
	group.finish().unwrap();

	let mut group = recv_group(&mut pair.consumer);
	assert_eq!(read_frame(&mut group).plaintext, &b"ok"[..]);
}

#[test]
fn resumed_group_opens_at_transport_index() {
	use std::sync::{Arc, Mutex, atomic::AtomicBool};

	let generation = test_generation();
	let name = generation.name("video").unwrap();
	let net = net_track(&name);
	let mut subscriber = net.subscribe(subscribe_all());
	let mut producer = generation.produce(net).unwrap();

	let mut group = producer.append_group().unwrap();
	group.write_frame(ms(1), b"zero").unwrap();
	group.write_frame(ms(2), b"one").unwrap();
	group.finish().unwrap();

	// Simulate a resume at object 1: skip frame 0 on the net cursor before wrapping.
	let Poll::Ready(Ok(Some(mut net_group))) = subscriber.poll_recv_group(&kio::Waiter::noop()) else {
		panic!("net group not ready");
	};
	net_group.skip_to(1);
	let key = Arc::new(Mutex::new(generation.key(&name, Domain::Group).unwrap()));
	let mut group = group::Consumer::new(net_group, key, Arc::new(AtomicBool::new(false)));
	assert_eq!(read_frame(&mut group).plaintext, &b"one"[..]);
}
