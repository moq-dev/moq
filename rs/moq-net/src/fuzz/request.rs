//! The `request` target: the messages on a moq-transport request stream, framed as the
//! session reads them.
//!
//! A request stream is read one whole `[type][length][body]` message at a time
//! ([`ietf::Framed`]), at most one GOAWAY migrating it ([`ietf::Next`]), and the body is
//! decoded once the type is known ([`ietf::Body::message`]). The control stream decodes
//! the same messages through their own length prefix, so both readings must agree.

use crate::{
	coding::{Decode, DecodeError, Decoder, Encoder, Form},
	ietf,
	ietf::Message,
};

/// Decode a request stream from arbitrary bytes.
///
/// Byte 0 picks the draft and the rest is the stream. Returns whether any message decoded.
pub fn request(data: &[u8]) -> bool {
	let Some((&selector, mut rest)) = data.split_first() else {
		return false;
	};
	let version = super::IETF_VERSIONS[selector as usize % super::IETF_VERSIONS.len()];

	let mut any = false;
	let mut goaway = false;
	while !rest.is_empty() {
		let mut reader = Decoder::new(rest, version.into());
		let next = match ietf::Next::<ietf::Framed>::decode(&mut reader, version) {
			Ok(next) => next,
			Err(DecodeError::Short) => {
				assert!(reader.need() > rest.len(), "short read inside a complete message");
				break;
			}
			Err(_) => break,
		};
		let used = reader.position();
		any = true;

		match next {
			// A second GOAWAY ends the session, so nothing after it is read.
			ietf::Next::GoAway { .. } if std::mem::replace(&mut goaway, true) => break,
			ietf::Next::GoAway { .. } => {}
			ietf::Next::Message(framed) => {
				framing(&rest[..used], &framed, version);
				body(&framed, version);
			}
		}
		rest = &rest[used..];
	}
	any
}

/// A whole message is its type, a two-byte length, and exactly that many bytes, and
/// nothing past it was read.
fn framing(consumed: &[u8], framed: &ietf::Framed, version: ietf::Version) {
	let ietf::Framed(id, ietf::Body(body)) = framed;
	let form: Form = version.into();
	let mut decoder = Decoder::new(consumed, form);
	assert_eq!(
		decoder.varint().ok(),
		Some(*id),
		"message type {id:#x} was not read from the stream"
	);
	let id_len = decoder.position();
	let length = u16::from_be_bytes([consumed[id_len], consumed[id_len + 1]]) as usize;
	assert_eq!(length, body.len(), "message {id:#x} body is not its declared length");
	assert_eq!(
		consumed.len(),
		id_len + 2 + length,
		"message {id:#x} read past its body"
	);
	assert_eq!(
		&consumed[id_len + 2..],
		&body[..],
		"message {id:#x} body is not the stream's bytes"
	);

	// Not a GOAWAY, or the stream would have migrated instead.
	assert_ne!(*id, ietf::GoAway::ID, "a GOAWAY was read as a request message");

	// The hand-written REQUEST_UPDATE readings frame the message themselves, and must
	// stop where the generic framing did. The message is complete, so none may run short.
	fn whole<'a, T: Decode<'a, ietf::Version>>(consumed: &'a [u8], version: ietf::Version) {
		match T::decode_slice(consumed, version) {
			Ok((_, used)) => assert_eq!(used, consumed.len(), "an update read past its frame"),
			Err(DecodeError::Short) => panic!("an update ran short inside a complete frame"),
			Err(_) => {}
		}
	}
	if *id == ietf::SubscribeUpdate::ID {
		whole::<ietf::Update>(consumed, version);
		whole::<ietf::NamespaceUpdate<false>>(consumed, version);
		whole::<ietf::NamespaceUpdate<true>>(consumed, version);
	}
}

