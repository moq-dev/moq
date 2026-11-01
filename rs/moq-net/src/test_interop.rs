//! Bytes cross the language boundary to a Bun script under `test/interop`: subscription
//! responses for subscribers on a mock transport, varint encodings, and moq-lite messages.

use crate::coding::{Decoder, Encode, Encoder, Form, varint};
use crate::{ietf, lite};

// Runs a script with a JSON argument, returning its stdout.
pub(crate) fn bun(script: &str, input: serde_json::Value) -> Vec<u8> {
	let output = std::process::Command::new("bun")
		.arg(format!("{}/../../test/interop/{script}", env!("CARGO_MANIFEST_DIR")))
		.arg(input.to_string())
		.output()
		.expect("Bun must be installed for the interop tests");
	assert!(
		output.status.success(),
		"{script} failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	output.stdout
}

pub(crate) fn fin(version: &str, started: bool, clean: bool, responses: Vec<u8>) -> Vec<u8> {
	let input = serde_json::json!({ "version": version, "started": started, "clean": clean, "responses": responses });
	serde_json::from_slice(&bun("bare-fin.ts", input)).expect("JS publisher returned response bytes")
}

/// Rust updates keep the JS subscription alive, cover both unseen groups after narrowing,
/// and fill the largest group's head under the update's ID on draft-22.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn ietf_range_updates_interop() {
	use crate::coding::Decode;
	use ietf::{EndLocation, Filter, Location, Message, RequestId};
	#[derive(serde::Deserialize)]
	struct Served {
		responses: Vec<Vec<u8>>,
		streams: Vec<Vec<u8>>,
	}
	for version in [ietf::Version::Draft18, ietf::Version::Draft20, ietf::Version::Draft22] {
		let draft20 = Filter::is_draft20(version);
		let range = |start, end: Option<u64>| Filter::Absolute {
			start: Location {
				group: start,
				object: 0,
			},
			end: end.map(|group| EndLocation { group, object: None }),
		};
		let mut subscribe = Vec::new();
		let mut w = Encoder::new(&mut subscribe, version.into());
		w.varint(ietf::Subscribe::ID).unwrap();
		ietf::Subscribe {
			request_id: RequestId(0),
			track_namespace: crate::Path::from_static("room"),
			track_name: "video".into(),
			subscriber_priority: 127,
			group_order: ietf::GroupOrder::Ascending,
			filter: if draft20 { range(4, None) } else { Filter::Unfiltered },
			fill: None,
			properties_wanted: false,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			new_group: None,
			max_age: None,
			epoch: None,
		}
		.encode(&mut w, version)
		.unwrap();
		let update = |id, filter, fill| {
			let mut bytes = Vec::new();
			let mut w = Encoder::new(&mut bytes, version.into());
			w.varint(ietf::SubscribeUpdate::ID).unwrap();
			ietf::SubscribeUpdate {
				request_id: RequestId(id),
				subscriber_priority: None,
				forward: None,
				filter: Some(filter),
				authorization_token: None,
				new_group: None,
				max_age: None,
				fill,
			}
			.encode(&mut w, version)
			.unwrap();
			bytes
		};
		let mut updates = Vec::new();
		if draft20 {
			updates.push(update(
				2,
				range(3, None),
				Some(ietf::Fill {
					group_order: Some(ietf::GroupOrder::Ascending),
					filter: Some(range(3, None)),
					range_filters: false,
				}),
			));
		} else {
			updates.push(update(2, range(10, None), None));
		}
		updates.push(update(4, range(6, Some(7)), None));
		let input = serde_json::json!({
			"draft": crate::Version::from(version).code() & 0xff,
			"subscribe": subscribe,
			"updates": updates,
		});
		let served: Served = serde_json::from_slice(&bun("ietf-range-updates.ts", input)).unwrap();
		assert_eq!(served.responses.len(), 3);
		for (index, bytes) in served.responses.iter().enumerate() {
			let mut r = Decoder::new(bytes, version.into());
			if index == 0 {
				assert_eq!(r.varint().unwrap(), ietf::SubscribeOk::ID);
				let ok = ietf::SubscribeOk::decode(&mut r, version).unwrap();
				assert_eq!(ok.track_alias, 0);
				assert_eq!(ok.largest, draft20.then_some(Location { group: 3, object: 1 }));
			} else {
				assert_eq!(r.varint().unwrap(), ietf::RequestOk::ID);
				let ok = ietf::RequestOk::decode(&mut r, version).unwrap();
				assert_eq!(
					ok.largest,
					if draft20 {
						Some(Location {
							group: index as u64 + 2,
							object: 1,
						})
					} else {
						(index == 2).then_some(Location { group: 8, object: 1 })
					}
				);
			}
			assert!(r.is_empty());
		}
		let mut groups = Vec::new();
		let mut filled = Vec::new();
		for bytes in &served.streams {
			let mut r = Decoder::new(bytes, version.into());
			if r.varint().unwrap() == ietf::FetchHeader::TYPE {
				assert!(draft20);
				assert_eq!(
					ietf::FetchHeader::decode(&mut r, version).unwrap().request_id,
					RequestId(2)
				);
				let mut prior: Option<(u64, u64)> = None;
				while !r.is_empty() {
					let ietf::FetchObject::Object { group, object, .. } =
						ietf::FetchObject::decode(&mut r, version).unwrap()
					else {
						panic!("a complete cached fill must contain objects");
					};
					let location = match (prior, group) {
						(None, Some(group)) => (group, object.unwrap()),
						(Some((prior, _)), Some(delta)) => (prior + delta + 1, object.unwrap()),
						(Some((group, prior)), None) => (group, prior + object.unwrap_or(1)),
						_ => panic!("fill omitted its first group"),
					};
					let len = r.varint().unwrap() as usize;
					assert_eq!(r.slice(len).unwrap(), [location.0 as u8, location.1 as u8]);
					filled.push(location);
					prior = Some(location);
				}
			} else {
				let mut r = Decoder::new(bytes, version.into());
				let header = ietf::GroupHeader::decode(&mut r, version).unwrap();
				assert_eq!(header.track_alias, 0, "updates must retain the subscription alias");
				assert!(header.flags.has_end, "a complete group must end");
				let mut objects = Vec::new();
				while !r.is_empty() {
					let delta = r.varint().unwrap();
					let object = objects.last().map_or(delta, |prior| prior + delta + 1);
					if header.flags.has_extensions {
						let len = r.varint().unwrap() as usize;
						r.slice(len).unwrap();
					}
					let len = r.varint().unwrap() as usize;
					assert_eq!(r.slice(len).unwrap(), [header.group_id as u8, object as u8]);
					objects.push(object);
				}
				groups.push((header.group_id, objects));
			}
		}
		groups.sort_unstable();
		assert_eq!(
			groups,
			if draft20 {
				vec![(3, vec![]), (4, vec![0, 1]), (6, vec![0, 1]), (7, vec![0, 1])]
			} else {
				vec![(6, vec![0, 1]), (7, vec![0, 1])]
			},
			"{version}"
		);
		assert_eq!(
			filled,
			if draft20 {
				(3..4).flat_map(|group| [(group, 0), (group, 1)]).collect::<Vec<_>>()
			} else {
				vec![]
			},
			"{version}"
		);
	}
}

