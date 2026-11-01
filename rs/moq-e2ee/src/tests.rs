use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use rind::{aead, hmac};
use serde_json::Value;

use crate::credential::{Config, Credential};
use crate::datagram::Event;
use crate::error::Error;
use crate::generation::{Consumer, Domain, Producer, name_key_info, name_mac_info};
use crate::limits::{
	DATAGRAM_WINDOW, KEY_LABEL, MAX_DATAGRAM_BODY, MAX_DATAGRAM_HEADER, MAX_DATAGRAM_PAYLOAD, MAX_DATAGRAM_PLAINTEXT,
	MAX_GROUPED_PAYLOAD, MAX_GROUPED_PLAINTEXT, MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, MAX_U53, NAME_BLOCK,
	NAME_KEY_LABEL, NAME_MAC_LABEL, NAME_NONCE_LEN, PATH_LABEL, PATH_LEN, PROFILE, SECRET_LEN, TAG_LEN,
};
use crate::name::{Name, pad};
use crate::protect::{nonce, protect};
use crate::{group, track};
use moq_net::Epoch;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Raw lengths of the derived AES-128-GCM and HMAC-SHA-256 keys, which the vectors pin.
const KEY_LEN: usize = 16;
const NAME_MAC_LEN: usize = 32;

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

fn epoch_from(row: &Value) -> Result<Epoch, String> {
	row["epoch"]
		.as_str()
		.unwrap_or("")
		.parse()
		.map_err(|_| "identity".to_string())
}

fn generation_from(row: &Value) -> Result<Consumer, String> {
	Ok(credential_from(row)?.consume(epoch_from(row)?))
}

/// A producing generation under a vector's epoch, which only tests may choose.
fn producer_from(row: &Value) -> Producer {
	Producer::new(credential_from(row).unwrap(), epoch_from(row).unwrap())
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

/// The raw track key bytes the vectors pin; production only derives the expanded key.
fn key_bytes(generation: &Consumer, semantic: &str, domain: Domain) -> [u8; KEY_LEN] {
	generation
		.credential()
		.expand(&generation.key_info(semantic, domain).unwrap())
}

fn semantic_from(row: &Value) -> String {
	let bytes = unhex(row["semantic_name_bytes"].as_str().unwrap());
	assert_eq!(bytes, row["semantic_name"].as_str().unwrap().as_bytes());
	String::from_utf8(bytes).unwrap()
}

#[test]
fn constants_match_vectors() {
	let v = vectors();
	assert_eq!(v["profile"], PROFILE);
	let c = &v["constants"];
	assert_eq!(c["salt"], hex::encode(PROFILE));
	assert_eq!(c["path_label"], hex::encode(PATH_LABEL));
	assert_eq!(c["name_mac_label"], hex::encode(NAME_MAC_LABEL));
	assert_eq!(c["name_key_label"], hex::encode(NAME_KEY_LABEL));
	assert_eq!(c["key_label"], hex::encode(KEY_LABEL));
	assert_eq!(c["domain_group"], Domain::Group as u8);
	assert_eq!(c["domain_datagram"], Domain::Datagram as u8);
	assert_eq!(c["tag_len"], TAG_LEN);
	assert_eq!(c["key_len"], KEY_LEN);
	assert_eq!(KEY_LEN, aead::AES_128_GCM.key_len());
	assert_eq!(c["path_len"], PATH_LEN);
	assert_eq!(c["name_mac_len"], NAME_MAC_LEN);
	assert_eq!(NAME_MAC_LEN, hmac::HMAC_SHA256.tag_len());
	assert_eq!(c["name_nonce_len"], NAME_NONCE_LEN);
	assert_eq!(c["name_block"], NAME_BLOCK);
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
		let semantic = semantic_from(row);
		assert_eq!(hex::encode(cred.prk_bytes()), row["prk"], "{}", row["id"]);
		assert_eq!(hex::encode(cred.path_info(&semantic).unwrap()), row["path_info"]);
		let path = cred.path(&semantic).unwrap();
		assert_eq!(path.as_str(), row["opaque"]);
		assert_eq!(
			hex::encode(URL_SAFE_NO_PAD.decode(path.as_str()).unwrap()),
			row["path_material"]
		);
	}
}