/// Decode a body as every message its type names, and check the control stream's reading
/// agrees. Some types are shared (REQUEST_UPDATE, REQUEST_OK), told apart by the request.
fn body(framed: &ietf::Framed, version: ietf::Version) {
	let ietf::Framed(id, body) = framed;
	let mut prefixed = Vec::with_capacity(body.0.len() + 2);
	prefixed.extend_from_slice(&(body.0.len() as u16).to_be_bytes());
	prefixed.extend_from_slice(&body.0);

	// A macro rather than a generic function, so each message borrows the body for as long
	// as its own decode needs, not for one lifetime every message shares.
	macro_rules! agree {
		($($ty:ty),* $(,)?) => {$(
			if *id == <$ty>::ID {
				let framed = body.message::<$ty>(version);
				// The control stream's reading: behind its own two-byte length, which must
				// bound it exactly.
				let control = <$ty>::decode_slice(&prefixed, version).map(|(msg, used)| {
					assert_eq!(used, prefixed.len(), "a control message stopped short of its length");
					format!("{msg:?}")
				});
				match (&framed, &control) {
					(Ok(msg), Ok(control)) => assert_eq!(&format!("{msg:?}"), control, "the two readings differ"),
					(Err(_), Err(_)) => {}
					_ => panic!("a request body reads {framed:?} but the control stream reads {control:?}"),
				}
				if let Ok(msg) = framed {
					let mut encoded = Vec::new();
					// An encode failure is legitimate: a value can decode at a draft that
					// cannot express it again. Emitting a body we refuse is not.
					if msg.encode_msg(&mut Encoder::new(&mut encoded, version.into()), version).is_ok() {
						let echo = ietf::Body(encoded.into());
						let reread = echo
							.message::<$ty>(version)
							.unwrap_or_else(|err| panic!("could not read our own {msg:?} back: {err}"));
						let mut again = Vec::new();
						reread
							.encode_msg(&mut Encoder::new(&mut again, version.into()), version)
							.expect("could not re-encode what we just encoded");
						assert_eq!(&again[..], &echo.0[..], "encoding is not stable");
					}
				}
			}
		)*};
	}
	agree!(
		ietf::Subscribe<'_>,
		ietf::SubscribeOk,
		ietf::SubscribeUpdate,
		ietf::Publish<'_>,
		ietf::PublishOk,
		ietf::PublishDone<'_>,
		ietf::Fetch<'_>,
		ietf::FetchOk,
		ietf::TrackStatus<'_>,
		ietf::TrackStatusOk,
		ietf::PublishNamespace,
		ietf::PublishNamespaceUpdate,
		ietf::SubscribeNamespace,
		ietf::Namespace,
		ietf::NamespaceDone,
		ietf::SubscribeTracks,
		ietf::PublishSkipped<'_>,
		ietf::RequestOk,
		ietf::RequestError<'_>,
	);
}

/// The streams the seeds open with: one of each request message from our own encoder,
/// then the same behind a GOAWAY.
pub(super) fn seeds() -> Vec<super::Seed> {
	let mut seeds = Vec::new();
	for (index, version) in super::IETF_VERSIONS.iter().enumerate() {
		let messages: Vec<(u64, Vec<u8>)> = vec![
			encode(
				&ietf::RequestOk {
					active: None,
					largest: None,
				},
				*version,
			),
			encode(
				&ietf::RequestError {
					error_code: 1,
					reason_phrase: "no".into(),
					retry_interval: 0,
				},
				*version,
			),
			encode(
				&ietf::SubscribeOk {
					track_alias: 1,
					largest: Some(ietf::Location { group: 2, object: 3 }),
					properties: Default::default(),
				},
				*version,
			),
			encode(
				&ietf::PublishDone {
					status_code: ietf::PublishDoneStatus::TrackEnded.code(),
					stream_count: 0,
					reason_phrase: "done".into(),
				},
				*version,
			),
		]
		.into_iter()
		.flatten()
		.collect();

		let goaway = encode(
			&ietf::GoAway {
				new_session_uri: "".into(),
				timeout: 0,
				request_id: None,
			},
			*version,
		);

		for (_, message) in &messages {
			let mut data = vec![index as u8];
			data.extend_from_slice(message);
			seeds.push(super::Seed {
				target: "request",
				kind: 0,
				data: data.clone(),
			});
			if let Some((_, goaway)) = &goaway {
				let mut data = vec![index as u8];
				data.extend_from_slice(goaway);
				data.extend_from_slice(message);
				seeds.push(super::Seed {
					target: "request",
					kind: 0,
					data,
				});
			}
		}
	}
	seeds
}

/// `msg` framed as a request stream carries it, or `None` if this draft cannot.
fn encode<'a, M: Message<'a>>(msg: &M, version: ietf::Version) -> Option<(u64, Vec<u8>)> {
	let mut out = Vec::new();
	let mut w = Encoder::new(&mut out, version.into());
	w.varint(M::ID).ok()?;
	crate::coding::Encode::encode(msg, &mut w, version).ok()?;
	Some((M::ID, out))
}