/// Every varint size boundary in both formats, plus the 2^53 edge of a JS `number`. js/net's own
/// tests cover leading-ones values past 2^62 - 1, where the QUIC form stops.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn varint_interop() {
	let mut values = vec![0u64];
	for bits in [6, 7, 14, 21, 28, 30, 32, 35, 42, 49, 53, 56] {
		values.extend([(1 << bits) - 1, 1 << bits]);
	}
	values.push(varint::MAX_QUIC);

	let quic = Form::Quic;
	let leading_ones = Form::from(ietf::Version::Draft18);
	let encode = |v: u64, form: Form| {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, form).varint(v).unwrap();
		buf
	};
	let decode = |buf: &[u8], form: Form| {
		let mut decoder = Decoder::new(buf, form);
		let v = decoder.varint().unwrap();
		assert!(decoder.is_empty());
		v
	};

	let input = serde_json::json!({
		"values": values.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"quic": values.iter().map(|&v| encode(v, quic)).collect::<Vec<_>>(),
		"leadingOnes": values.iter().map(|&v| encode(v, leading_ones)).collect::<Vec<_>>(),
	});
	let output: serde_json::Value =
		serde_json::from_slice(&bun("varint.ts", input)).expect("JS returned its encodings");
	let js = |format: &str| -> Vec<Vec<u8>> { serde_json::from_value(output[format].clone()).unwrap() };

	for ((&value, js_quic), js_leading) in values.iter().zip(js("quic")).zip(js("leadingOnes")) {
		assert_eq!(js_quic, encode(value, quic), "QUIC encoding of {value}");
		assert_eq!(
			js_leading,
			encode(value, leading_ones),
			"leading-ones encoding of {value}"
		);
		assert_eq!(decode(&js_quic, quic), value);
		assert_eq!(decode(&js_leading, leading_ones), value);
	}
}