#[test]
fn derivation_vectors() {
	for row in vectors()["derivation"].as_array().unwrap() {
		let id = &row["id"];
		let generation = generation_from(row).unwrap();
		let (cred, epoch) = (generation.credential(), generation.epoch());
		let semantic = semantic_from(row);
		assert_eq!(hex::encode(cred.prk_bytes()), row["prk"], "{id}");
		let mac_info = name_mac_info(cred, epoch);
		assert_eq!(hex::encode(&mac_info), row["name_mac_info"], "{id}");
		assert_eq!(
			hex::encode(cred.expand::<NAME_MAC_LEN>(&mac_info)),
			row["name_mac"],
			"{id}"
		);
		let key_info = name_key_info(cred, epoch);
		assert_eq!(hex::encode(&key_info), row["name_key_info"], "{id}");
		assert_eq!(hex::encode(cred.expand::<KEY_LEN>(&key_info)), row["name_key"], "{id}");
		assert_eq!(hex::encode(pad(semantic.as_bytes()).unwrap()), row["padded"], "{id}");
		let name = generation.name(&semantic).unwrap();
		assert_eq!(name.as_str(), row["physical_name"], "{id}");
		let material = URL_SAFE_NO_PAD.decode(name.as_str()).unwrap();
		assert_eq!(hex::encode(&material[..NAME_NONCE_LEN]), row["name_nonce"], "{id}");
		let domain = domain_from(row);
		assert_eq!(
			hex::encode(generation.key_info(&semantic, domain).unwrap()),
			row["key_info"]
		);
		assert_eq!(hex::encode(key_bytes(&generation, &semantic, domain)), row["key"]);
	}
}

#[test]
fn naming_vectors() {
	let v = vectors();
	for row in v["naming"].as_array().unwrap() {
		let id = &row["id"];
		let semantic = semantic_from(row);
		let mut generation = v["generation"].clone();
		generation["epoch"] = row["epoch"].clone();
		let producer = producer_from(&generation);
		assert_eq!(hex::encode(pad(semantic.as_bytes()).unwrap()), row["padded"], "{id}");
		let name = producer.name(&semantic).unwrap();
		assert_eq!(name.as_str(), row["physical_name"], "{id}");
		// The producer recovers the semantic name from the physical one alone.
		let parsed: Name = row["physical_name"].as_str().unwrap().parse().unwrap();
		assert_eq!(producer.open(&parsed).unwrap(), semantic, "{id}");
	}
}

fn payload_row(row: &Value) {
	let generation = generation_from(row).unwrap();
	let semantic = row["semantic_name"].as_str().unwrap();
	let name = generation.name(semantic).unwrap();
	assert_eq!(name.as_str(), row["physical_name"]);
	let domain = domain_from(row);
	assert_eq!(hex::encode(key_bytes(&generation, semantic, domain)), row["key"]);
	let mut key = generation.key(semantic, domain).unwrap();
	let group = row["group"].as_u64().unwrap();
	let frame = row["frame"].as_u64().unwrap();
	assert_eq!(hex::encode(nonce(group, frame).unwrap()), row["nonce"]);
	let plaintext = unhex(row["plaintext"].as_str().unwrap());
	let limit = row["payload_limit"].as_u64().unwrap() as usize;
	let payload = key.protect(group, frame, &plaintext, limit).unwrap();
	assert_eq!(hex::encode(&payload), row["payload"], "{}", row["id"]);
	let opened = key.open(group, frame, &payload, limit).unwrap();
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
				let semantic = row["semantic_name"].as_str().unwrap();
				let domain = domain_from(row);
				assert_eq!(hex::encode(key_bytes(&generation, semantic, domain)), row["key"]);
				let mut key = generation.key(semantic, domain).unwrap();
				let group = row["group"].as_u64().unwrap();
				let frame = row["frame"].as_u64().unwrap();
				let payload = unhex(row["payload"].as_str().unwrap());
				let limit = row["payload_limit"].as_u64().unwrap() as usize;
				code(&key.open(group, frame, &payload, limit).unwrap_err())
			}
			"identity" => match (identity_u64(&row["group"]), identity_u64(&row["frame"])) {
				(Ok(g), Ok(f)) => code(&nonce(g, f).err().unwrap_or_else(|| panic!("{id} should fail"))),
				(Err(e), _) | (_, Err(e)) => code(&e),
			},
			"generation" => generation_from(&row["generation"]).map(|_| ()).unwrap_err(),
			"name" => {
				let producer = producer_from(&row["generation"]);
				let opened = row["physical_name"]
					.as_str()
					.unwrap()
					.parse::<Name>()
					.and_then(|name| producer.open(&name));
				code(&opened.unwrap_err())
			}
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
					"epoch" => {
						assert!(String::from_utf8(data).unwrap().parse::<Epoch>().is_err());
						"identity".to_string()
					}
					"semantic_name" => {
						let semantic = String::from_utf8(data).unwrap();
						let generation = cred.produce();
						let name = code(&generation.name(&semantic).unwrap_err());
						assert_eq!(code(&cred.path(&semantic).unwrap_err()), name);
						name
					}
					other => panic!("unknown bytes field {other}"),
				}
			}
			"protect" => {
				let key = aead::Key::new(&aead::AES_128_GCM, &unhex(row["key"].as_str().unwrap())).unwrap();
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

fn test_generation() -> Producer {
	test_credential().produce()
}

fn consume(generation: &Producer) -> Consumer {
	generation.credential().consume(generation.epoch().clone())
}

fn subscribe_all() -> moq_net::track::Subscription {
	moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(60))
}

fn net_track(name: &Name) -> moq_net::track::Producer {
	moq_net::broadcast::Info::new()
		.produce()
		.create_track(name.as_str(), timed())
		.unwrap()
}

struct Pair {
	semantic: String,
	generation: Consumer,
	net: moq_net::track::Producer,
	producer: track::Producer,
	consumer: track::Consumer,
}

fn pair(semantic: &str) -> Pair {
	let generation = test_generation();
	let net = net_track(&generation.name(semantic).unwrap());
	let producer = generation.produce(net.clone(), semantic).unwrap();
	let consuming = consume(&generation);
	let consumer = consuming.consume(net.subscribe(subscribe_all()), semantic).unwrap();
	Pair {
		semantic: semantic.to_string(),
		generation: consuming,
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

fn datagram_ciphertext(generation: &Consumer, semantic: &str, sequence: u64, plaintext: &[u8]) -> Bytes {
	let mut key = generation.key(semantic, Domain::Datagram).unwrap();
	key.protect(sequence, 0, plaintext, MAX_DATAGRAM_PAYLOAD).unwrap()
}

#[test]
fn path_is_opaque() {
	let path = test_credential().path("meeting.hang").unwrap();
	assert_eq!(path.as_str().len(), 22);
	assert!(!path.as_str().contains("meeting"));
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
			assert_eq!(d.timestamp, Some(ms(5)));
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
fn reproduced_track_resumes_above_its_marks() {
	let generation = test_generation();
	let name = generation.name("video").unwrap();
	let net = net_track(&name);
	let mut producer = generation.produce(net.clone(), "video").unwrap();
	assert_eq!(producer.name(), name.as_str());
	producer.create_group(4).unwrap();
	assert_eq!(producer.append_datagram(ms(1), b"x").unwrap(), 5);
	drop(producer);

	// A clone of the generation reopening the name, on any output, resumes above its marks.
	let mut again = generation.clone().produce(net_track(&name), "video").unwrap();
	for sequence in [0, 5] {
		assert_eq!(
			again.create_group(sequence).map(|_| ()).unwrap_err().to_string(),
			"reuse"
		);
		assert_eq!(
			again.insert_datagram(sequence, ms(1), b"x").unwrap_err().to_string(),
			"reuse"
		);
	}
	assert_eq!(again.append_group().unwrap().sequence(), 6);
	// Two live producers of the name share one sequence namespace.
	let mut other = generation.produce(net_track(&name), "video").unwrap();
	assert_eq!(other.append_datagram(ms(1), b"x").unwrap(), 7);
	assert_eq!(again.append_group().unwrap().sequence(), 8);

	// A new epoch is a new publisher instance.
	let fresh = generation.credential().produce();
	let mut fresh = fresh
		.produce(net_track(&fresh.name("video").unwrap()), "video")
		.unwrap();
	assert_eq!(fresh.append_group().unwrap().sequence(), 0);
	// Consuming never claims.
	consume(&generation)
		.consume(net.subscribe(subscribe_all()), "video")
		.unwrap();
	assert_eq!(generation.claims(), 1);
}

#[test]
fn refused_track_is_aborted() {
	let generation = test_generation();
	let net = net_track(&generation.name("video").unwrap());
	let unnameable = "x".repeat(65536);
	assert_eq!(
		generation
			.produce(net.clone(), &unnameable)
			.map(|_| ())
			.unwrap_err()
			.to_string(),
		"identity"
	);
	assert!(matches!(
		net.poll_closed(&kio::Waiter::noop()),
		Poll::Ready(Err(moq_net::Error::Cancel))
	));
}

/// One broadcast seen through the wrappers and, raw, the way a relay sees it.
struct Protected {
	producer: crate::broadcast::Producer,
	consumer: crate::broadcast::Consumer,
	raw: moq_net::broadcast::Consumer,
}

fn protected() -> Protected {
	let generation = test_generation();
	let net = moq_net::broadcast::Info::new().produce();
	Protected {
		consumer: crate::broadcast::Consumer::new(net.consume(), consume(&generation)),
		raw: net.consume(),
		producer: crate::broadcast::Producer::new(net, generation),
	}
}

fn resolve(subscribing: &kio::Pending<track::Subscribing>) -> Result<track::Consumer, Error> {
	match subscribing.poll_ok(&kio::Waiter::noop()) {
		Poll::Ready(res) => res,
		Poll::Pending => panic!("subscription still pending"),
	}
}

#[test]
fn broadcast_speaks_semantic_names() {
	let protected = protected();
	let mut video = protected.producer.create_track("video", timed()).unwrap();
	let physical = protected.producer.generation().name("video").unwrap();
	assert_eq!(video.name(), physical.as_str());

	let mut reader = resolve(&protected.consumer.subscribe("video", subscribe_all()).unwrap()).unwrap();
	let mut group = video.append_group().unwrap();
	group.write_frame(ms(1), b"frame").unwrap();
	group.finish().unwrap();
	assert_eq!(read_frame(&mut recv_group(&mut reader)).plaintext, &b"frame"[..]);

	// The relay's view holds only the physical name.
	assert!(protected.raw.track(physical.as_str()).is_ok());
	assert!(matches!(protected.raw.track("video"), Err(moq_net::Error::NotFound)));
	// A track nobody created fails like any missing track.
	assert!(matches!(
		protected.consumer.subscribe("audio", subscribe_all()),
		Err(Error::Net(moq_net::Error::NotFound))
	));
}

#[test]
fn reserved_track_serves_on_demand() {
	let protected = protected();
	let request = protected.producer.reserve_track("audio").unwrap();
	assert_eq!(request.name(), "audio");
	let demand = request.demand();
	assert!(!demand.wanted().is_some());

	let subscribing = protected.consumer.subscribe("audio", subscribe_all()).unwrap();
	assert!(subscribing.poll_ok(&kio::Waiter::noop()).is_pending());
	assert!(demand.wanted().is_some());

	let mut audio = request.accept(timed());
	let mut reader = resolve(&subscribing).unwrap();
	audio.append_datagram(ms(1), b"opus").unwrap();
	match recv_datagram(&mut reader) {
		Event::Datagram(d) => assert_eq!(&d.plaintext[..], b"opus"),
		other => panic!("{other:?}"),
	}
}

#[test]
fn broadcast_claims_span_outputs() {
	let generation = test_generation();
	let first = crate::broadcast::Producer::new(moq_net::broadcast::Info::new().produce(), generation.clone());
	let mut video = first.create_track("video", timed()).unwrap();
	video.create_group(2).unwrap();
	let mut audio = first.reserve_track("audio").unwrap().accept(timed());
	audio.append_datagram(ms(1), b"opus").unwrap();

	// A clone of the generation on another output resumes either name above its marks.
	let second = crate::broadcast::Producer::new(moq_net::broadcast::Info::new().produce(), generation);
	let mut video = second.create_track("video", timed()).unwrap();
	assert_eq!(video.append_group().unwrap().sequence(), 3);
	let request = second.reserve_track("audio").unwrap();
	assert_eq!(request.name(), "audio");
	assert_eq!(request.accept(timed()).append_datagram(ms(1), b"opus").unwrap(), 1);
}

#[test]
fn recovery_claims_nothing() {
	let generation = test_generation();
	let name = generation.name("video").unwrap();
	assert_eq!(generation.open(&name).unwrap(), "video");
	// Opening a name neither claims it nor derives its payload keys.
	assert_eq!(generation.claims(), 0);
	// The same semantic name under another epoch of the same credential does not open.
	let other = test_credential().produce().name("video").unwrap();
	assert_eq!(generation.open(&other).unwrap_err().to_string(), "authentication");
}

/// Poll a handler once, expecting a request.
fn requested(requests: &mut crate::broadcast::Requests) -> track::Request {
	match requests.poll_requested_track(&kio::Waiter::noop()) {
		Poll::Ready(Ok(request)) => request,
		other => panic!("expected a request, got {other:?}"),
	}
}

/// Subscribe to `name` the way a relay forwards it, and poll once so it reaches the handler.
fn subscribe_raw(raw: &moq_net::broadcast::Consumer, name: &str) -> kio::Pending<moq_net::track::Subscribing> {
	let subscribing = raw.track(name).unwrap().subscribe(subscribe_all());
	let _ = subscribing.poll_ok(&kio::Waiter::noop());
	subscribing
}

#[test]
fn requests_serve_undeclared_names() {
	let protected = protected();
	let mut requests = protected.producer.requests();
	assert!(requests.poll_requested_track(&kio::Waiter::noop()).is_pending());

	let subscribing = protected.consumer.subscribe("stats/viewers", subscribe_all()).unwrap();
	assert!(subscribing.poll_ok(&kio::Waiter::noop()).is_pending());
	let request = requested(&mut requests);
	assert_eq!(request.name(), "stats/viewers");
	assert!(request.demand().wanted().is_some());
	assert_eq!(protected.producer.generation().claims(), 0);

	let mut track = request.accept(timed());
	assert_eq!(protected.producer.generation().claims(), 1);
	let mut reader = resolve(&subscribing).unwrap();
	let mut group = track.append_group().unwrap();
	group.write_frame(ms(1), b"42").unwrap();
	group.finish().unwrap();
	assert_eq!(read_frame(&mut recv_group(&mut reader)).plaintext, &b"42"[..]);

	// The handler's own consumer reads the broadcast under semantic names too.
	let mut local = resolve(&requests.consume().subscribe("stats/viewers", subscribe_all()).unwrap()).unwrap();
	assert_eq!(read_frame(&mut recv_group(&mut local)).plaintext, &b"42"[..]);
}

#[test]
fn requests_refuse_names_the_generation_did_not_issue() {
	let protected = protected();
	let mut requests = protected.producer.requests();
	let generation = protected.producer.generation();

	let older = test_credential().produce().name("video").unwrap();
	let forged = URL_SAFE_NO_PAD.encode([0u8; NAME_NONCE_LEN + NAME_BLOCK + TAG_LEN]);
	for name in [older.as_str(), forged.as_str(), "video", "!!", ""] {
		let subscribing = subscribe_raw(&protected.raw, name);
		assert!(
			requests.poll_requested_track(&kio::Waiter::noop()).is_pending(),
			"{name:?} reached the handler"
		);
		assert!(
			matches!(
				subscribing.poll_ok(&kio::Waiter::noop()),
				Poll::Ready(Err(moq_net::Error::NotFound))
			),
			"{name:?} was not answered as missing"
		);
	}
	assert_eq!(generation.claims(), 0);

	// A burst of forged names leaves nothing behind, and a real request still gets through.
	let burst: Vec<_> = (0..256u32)
		.map(|i| {
			let mut material = vec![0u8; NAME_NONCE_LEN + NAME_BLOCK + TAG_LEN];
			material[..4].copy_from_slice(&i.to_be_bytes());
			subscribe_raw(&protected.raw, &URL_SAFE_NO_PAD.encode(material))
		})
		.collect();
	let _video = protected.consumer.subscribe("video", subscribe_all()).unwrap();
	assert_eq!(requested(&mut requests).name(), "video");
	assert!(requests.poll_requested_track(&kio::Waiter::noop()).is_pending());
	for subscribing in &burst {
		assert!(matches!(
			subscribing.poll_ok(&kio::Waiter::noop()),
			Poll::Ready(Err(moq_net::Error::NotFound))
		));
	}
	// Not accepted, so not claimed either.
	assert_eq!(generation.claims(), 0);
}

#[test]
fn requests_claim_only_accepted_names() {
	let protected = protected();
	let mut requests = protected.producer.requests();
	let generation = protected.producer.generation();

	// A credential holder can name any track: rejecting or dropping its requests keeps nothing.
	for i in 0..256 {
		let name = generation.name(&format!("unknown/{i}")).unwrap();
		let subscribing = subscribe_raw(&protected.raw, name.as_str());
		let request = requested(&mut requests);
		assert_eq!(request.name(), format!("unknown/{i}"));
		if i % 2 == 0 {
			request.reject(moq_net::Error::NotFound);
		} else {
			drop(request);
		}
		assert!(subscribing.poll_ok(&kio::Waiter::noop()).is_ready());
	}
	assert_eq!(generation.claims(), 0);

	// A name claimed while its request waits resumes that claim once accepted.
	let subscribing = protected.consumer.subscribe("video", subscribe_all()).unwrap();
	let _ = subscribing.poll_ok(&kio::Waiter::noop());
	let request = requested(&mut requests);
	assert_eq!(generation.claims(), 0);
	let other = crate::broadcast::Producer::new(moq_net::broadcast::Info::new().produce(), generation.clone());
	other.create_track("video", timed()).unwrap().create_group(5).unwrap();
	let mut video = request.accept(timed());
	assert_eq!(generation.claims(), 1);
	assert_eq!(video.append_group().unwrap().sequence(), 6);
}

#[test]
fn requests_serve_a_dropped_name_above_its_marks() {
	let protected = protected();
	let mut requests = protected.producer.requests();

	let subscribing = protected.consumer.subscribe("presence", subscribe_all()).unwrap();
	let _ = subscribing.poll_ok(&kio::Waiter::noop());
	let mut track = requested(&mut requests).accept(timed());
	let reader = resolve(&subscribing).unwrap();
	track.create_group(3).unwrap();
	// The handler drops the idle track.
	drop(reader);
	drop(subscribing);
	drop(track);

	let subscribing = protected.consumer.subscribe("presence", subscribe_all()).unwrap();
	let _ = subscribing.poll_ok(&kio::Waiter::noop());
	let request = requested(&mut requests);
	assert_eq!(request.name(), "presence");
	let mut track = request.accept(timed());
	assert_eq!(track.create_group(3).map(|_| ()).unwrap_err().to_string(), "reuse");
	assert_eq!(track.append_group().unwrap().sequence(), 4);
	let mut reader = resolve(&subscribing).unwrap();
	assert_eq!(recv_group(&mut reader).sequence(), 4);
}

#[test]
fn lister_lists_physical_names() {
	let protected = protected();
	let requests = protected.producer.requests();
	let lister = requests.lister().unwrap();
	assert!(matches!(requests.lister(), Err(Error::Net(moq_net::Error::Duplicate))));

	let mut tracks = protected.raw.tracks().unwrap();
	assert!(lister.poll_wanted(&kio::Waiter::noop()).is_ready());
	lister.add("stats/viewers", timed()).unwrap();
	lister.synced();
	let next = |tracks: &mut moq_net::broadcast::Tracks| match tracks.poll_next(&kio::Waiter::noop()) {
		Poll::Ready(Ok(Some(event))) => event,
		other => panic!("expected a listing event, got {other:?}"),
	};
	let moq_net::broadcast::TracksEvent::Added { name, .. } = next(&mut tracks) else {
		panic!("expected the listed track");
	};
	// A listed name round-trips to its semantic name.
	let physical: Name = name.parse().unwrap();
	assert_eq!(
		protected.producer.generation().open(&physical).unwrap(),
		"stats/viewers"
	);
	assert!(matches!(next(&mut tracks), moq_net::broadcast::TracksEvent::Synced));

	lister.finish("stats/viewers").unwrap();
	assert!(matches!(
		next(&mut tracks),
		moq_net::broadcast::TracksEvent::Finished { name: finished } if finished == name
	));
	assert!(matches!(
		lister.remove("stats/viewers"),
		Err(Error::Net(moq_net::Error::NotFound))
	));
	// Listing claims nothing: only serving a name does.
	assert_eq!(protected.producer.generation().claims(), 0);
}

#[test]
fn new_epoch_authenticates_old_does_not() {
	let cred = test_credential();
	let old = cred.consume(cred.produce().epoch().clone());
	let new = cred.produce();
	assert_ne!(old.name("video").unwrap(), new.name("video").unwrap());

	let net = net_track(&new.name("video").unwrap());
	let mut producer = new.produce(net.clone(), "video").unwrap();
	let mut group = producer.append_group().unwrap();
	group.write_frame(ms(1), b"fresh").unwrap();
	group.finish().unwrap();

	let mut consumer = consume(&new).consume(net.subscribe(subscribe_all()), "video").unwrap();
	assert_eq!(read_frame(&mut recv_group(&mut consumer)).plaintext, &b"fresh"[..]);

	// The old generation derives a different key for the same semantic name.
	let mut stale = old.consume(net.subscribe(subscribe_all()), "video").unwrap();
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
	let mut key = consume(&generation).key("video", Domain::Group).unwrap();
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
	let mut key = consume(&generation).key("video", Domain::Group).unwrap();
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
	let mut key = consume(&generation).key("video", Domain::Group).unwrap();
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
	let mut key = consume(&generation).key("video", Domain::Group).unwrap();
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
	let semantic = pair.semantic.as_str();
	let mut key = pair.generation.key(semantic, Domain::Group).unwrap();
	let mut payload = key.protect(1, 0, b"ok", MAX_GROUPED_PAYLOAD).unwrap().to_vec();
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

/// Wakes when a parked read is notified. One flag per parked task, so a sibling
/// that happens to be polled again cannot hide a missing wake.
struct WakeFlag(AtomicBool);

impl Wake for WakeFlag {
	fn wake(self: Arc<Self>) {
		self.0.store(true, Ordering::SeqCst);
	}
}

struct Parked {
	flag: Arc<WakeFlag>,
	waker: Waker,
	park: kio::Park,
}

impl Parked {
	fn new() -> Self {
		let flag = Arc::new(WakeFlag(AtomicBool::new(false)));
		let waker = Waker::from(Arc::clone(&flag));
		Self {
			flag,
			waker,
			park: kio::Park::default(),
		}
	}

	fn poll<T>(&mut self, poll: impl FnOnce(&kio::Waiter) -> Poll<T>) -> Poll<T> {
		let cx = Context::from_waker(&self.waker);
		let waiter = self.park.hold(&cx);
		poll(waiter)
	}

	fn woken(&self) -> bool {
		self.flag.0.load(Ordering::SeqCst)
	}
}

#[test]
fn grouped_failure_wakes_pending_reads_and_releases_demand() {
	let mut pair = pair("video");
	let demand = pair.net.demand();
	assert!(demand.wanted().is_some());

	// One authentic frame, then silence. The group and the track stay open.
	let semantic = pair.semantic.as_str();
	let mut key = pair.generation.key(semantic, Domain::Group).unwrap();
	let mut open = pair.net.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
	let open_demand = open.demand();
	open.write_frame(ms(1), key.protect(0, 0, b"ok", MAX_GROUPED_PAYLOAD).unwrap())
		.unwrap();

	let mut payload = key.protect(1, 0, b"nope", MAX_GROUPED_PAYLOAD).unwrap().to_vec();
	payload[0] ^= 1;
	let mut bad = pair.net.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
	let bad_demand = bad.demand();
	bad.write_frame(ms(2), payload).unwrap();

	let mut good = recv_group(&mut pair.consumer);
	assert_eq!(read_frame(&mut good).plaintext, &b"ok"[..]);
	let mut forged = recv_group(&mut pair.consumer);
	assert!(bad_demand.wanted());

	// Sibling frame, next group, and datagram reads are already parked. Nothing
	// further is published.
	let mut frame = Parked::new();
	assert!(matches!(
		frame.poll(|waiter| good.poll_read_frame(waiter)),
		Poll::Pending
	));
	let mut track = Parked::new();
	assert!(matches!(
		track.poll(|waiter| pair.consumer.poll_recv_group(waiter)),
		Poll::Pending
	));
	assert!(matches!(
		track.poll(|waiter| pair.consumer.poll_recv_datagram(waiter)),
		Poll::Pending
	));

	assert_eq!(
		forged
			.poll_read_frame(&kio::Waiter::noop())
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);

	assert!(frame.woken(), "parked frame read was not woken");
	assert!(track.woken(), "parked group and datagram reads were not woken");
	assert!(!bad_demand.wanted(), "failed group demand stayed live");
	assert!(!pair.net.is_closed(), "publisher must stay open");

	assert_eq!(
		frame
			.poll(|waiter| good.poll_read_frame(waiter))
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);
	assert!(
		!open_demand.wanted(),
		"sibling group demand stayed live after its woken read"
	);
	// The sibling's reader held the track wanted until that read released it.
	assert!(
		!demand.wanted().is_some(),
		"track demand stayed live after authentication failure"
	);
	assert_eq!(
		track
			.poll(|waiter| pair.consumer.poll_recv_group(waiter))
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);
	assert_eq!(
		track
			.poll(|waiter| pair.consumer.poll_recv_datagram(waiter))
			.map(|r| r.unwrap_err().to_string()),
		Poll::Ready("authentication".to_string())
	);

	// The handles that observed the failure are still held.
	drop(good);
	drop(forged);
	drop(open);
}

#[test]
fn a_held_group_keeps_demand_after_the_track_drops() {
	let mut pair = pair("video");
	let demand = pair.net.demand();
	let mut open = pair.producer.append_group().unwrap();
	open.write_frame(ms(1), b"ok").unwrap();

	let mut group = recv_group(&mut pair.consumer);
	drop(pair.consumer);
	assert!(demand.wanted().is_some(), "a held group keeps its track wanted");

	// The held group is not failed by the unsubscribe.
	assert_eq!(read_frame(&mut group).plaintext, &b"ok"[..]);
	open.write_frame(ms(2), b"next").unwrap();
	assert_eq!(read_frame(&mut group).plaintext, &b"next"[..]);

	drop(group);
	assert!(demand.wanted().is_none(), "the last reader leaving releases the track");
}

#[test]
fn bad_datagram_stays_an_event_and_keeps_demand() {
	let mut pair = pair("audio");
	let demand = pair.net.demand();
	let mut open = pair.producer.append_group().unwrap();
	open.write_frame(ms(1), b"ok").unwrap();

	let semantic = pair.semantic.as_str();
	let mut forged = datagram_ciphertext(&pair.generation, semantic, 0, b"nope").to_vec();
	forged[0] ^= 1;
	pair.net.insert_datagram(0, ms(2), forged).unwrap();

	let mut good = recv_group(&mut pair.consumer);
	assert_eq!(read_frame(&mut good).plaintext, &b"ok"[..]);
	let mut frame = Parked::new();
	assert!(matches!(
		frame.poll(|waiter| good.poll_read_frame(waiter)),
		Poll::Pending
	));

	assert_eq!(recv_datagram(&mut pair.consumer), Event::Authentication { sequence: 0 });
	assert!(!frame.woken(), "a bad datagram must not wake grouped reads");
	assert!(matches!(
		frame.poll(|waiter| good.poll_read_frame(waiter)),
		Poll::Pending
	));
	assert!(demand.wanted().is_some());

	pair.producer.insert_datagram(1, ms(3), b"next").unwrap();
	match recv_datagram(&mut pair.consumer) {
		Event::Datagram(d) => assert_eq!(&d.plaintext[..], b"next"),
		other => panic!("{other:?}"),
	}
	assert!(demand.wanted().is_some());
}

#[test]
fn bad_datagram_is_event() {
	let mut pair = pair("audio");
	let semantic = pair.semantic.as_str();
	pair.producer.append_datagram(ms(1), b"one").unwrap();
	let mut forged = datagram_ciphertext(&pair.generation, semantic, 1, b"two").to_vec();
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
fn unsealable_datagram_is_dropped_with_its_code() {
	let mut pair = pair("audio");
	let semantic = pair.semantic.as_str();
	let sealed = datagram_ciphertext(&pair.generation, semantic, 0, b"x");
	pair.net
		.insert_datagram(1, ms(1), vec![0u8; MAX_DATAGRAM_PAYLOAD + 1])
		.unwrap();
	pair.net.insert_datagram(2, ms(2), sealed.slice(..3)).unwrap();
	pair.net.insert_datagram(MAX_U53 + 1, ms(3), sealed.clone()).unwrap();
	pair.net.insert_datagram(MAX_U53 + 2, ms(3), sealed.slice(..3)).unwrap();
	pair.producer.insert_datagram(MAX_U53, ms(4), b"last").unwrap();

	assert_eq!(recv_datagram(&mut pair.consumer), Event::Oversize { sequence: 1 });
	assert_eq!(recv_datagram(&mut pair.consumer), Event::Oversize { sequence: 2 });
	assert_eq!(
		recv_datagram(&mut pair.consumer),
		Event::Identity { sequence: MAX_U53 + 1 }
	);
	// Identity is checked before size, as in every core.
	assert_eq!(
		recv_datagram(&mut pair.consumer),
		Event::Identity { sequence: MAX_U53 + 2 }
	);
	match recv_datagram(&mut pair.consumer) {
		Event::Datagram(d) => assert_eq!(&d.plaintext[..], b"last"),
		other => panic!("{other:?}"),
	}
}

#[test]
fn resubscribing_shares_a_keys_budget() {
	let mut pair = pair("audio");
	let semantic = pair.semantic.as_str();
	pair.producer.append_datagram(ms(1), b"one").unwrap();
	assert!(matches!(recv_datagram(&mut pair.consumer), Event::Datagram(_)));
	drop(pair.consumer);
	let keys = pair.generation.keys(semantic).unwrap();
	assert_eq!(keys.datagram.lock().unwrap().invocations(), 1);
	keys.datagram.lock().unwrap().set_usage(MAX_INVOCATIONS, 0);

	let mut again = pair
		.generation
		.consume(pair.net.subscribe(subscribe_all()), semantic)
		.unwrap();
	pair.producer.append_datagram(ms(2), b"two").unwrap();
	assert!(matches!(
		again.poll_recv_datagram(&kio::Waiter::noop()),
		Poll::Ready(Err(Error::Exhausted))
	));
}

#[test]
fn forged_datagram_does_not_burn_identity() {
	let mut pair = pair("audio");
	let semantic = pair.semantic.as_str();
	pair.producer.append_datagram(ms(1), b"one").unwrap();
	let mut forged = datagram_ciphertext(&pair.generation, semantic, 1, b"two").to_vec();
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
	let semantic = pair.semantic.as_str();
	let replay = datagram_ciphertext(&pair.generation, semantic, 0, b"once");
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
				datagram_ciphertext(&pair.generation, semantic, sequence, b"x"),
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
	let key = consume(&generation).key("video", Domain::Group).unwrap();
	assert!(format!("{key:?}").contains("<redacted>"));
}

#[test]
fn failed_group_write_does_not_burn_nonce() {
	let mut pair = pair("video");
	let mut group = pair.producer.append_group().unwrap();
	// (2^61-1) seconds overflows conversion to the milli-scale track.
	let huge = moq_net::Timestamp::from_secs((1 << 61) - 1).unwrap();
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
fn failed_net_write_after_encryption_aborts_the_group() {
	let mut pair = pair("video");
	let mut first = pair.producer.append_group().unwrap();
	first.write_frame(ms(10), b"start").unwrap();
	first.finish().unwrap();

	// The net track refuses a group start that rewinds past the one before it, only after
	// the frame was encrypted, so the group can no longer line its transport index up with
	// the nonce.
	let mut group = pair.producer.append_group().unwrap();
	let err = group.write_frame(ms(5), b"rewind").unwrap_err();
	assert!(matches!(err, Error::Net(moq_net::Error::TimestampMismatch)), "{err:?}");
	// The track's grouped key: one frame in each group.
	assert_eq!(group.invocations(), 2);
	let after = group.write_frame(ms(20), b"after").unwrap_err();
	assert_eq!(after.to_string(), err.to_string());
	assert_eq!(group.invocations(), 2);
	assert_eq!(group.finish().unwrap_err().to_string(), err.to_string());

	// Subscribers see the group end with the failure instead of waiting on it.
	let mut start = recv_group(&mut pair.consumer);
	assert_eq!(read_frame(&mut start).plaintext, &b"start"[..]);
	let mut aborted = recv_group(&mut pair.consumer);
	assert!(matches!(
		aborted.poll_read_frame(&kio::Waiter::noop()),
		Poll::Ready(Err(Error::Net(moq_net::Error::TimestampMismatch)))
	));
}

#[test]
fn resumed_group_opens_at_transport_index() {
	use std::sync::{Arc, Mutex};

	let generation = test_generation();
	let name = generation.name("video").unwrap();
	let net = net_track(&name);
	let mut subscriber = net.subscribe(subscribe_all());
	let mut producer = generation.produce(net, "video").unwrap();

	let mut group = producer.append_group().unwrap();
	group.write_frame(ms(1), b"zero").unwrap();
	group.write_frame(ms(2), b"one").unwrap();
	group.finish().unwrap();

	// Simulate a resume at object 1: skip frame 0 on the net cursor before wrapping.
	let Poll::Ready(Ok(Some(mut net_group))) = subscriber.poll_recv_group(&kio::Waiter::noop()) else {
		panic!("net group not ready");
	};
	net_group.skip_to(1);
	let key = Arc::new(Mutex::new(consume(&generation).key("video", Domain::Group).unwrap()));
	let mut group = group::Consumer::new(net_group, key, crate::terminal::Terminal::new(None));
	assert_eq!(read_frame(&mut group).plaintext, &b"one"[..]);
}

#[test]
fn credential_clones_mint_fresh_epochs_and_consumers_bind_them() {
	let credential = test_credential();
	let first = credential.produce();
	let second = credential.clone().produce();
	assert_ne!(first.epoch(), second.epoch());
	assert_ne!(first.name("video").unwrap(), second.name("video").unwrap());
	let bound = credential.consume(first.epoch().clone());
	assert_eq!(bound.epoch(), first.epoch());
	assert_eq!(bound.name("video").unwrap(), first.name("video").unwrap());
	assert_eq!(bound.credential().kid(), credential.kid());
}