/// Every leading-ones length boundary, plus the JS safe-integer edge and the 62-bit ceiling.
fn lite_values() -> Vec<u64> {
	let mut values = vec![0];
	for bits in [6, 7, 14, 21, 28, 30, 35, 42, 49, 53, 56] {
		values.extend([(1u64 << bits) - 1, 1 << bits]);
	}
	values.push(varint::MAX_QUIC);
	values
}

/// One GROUP header and its frames, as a publisher writes them: a zigzag timestamp delta on
/// a timed track, the payload size, then the payload.
fn lite_group(version: lite::Version, frames: impl IntoIterator<Item = (Option<i64>, usize)>) -> Vec<u8> {
	let mut buf = Vec::new();
	let w = &mut Encoder::new(&mut buf, version.into());
	let header = lite::Group {
		subscribe: 5,
		sequence: 1 << 20,
		frame_start: 200,
	};
	header.encode(w, version).unwrap();
	for (delta, size) in frames {
		if let Some(delta) = delta {
			w.varint(varint::zigzag(delta)).unwrap();
		}
		w.varint(size as u64).unwrap();
		w.slice(&vec![0xAB; size]);
	}
	buf
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Echo {
	values: Vec<String>,
	varints: Vec<u8>,
	setup: Vec<u8>,
	datagram: Vec<u8>,
	padding: Vec<u8>,
	untimed_datagram: Vec<u8>,
	track_info: Vec<u8>,
	group: Vec<u8>,
	untimed_group: Vec<u8>,
	beyond: Vec<u8>,
}

/// JS decodes what Rust encodes back to the same values, and its own encoding of those
/// values is byte for byte Rust's, on lite-06 (QUIC) and lite-07 (leading-ones).
///
/// Past 2^62-1 the range is per version: JS writes lite-07's 64-bit values, which Rust
/// reads back, while on lite-06 JS refuses them.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn lite_varint_interop() {
	let values = lite_values();
	for version in [lite::Version::Lite06, lite::Version::Lite07] {
		let mut varints = Vec::new();
		let w = &mut Encoder::new(&mut varints, version.into());
		for value in &values {
			w.varint(*value).unwrap();
		}

		let setup = lite::Setup {
			hop: Some(crate::Hop::new(varint::MAX_QUIC).unwrap()),
			..Default::default()
		}
		.encode_bytes(version)
		.unwrap();

		let datagram = lite::Datagram {
			subscribe: (1 << 40) + 1,
			sequence: 300,
			timestamp: Some((1 << 50) + 5),
			payload: bytes::Bytes::from_static(b"x"),
			inflated: None,
			ietf: None,
		}
		.encode_bytes(version)
		.unwrap();

		// Only lite-07 can carry an untimed track; lite-06 stamps it, so it has no untimed bytes.
		let untimed = version.wire_timescale(None).is_none();
		let padding = match version.has_padding() {
			true => {
				let mut bytes = Vec::new();
				let mut w = Encoder::new(&mut bytes, version.into());
				w.varint(1).unwrap();
				w.slice(&[0xff; 1199]);
				bytes
			}
			false => Vec::new(),
		};

		// The body doesn't say it is untimed; the subscription's track does.
		let untimed_datagram = lite::Datagram {
			subscribe: 9,
			sequence: 1 << 30,
			timestamp: None,
			payload: bytes::Bytes::from_static(b"untimed"),
			inflated: None,
			ietf: None,
		}
		.encode_bytes(version);
		let untimed_datagram = match untimed {
			true => untimed_datagram.unwrap().to_vec(),
			false => {
				assert!(untimed_datagram.is_err(), "{version}: encoded an untimed datagram");
				Vec::new()
			}
		};

		// An untimed track: zero on lite-07, milliseconds before it. Compressed where the
		// version can say so.
		let track_info = lite::TrackInfo {
			refresh: None,
			priority: 1,
			max_age: None,
			timescale: None,
			compress: version.has_compression(),
		}
		.encode_bytes(version)
		.unwrap();

		let group = lite_group(
			version,
			[
				(Some(0), 10),
				(Some(33_333), 300),
				(Some(-1_000), 20_000),
				(Some(1 << 40), 1),
			],
		);
		// Small, since the whole input rides in one argument.
		let untimed_group = match untimed {
			true => lite_group(version, [(None, 10), (None, 300)]),
			false => Vec::new(),
		};

		let input = serde_json::json!({
			"version": crate::Version::from(version).alpn(),
			"values": values.len(),
			"varints": varints,
			"setup": setup.to_vec(),
			"datagram": datagram.to_vec(),
			"padding": padding,
			"untimedDatagram": untimed_datagram,
			"trackInfo": track_info.to_vec(),
			"group": group,
			"untimedGroup": untimed_group,
		});
		let echo: Echo = serde_json::from_slice(&bun("lite-varint.ts", input)).expect("JS returned its encodings");

		let decoded: Vec<u64> = echo.values.iter().map(|v| v.parse().unwrap()).collect();
		assert_eq!(decoded, values, "{version}: JS decoded different values");
		assert_eq!(echo.varints, varints, "{version}: JS encoded the values differently");
		assert_eq!(echo.setup, setup, "{version}: SETUP");
		assert_eq!(echo.datagram, datagram, "{version}: datagram");
		assert_eq!(echo.padding, padding, "{version}: padding");
		if version.has_padding() {
			assert!(
				lite::Datagram::decode(echo.padding.into(), version, false, |_| {
					panic!("padding must not look up a subscription")
				})
				.unwrap()
				.is_none()
			);
		}
		assert_eq!(echo.untimed_datagram, untimed_datagram, "{version}: untimed datagram");
		assert_eq!(echo.track_info, track_info, "{version}: TRACK_INFO");
		assert_eq!(echo.group, group, "{version}: group");
		assert_eq!(echo.untimed_group, untimed_group, "{version}: untimed group");

		match version {
			lite::Version::Lite07 => {
				let mut expected = vec![0xFF, 0x40, 0, 0, 0, 0, 0, 0, 0];
				expected.extend([0xFF; 9]);
				assert_eq!(echo.beyond, expected, "{version}: JS's 64-bit encodings");
				let mut r = Decoder::new(&echo.beyond, Form::from(version));
				assert_eq!(r.varint().unwrap(), 1 << 62);
				assert_eq!(r.varint().unwrap(), u64::MAX);
				assert!(r.is_empty());
			}
			_ => assert!(echo.beyond.is_empty(), "{version}: JS wrote a value past 2^62-1"),
		}
	}
}

#[derive(serde::Deserialize)]
struct DatagramEcho {
	echoes: Vec<Vec<u8>>,
	/// The Timestamp each datagram's Properties carry, in milliseconds, if any.
	timestamps: Vec<Option<u64>>,
	/// The OBJECT_DATAGRAM the JS publisher sends for the first datagram's fields.
	published: Vec<u8>,
}

/// OBJECT_DATAGRAM on every draft, both ways: JS decodes what Rust encodes, including the
/// Timestamp, and re-encodes it byte for byte; Rust decodes what the JS publisher writes.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn ietf_datagram_interop() {
	use crate::coding::Decode;
	use crate::{Timescale, Timestamp};

	for (draft, version) in [
		(18, ietf::Version::Draft18),
		(20, ietf::Version::Draft20),
		(22, ietf::Version::Draft22),
	] {
		let mut properties = Vec::new();
		let mut w = Encoder::new(&mut properties, version.into());
		ietf::encode_object_time(&mut w, Timestamp::from_millis(1234).unwrap(), Timescale::MILLI).unwrap();

		// What a publisher sends: Object 0, explicit priority, ending its group, stamped.
		let mut datagrams = vec![ietf::ObjectDatagram {
			track_alias: 3,
			group_id: 42,
			object_id: None,
			publisher_priority: Some(7),
			end_of_group: true,
			properties: Some(properties.clone()),
			body: ietf::DatagramBody::Payload(bytes::Bytes::from_static(b"hello")),
		}];
		datagrams.push(ietf::ObjectDatagram {
			track_alias: 1,
			group_id: 2,
			object_id: Some(0),
			publisher_priority: Some(128),
			end_of_group: false,
			properties: None,
			body: ietf::DatagramBody::Status(0),
		});
		datagrams.push(ietf::ObjectDatagram {
			track_alias: (1 << 40) + 1,
			group_id: (1 << 50) + 1,
			object_id: Some(1 << 20),
			publisher_priority: None,
			end_of_group: true,
			properties: None,
			body: ietf::DatagramBody::Payload(bytes::Bytes::new()),
		});
		let encoded: Vec<Vec<u8>> = datagrams
			.iter()
			.map(|datagram| datagram.encode_bytes(version).unwrap().to_vec())
			.collect();

		let input = serde_json::json!({ "draft": draft, "datagrams": encoded });
		let echo: DatagramEcho =
			serde_json::from_slice(&bun("ietf-datagram.ts", input)).expect("JS returned its encodings");

		assert_eq!(echo.echoes, encoded, "{version}: JS re-encoded differently");
		let expected: Vec<Option<u64>> = datagrams
			.iter()
			.map(|datagram| datagram.properties.as_ref().map(|_| 1234))
			.collect();
		assert_eq!(echo.timestamps, expected, "{version}: JS decoded different timestamps");

		let (published, used) = ietf::ObjectDatagram::decode_slice(&echo.published, version).unwrap();
		assert_eq!(used, echo.published.len(), "{version}: a datagram runs to its end");
		assert_eq!(published, datagrams[0], "{version}: the JS publisher's datagram");
		let Some(properties) = &published.properties else {
			panic!("{version}: the JS publisher sent no Timestamp");
		};
		let mut r = Decoder::new(properties, version.into());
		let timestamp = ietf::decode_object_time(&mut r, Timescale::MILLI, version).unwrap();
		assert_eq!(timestamp.map(|t| t.as_millis()), Some(1234), "{version}: Timestamp");
	}
}

/// Unsupported after a grant keeps its meaning across the Rust/JS Lite binding boundary.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn lite_auth_unsupported_interop() {
	let version = lite::Version::Lite07;
	let grant = lite::AuthReply::Ok(lite::AuthOk {
		publish: crate::Patterns::from(crate::Pattern::subtree("room").unwrap()),
		subscribe: crate::Patterns::new(),
		expires: None,
	});
	let refused = lite::AuthReply::Error(lite::AuthError {
		code: crate::lite::error::to_session_code(&crate::SessionError::Unsupported).into(),
		reason: "unsupported".into(),
	});
	let mut bytes = Vec::new();
	let mut writer = Encoder::new(&mut bytes, version.into());
	grant.encode(&mut writer, version).unwrap();
	refused.encode(&mut writer, version).unwrap();
	let echo: Vec<u8> = serde_json::from_slice(&bun("auth-unsupported.ts", serde_json::json!(bytes))).unwrap();
	assert_eq!(echo, bytes);
	let mut reader = Decoder::new(&echo, version.into());
	assert_eq!(
		<lite::AuthReply as crate::coding::Decode<_>>::decode(&mut reader, version).unwrap(),
		grant
	);
	assert_eq!(
		<lite::AuthReply as crate::coding::Decode<_>>::decode(&mut reader, version).unwrap(),
		refused
	);
	assert!(reader.is_empty());
}

/// moq-lite-07 TRACKS across the Rust/JS boundary: the JS publisher answers a Rust TRACKS request
/// with a listing Rust decodes, and JS decodes Rust's listing events and re-encodes them byte for
/// byte.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn lite_tracks_interop() {
	use crate::coding::Decode;

	let version = lite::Version::Lite07;
	let request = lite::Tracks {
		broadcast: crate::Path::from_static("room"),
		epoch: None,
	}
	.encode_bytes(version)
	.unwrap();
	let input = serde_json::json!({ "mode": "answer", "bytes": request.to_vec() });
	let answer: Vec<u8> = serde_json::from_slice(&bun("tracks.ts", input)).expect("JS returned its listing");
	let mut r = Decoder::new(&answer, version.into());
	let mut events = Vec::new();
	while !r.is_empty() {
		events.push(match lite::TracksEvent::decode(&mut r, version).unwrap() {
			lite::TracksEvent::Added { name, info } => {
				if name == "a" {
					assert_eq!(info.timescale, Some(crate::Timescale::MILLI));
					assert_eq!(info.max_age, Some(std::time::Duration::from_secs(3)));
					assert_eq!(info.priority, 9);
					assert_eq!(info.refresh, Some(4));
				}
				format!("+{name}")
			}
			lite::TracksEvent::Removed { name } => format!("-{name}"),
			lite::TracksEvent::Finished { name } => format!("~{name}"),
			lite::TracksEvent::Synced => "synced".to_string(),
		});
	}
	// The broadcast's end is the JS publisher's FIN, after the last event.
	assert_eq!(events, ["+a", "+b", "synced", "-a"]);

	let info = lite::TrackInfo {
		refresh: Some(4),
		priority: 3,
		max_age: Some(std::time::Duration::from_secs(2)),
		timescale: Some(crate::Timescale::MICRO),
		compress: true,
	};
	let mut bytes = Vec::new();
	let mut w = Encoder::new(&mut bytes, version.into());
	for event in [
		lite::TracksEvent::Added {
			name: "video".into(),
			info,
		},
		lite::TracksEvent::Removed { name: "video".into() },
		lite::TracksEvent::Synced,
	] {
		event.encode(&mut w, version).unwrap();
	}
	let input = serde_json::json!({ "mode": "echo", "bytes": bytes });
	let echo: Vec<u8> = serde_json::from_slice(&bun("tracks.ts", input)).expect("JS echoed the events");
	assert_eq!(echo, bytes);
}

/// TRACK_STATUS on every draft: the JS publisher decodes what Rust encodes and answers with a
/// TRACK_STATUS_OK the Rust subscriber reads, carrying what a SUBSCRIBE_OK would as far as the
/// draft has room, or refuses a missing broadcast DOES_NOT_EXIST.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn ietf_track_status_interop() {
	use crate::coding::Decode;
	use crate::ietf::error::request;

	for (draft, version) in [
		(18, ietf::Version::Draft18),
		(20, ietf::Version::Draft20),
		(22, ietf::Version::Draft22),
	] {
		let request = |id: u64, namespace: &str, wanted: bool| {
			let mut buf = Vec::new();
			let mut w = Encoder::new(&mut buf, version.into());
			w.varint(<ietf::TrackStatus as ietf::Message>::ID).unwrap();
			ietf::TrackStatus {
				request_id: ietf::RequestId(id),
				track_namespace: crate::Path::new(namespace).unwrap(),
				track_name: "video".into(),
				properties_wanted: wanted,
				epoch: None,
			}
			.encode(&mut w, version)
			.unwrap();
			buf
		};
		let input = serde_json::json!({
			"draft": draft,
			"requests": [request(1, "room", true), request(3, "room", false), request(5, "absent", true)],
		});
		let responses: Vec<Vec<u8>> =
			serde_json::from_slice(&bun("track-status.ts", input)).expect("JS returned its responses");
		assert_eq!(responses.len(), 3, "{version}");

		for (index, wanted) in [true, false].into_iter().enumerate() {
			let mut r = Decoder::new(&responses[index], version.into());
			assert_eq!(
				r.varint().unwrap(),
				<ietf::TrackStatusOk as ietf::Message>::ID,
				"{version}"
			);
			let ok = ietf::TrackStatusOk::decode(&mut r, version).unwrap();
			assert!(r.is_empty(), "{version}: trailing bytes");

			let expected = match version {
				// Draft 20 and later can carry the opt-out.
				_ if !wanted && ietf::Filter::is_draft20(version) => ietf::Properties::default(),
				_ => ietf::Properties {
					unknown: Default::default(),
					dynamic_groups: false,
					compress: false,
					max_cache_duration: Some(std::time::Duration::from_millis(5000)),
					timescale: Some(crate::Timescale::MILLI),
					// The JS track's priority 2, lower-first on the wire as `127 - 2`.
					priority: Some(125),
					group_order: Some(ietf::GroupOrder::Descending),
				},
			};
			assert_eq!(
				ok,
				ietf::TrackStatusOk {
					largest: Some(ietf::Location { group: 4, object: 1 }),
					properties: expected,
				},
				"{version} wanted={wanted}"
			);
		}

		let mut r = Decoder::new(&responses[2], version.into());
		let code = match r.varint().unwrap() {
			id if id == <ietf::RequestError as ietf::Message>::ID => {
				ietf::RequestError::decode(&mut r, version).unwrap().error_code
			}
			id => panic!("{version}: unexpected refusal type {id}"),
		};
		assert!(
			matches!(
				request::from_code(code, request::Kind::TrackStatus, version),
				crate::Error::NotFound
			),
			"{version}: missing broadcast refused with {code}"
		);
	}
}
