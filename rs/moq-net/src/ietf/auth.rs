//! The MoQ Auth extension (draft-lcurley-moq-auth-01).
//!
//! The moq-transport binding of the lite Auth Stream (see [`crate::auth`]): each
//! token rides a request stream of its own, answered with the patterns it grants,
//! encoded as lite's AUTH_OK encodes them. Negotiated with the AUTH Setup Option.

#[cfg(feature = "async")]
use crate::driver::Transport;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;

use crate::auth::{Grant, Issue, Link, Reply, Request, Serving};
use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};
use crate::{Error, Patterns, SessionError};

#[cfg(feature = "async")]
use super::Control;
use super::{Message, RequestId, Version, peer};
#[cfg(feature = "async")]
use crate::coding::Stream;

/// AUTH Setup Option: the sender speaks this extension. Even, so the value is a bare
/// varint. Draft-01's code point, so a draft-00 peer, which read prefixes, never
/// negotiates it.
pub const AUTH: u64 = 0x40B70;

/// What the peer declared: `None` for no option, otherwise whether it offered the
/// extension. Only an explicit 1 negotiates it.
pub fn from_setup(params: &super::Parameters) -> Option<bool> {
	params.get_varint(super::ParameterVarInt::Auth).map(|value| value == 1)
}

/// Offer the extension.
pub fn into_setup(params: &mut super::Parameters) {
	params.set_varint(super::ParameterVarInt::Auth, 1);
}

/// AUTH: the first message on an Auth request stream, presenting a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
	pub request_id: RequestId,
	/// Empty presents the credential the connection already carried.
	pub token: Bytes,
}

impl<'a> Message<'a> for Auth {
	const ID: u64 = 0x40B61;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		w.bytes(&self.token)
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		Ok(Self {
			request_id: RequestId::decode(r, version)?,
			token: Bytes::copy_from_slice(r.bytes()?),
		})
	}
}

/// AUTH_OK: the grant a token earns, replacing any earlier one on the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthOk {
	/// What the presenter may publish to the acceptor.
	pub publish: Patterns,
	/// What the presenter may subscribe to from the acceptor.
	pub subscribe: Patterns,
	/// How long until the grant lapses, or `None` for never.
	pub expires: Option<Duration>,
}

/// Largest millisecond count every implementation carries losslessly.
const MAX_EXPIRES_MS: u64 = (1 << 53) - 1;

impl<'a> Message<'a> for AuthOk {
	const ID: u64 = 0x40B62;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.publish.encode(w, version)?;
		self.subscribe.encode(w, version)?;
		// 0 means never, so a grant that has already lapsed rounds up to the smallest
		// value that still reads as an expiry.
		let expires = match self.expires {
			None => 0,
			Some(expires) => (expires.as_nanos().div_ceil(1_000_000).min(MAX_EXPIRES_MS as u128) as u64).max(1),
		};
		w.varint(expires)
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let publish = Patterns::decode(r, version)?;
		let subscribe = Patterns::decode(r, version)?;
		let expires = match r.varint()? {
			0 => None,
			ms if ms <= MAX_EXPIRES_MS => Some(Duration::from_millis(ms)),
			_ => return Err(DecodeError::InvalidValue),
		};
		Ok(Self {
			publish,
			subscribe,
			expires,
		})
	}
}

/// AUTH_ERROR: the acceptor refusing a token, or revoking it after an AUTH_OK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthError {
	/// A code from the REQUEST_ERROR registry.
	pub code: u64,
	pub reason: String,
}

/// Longest AUTH_ERROR reason, in bytes, matching the lite wire and rejected from the
/// length prefix alone.
const MAX_REASON: usize = 8192;

impl<'a> Message<'a> for AuthError {
	const ID: u64 = 0x40B63;

	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		if self.reason.len() > MAX_REASON {
			return Err(EncodeError::TooLarge);
		}
		w.varint(self.code)?;
		w.string(&self.reason)
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let code = r.varint()?;
		if code > u32::MAX as u64 {
			return Err(DecodeError::InvalidValue);
		}
		let len = r.varint()?;
		if len > MAX_REASON as u64 {
			return Err(DecodeError::InvalidValue);
		}
		let reason = String::from_utf8(r.slice(len as usize)?.to_vec())?;
		Ok(Self { code, reason })
	}
}

/// The REQUEST_ERROR codes AUTH_ERROR carries.
const UNAUTHORIZED: u64 = 0x1;
const NOT_SUPPORTED: u64 = 0x3;
const MALFORMED_AUTH_TOKEN: u64 = 0x4;
const EXPIRED_AUTH_TOKEN: u64 = 0x5;
const GOING_AWAY: u64 = 0x6;

/// The AUTH_ERROR code for a refusal. The public API speaks session codes, and the
/// actionable distinctions this registry carries are unsupported, malformed, expired, and
/// unauthorized.
fn to_code(code: SessionError) -> u64 {
	match code {
		SessionError::Version | SessionError::Unsupported => NOT_SUPPORTED,
		SessionError::MalformedAuthToken => MALFORMED_AUTH_TOKEN,
		SessionError::Expired => EXPIRED_AUTH_TOKEN,
		_ => UNAUTHORIZED,
	}
}

/// Read an AUTH_ERROR code, preserving expiration so the presenter can refresh.
fn from_code(code: u64) -> Error {
	match code {
		NOT_SUPPORTED => Error::Unsupported,
		MALFORMED_AUTH_TOKEN => Error::Session(SessionError::MalformedAuthToken),
		EXPIRED_AUTH_TOKEN => Error::Expired,
		GOING_AWAY => Error::GoingAway,
		_ => Error::Session(SessionError::Unauthorized),
	}
}

/// Refuse an AUTH past our GOAWAY cutoff with AUTH_ERROR, the only answer its presenter reads.
#[cfg(feature = "async")]
pub(super) async fn refuse_going_away(mut stream: Stream<Transport, Version>) -> Result<(), Error> {
	let reply = AuthError {
		code: GOING_AWAY,
		reason: "going away".to_string(),
	};
	stream.writer.encode_message(&reply).await?;
	// A peer dropping the response after reading the refusal is a normal end.
	let _ = stream.writer.close().await;
	Ok(())
}

/// What the peer's connection credential earns by default: publishing what our
/// subscribe half accepts, and subscribing to what our publish half serves.
pub(super) fn peer_grant(
	publish: Option<&crate::origin::Consumer>,
	subscribe: Option<&crate::origin::Producer>,
) -> Grant {
	Grant {
		publish: subscribe.map(|origin| origin.allowed()).unwrap_or_default(),
		subscribe: publish.map(|origin| origin.allowed()).unwrap_or_default(),
		expires: None,
	}
}

/// Present every token this side adds, one AUTH request each, once the peer's SETUP
/// says it negotiated the extension. Never returns: a session without it just fails
/// the tokens as unsupported.
#[cfg(feature = "async")]
pub(super) async fn run_present(
	runtime: crate::time::Deadlines,
	session: Transport,
	control: Control,
	handle: Link,
	peer_setup: peer::PeerSetup,
	version: Version,
	stats: crate::stats::Session,
) -> Result<(), Error> {
	if !peer_setup.get().await.auth {
		handle.unsupported();
		return std::future::pending().await;
	}

	let mut tasks = crate::util::TaskSet::owned();
	let violation: kio::Shared<Option<Error>> = Default::default();
	while let Some((id, token)) = tasks
		.drive(|waiter| {
			if let Poll::Ready(mut err) = violation.poll(waiter, |err| match err.is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
				return Poll::Ready(Err(err.take().unwrap()));
			}
			handle.poll_opening(waiter).map(Ok)
		})
		.await?
	{
		let present = Present {
			stats: stats.clone(),
			runtime: runtime.clone(),
			session: session.clone(),
			control: control.clone(),
			handle: handle.clone(),
			version,
			id,
		};
		let violation = violation.clone();
		tasks.push(async move {
			let mut token = Ok(token);
			let mut answered = false;
			loop {
				// After a GOAWAY, sent or received, the peer must not see new requests.
				let err = match token {
					Ok(token) if present.handle.active() => present.run(token, &mut answered).await,
					Ok(_) => Error::GoingAway,
					Err(err) => err,
				};
				let err = match err {
					// The peer negotiated the extension, but a reset before any reply still tells
					// us nothing about the token: it earns nothing here, as on moq-lite.
					Error::Stream(_) if !answered => Error::Unsupported,
					err => crate::auth::protocol_error(err),
				};
				// AUTH_ERROR carries no code that may pass, so unlike moq-lite only a lost
				// request is presented again.
				if let Some(delay) = present.handle.retry(present.id, &err) {
					tracing::debug!(%err, ?delay, "auth token presenting again");
					token = present.reopen(delay).await;
					continue;
				}
				if matches!(err, Error::ProtocolViolation) {
					violation.lock().get_or_insert(err.clone());
				}

				match &err {
					Error::Cancel
					| Error::Unsupported
					| Error::Transport(_)
					| Error::IdleTimeout
					| Error::Session(_) => {
						tracing::debug!(%err, "auth token ended")
					}
					err => tracing::warn!(%err, "auth token ended"),
				}
				present.handle.ended(present.id, err);
				return;
			}
		});
	}
	std::future::pending().await
}

/// One token's Auth request.
#[cfg(feature = "async")]
struct Present {
	stats: crate::stats::Session,
	runtime: crate::time::Deadlines,
	session: Transport,
	control: Control,
	handle: Link,
	version: Version,
	id: u64,
}

#[cfg(feature = "async")]
impl Present {
	/// Wait out a retry's `delay`, then take the token to present again.
	async fn reopen(&self, delay: Duration) -> Result<Bytes, Error> {
		let mut deadline = crate::time::Deadline::after(&self.runtime, delay);
		kio::wait(|waiter| {
			if self.handle.poll_withdrawn(self.id, waiter).is_ready() {
				return Poll::Ready(Err(Error::Cancel));
			}
			std::task::ready!(deadline.poll(waiter));
			Poll::Ready(self.handle.reopen(self.id))
		})
		.await
	}

	/// Send the token, then track its grant until either side ends it, resolving with why.
	///
	/// `answered` records that the peer replied to the token, on this request or an
	/// earlier one, so a request it closes or resets before replying is lost rather than
	/// unsupported.
	async fn run(&self, token: Bytes, answered: &mut bool) -> Error {
		let mut stream = match Stream::open_request(&mut self.session.clone(), self.version).await {
			Ok(stream) => stream,
			Err(err) => return err,
		};
		stream = stream.with_stats(&self.stats);
		let request_id = self.control.next_request_id();
		if let Err(err) = stream.writer.encode_message(&Auth { request_id, token }).await {
			return err;
		}

		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		loop {
			enum Next {
				Withdrawn,
				Reply(Result<Option<super::request_stream::Framed>, Error>),
			}

			let next = {
				let mut read = std::pin::pin!(messages.next(&mut stream.reader));
				kio::wait(|waiter| {
					if self.handle.poll_withdrawn(self.id, waiter).is_ready() {
						return Poll::Ready(Next::Withdrawn);
					}
					waiter.poll_future(read.as_mut()).map(Next::Reply)
				})
				.await
			};

			let (id, body) = match next {
				// Resetting our side is what tells the peer.
				Next::Withdrawn => return Error::Cancel,
				Next::Reply(Ok(Some(super::request_stream::Framed(id, body)))) => (id, body),
				// The peer ended the grant without revoking it.
				Next::Reply(Ok(None)) if *answered => return Error::Cancel,
				// A peer that closes before answering could not tell us anything.
				Next::Reply(Ok(None)) => return Error::Unsupported,
				Next::Reply(Err(err)) => return err,
			};

			match id {
				AuthOk::ID => {
					let ok: AuthOk = match decode_reply(&body, self.version) {
						Ok(ok) => ok,
						Err(err) => return err,
					};
					let now = self.runtime.now();
					// The expiry is the peer's number: one past the local clock's range is
					// malformed, not a reason to panic.
					let expires = match ok.expires.map(|expires| now.checked_add(expires)) {
						Some(None) => return Error::ProtocolViolation,
						expires => expires.flatten(),
					};
					*answered = true;
					self.handle.granted(
						self.id,
						Grant {
							publish: ok.publish,
							subscribe: ok.subscribe,
							expires,
						},
					);
				}
				AuthError::ID => {
					let refused: AuthError = match decode_reply(&body, self.version) {
						Ok(refused) => refused,
						Err(err) => return err,
					};
					let err = from_code(refused.code);
					tracing::warn!(%err, code = refused.code, reason = %refused.reason, "auth token refused");
					// A grant the acceptor could not tell, or never judged, is unknown, not
					// empty, so only a real refusal settles the union.
					if !matches!(err, Error::Unsupported | Error::GoingAway) {
						self.handle.refused(self.id);
					}
					*answered = true;
					return err;
				}
				_ => return Error::UnexpectedMessage,
			}
		}
	}
}

/// Decode a reply that must fill its whole body, as every framed message does.
fn decode_reply<'m, T: Message<'m>>(body: &'m super::Body, version: Version) -> Result<T, Error> {
	let mut data = body.decoder(version);
	let msg = T::decode_msg(&mut data, version)?;
	if !data.is_empty() {
		return Err(Error::WrongSize);
	}
	Ok(msg)
}

/// Answers the peer's Auth requests: the app's verdict when it took the requests,
/// otherwise the default grant for the connection's own credential.
#[derive(Clone)]
pub(super) struct Serve {
	pub runtime: crate::time::Deadlines,
	pub handle: Link,
	/// What the peer's connection credential earns by default.
	pub peer_grant: Grant,
}

impl Serve {
	/// Answer one Auth request. The stream lives as long as the token.
	#[cfg(feature = "async")]
	pub(super) async fn run(
		self,
		mut stream: Stream<Transport, Version>,
		msg: Auth,
		version: Version,
	) -> Result<(), Error> {
		let mut goaways = super::request_stream::GoAways::new(msg.request_id.0 % 2 == 1);
		let serving = Serving::new(self.handle.clone());
		let issue = &serving.issue;
		let mut default = None;
		match self.handle.acceptor() {
			Some(requests) => {
				requests.push(Request::new(msg.token, issue.clone()));
			}
			// Only the connection's own credential has a default answer; a token needs
			// someone to verify it.
			None if !msg.token.is_empty() => {
				let reply = AuthError {
					code: NOT_SUPPORTED,
					reason: "tokens are not verified in band".to_string(),
				};
				if stream.writer.encode_message(&reply).await.is_ok() {
					let _ = stream.writer.close().await;
				}
				return Ok(());
			}
			// Re-sent whenever a new limit changes it.
			None => default = Some(crate::auth::DefaultGrant::new(self.handle.clone(), self.peer_grant)),
		}

		let err = match serve_issue(
			&self.runtime,
			issue,
			default.as_mut(),
			&mut stream,
			version,
			&mut goaways,
		)
		.await
		{
			Ok(()) => Error::Cancel,
			Err(err) => {
				match &err {
					Error::Cancel
					| Error::Unsupported
					| Error::Stream(_)
					| Error::Session(_)
					| Error::Transport(_)
					| Error::IdleTimeout => {
						tracing::debug!(%err, "auth request ended")
					}
					err => tracing::warn!(%err, "auth request error"),
				}
				err
			}
		};
		let err = crate::auth::protocol_error(err);
		serving.end(err.clone());
		if matches!(err, Error::ProtocolViolation) {
			return Err(err);
		}
		Ok(())
	}
}

/// Anything but a GOAWAY after the presenter's Auth: refused once its type is read,
/// rather than waiting on a body that cannot make it legal.
#[cfg(feature = "async")]
#[derive(Debug)]
struct Unexpected;

#[cfg(feature = "async")]
impl<'a> crate::coding::Decode<'a, Version> for Unexpected {
	fn decode(r: &mut crate::coding::Decoder<'_>, _: Version) -> Result<Self, crate::coding::DecodeError> {
		Err(crate::coding::DecodeError::InvalidMessage(r.varint()?))
	}
}

/// Write the acceptor's replies until either side ends the token. `default` feeds
/// the default acceptor's grant into the outbox as it changes.
#[cfg(feature = "async")]
async fn serve_issue(
	runtime: &crate::time::Deadlines,
	issue: &kio::Shared<Issue>,
	mut default: Option<&mut crate::auth::DefaultGrant>,
	stream: &mut Stream<Transport, Version>,
	version: Version,
	goaways: &mut super::request_stream::GoAways,
) -> Result<(), Error> {
	enum Next {
		Withdrawn(Result<(), Error>),
		Reply(Option<Reply>),
	}

	loop {
		let next = kio::wait(|waiter| {
			if let Poll::Ready(res) = goaways
				.poll_next::<Unexpected, _>(&mut stream.reader, waiter)
				.map(|res| res.map(|_| ()))
			{
				return Poll::Ready(Next::Withdrawn(res));
			}
			if let Some(default) = default.as_deref_mut()
				&& let Poll::Ready(grant) = default.poll(waiter)
			{
				issue.lock().outbox.push_back(Reply::Grant(grant));
			}
			match issue.poll(waiter, |issue| match issue.outbox.is_empty() && !issue.done {
				true => Poll::Pending,
				false => Poll::Ready(()),
			}) {
				Poll::Ready(mut issue) => Poll::Ready(Next::Reply(issue.outbox.pop_front())),
				Poll::Pending => Poll::Pending,
			}
		})
		.await;

		match next {
			// The presenter withdrew the token, by FIN or by a cancelling reset. That is
			// why it ended, whether or not our own FIN reaches a presenter that left.
			Next::Withdrawn(Ok(()) | Err(Error::Stream(crate::StreamError::Cancel))) => {
				issue.lock().peer.get_or_insert(Error::Cancel);
				break;
			}
			Next::Withdrawn(Err(err)) => return Err(err),
			Next::Reply(Some(Reply::Grant(grant))) => {
				let now = runtime.now();
				let ok = AuthOk {
					publish: grant.publish,
					subscribe: grant.subscribe,
					expires: grant.expires.map(|at| at.saturating_duration_since(now)),
				};
				// Sized as the full message before anything is written, so a grant over the u16
				// message size never leaves half a message on the wire. Never trim it: refuse,
				// which revokes whatever this stream granted before.
				if let Err(err) = ok.encode(&mut Encoder::new(&mut Vec::new(), version.into()), version) {
					tracing::debug!(%err, "auth grant cannot be encoded as one AUTH_OK; refusing the token");
					issue.lock().done = true;
					let refused = AuthError {
						code: NOT_SUPPORTED,
						reason: "grant cannot be encoded as one AUTH_OK".to_string(),
					};
					stream.writer.encode_message(&refused).await?;
					break;
				}
				stream.writer.encode_message(&ok).await?;
			}
			Next::Reply(Some(Reply::Refuse { code, reason })) => {
				let refused = AuthError {
					code: to_code(code),
					reason,
				};
				stream.writer.encode_message(&refused).await?;
			}
			// The app is done with the grant, or refused the token: close our side.
			Next::Reply(None) => break,
		}
	}

	stream.writer.finish()?;
	stream.writer.closed().await
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Pattern;

	#[cfg(feature = "async")]
	#[moq_clock::test]
	async fn auth_refused_without_credit_spends_no_request_id() {
		for version in [Version::Draft18] {
			for client in [false, true] {
				let first = if client { 0 } else { 1 };
				let control = Control::new(client);
				let present = Present {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(
						crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit(),
					),
					control: control.clone(),
					handle: Link::new(false),
					version,
					id: 0,
				};
				assert!(matches!(
					present.run(Bytes::new(), &mut false).await,
					Error::NoStreamCredit
				));
				assert_eq!(control.next_request_id(), RequestId(first));
			}
		}
	}

	const VERSION: Version = Version::Draft18;

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| Pattern::try_from(*text).unwrap()).collect()
	}

	fn encode_msg<'m, T: Message<'m>>(msg: &T, version: Version) -> Result<Vec<u8>, EncodeError> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)?;
		Ok(buf)
	}

	fn round_trip<T: for<'m> Message<'m> + PartialEq>(msg: &T) -> T {
		let buf = encode_msg(msg, VERSION).unwrap();
		let mut r = Decoder::new(&buf, VERSION.into());
		let got = T::decode_msg(&mut r, VERSION).unwrap();
		assert!(r.is_empty(), "trailing bytes after decode");
		got
	}

	/// Every draft that negotiates the extension round-trips the option.
	#[test]
	fn setup_option_round_trips_on_supported_drafts() {
		for version in [Version::Draft18, Version::Draft22] {
			let mut params = super::super::Parameters::default();
			assert_eq!(from_setup(&params), None);
			into_setup(&mut params);
			assert_eq!(from_setup(&params), Some(true), "{version:?}");
		}
	}

	/// An explicit value other than 1 is an implementation that declined, which is
	/// not the same statement as saying nothing.
	#[test]
	fn only_one_negotiates() {
		let mut params = super::super::Parameters::default();
		params.set_varint(super::super::ParameterVarInt::Auth, 0);
		assert_eq!(from_setup(&params), Some(false));
	}

	#[test]
	fn auth_round_trips() {
		for token in [Bytes::new(), Bytes::from_static(b"eyJhbGciOi.jwt")] {
			let msg = Auth {
				request_id: RequestId(4),
				token,
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	/// `**` grants everything, the empty pattern only the root, and the empty list
	/// nothing; literals and wildcards travel exactly, never widened to a prefix.
	#[test]
	fn auth_ok_round_trips() {
		for (publish, subscribe, expires) in [
			(patterns(&["**"]), patterns(&[]), None),
			(patterns(&[""]), patterns(&["room/**"]), None),
			(
				patterns(&["ch1", "room/*/cam", "**/demo.hang"]),
				patterns(&["live/*", "lobby/cam-*.hang"]),
				Some(Duration::from_secs(60)),
			),
		] {
			let msg = AuthOk {
				publish,
				subscribe,
				expires,
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	/// Each pattern is its canonical text, in canonical order, as lite's AUTH_OK spells it.
	/// The exact bytes, shared with `js/net/src/ietf/auth.test.ts` so both encoders agree.
	#[test]
	fn auth_ok_golden() {
		let msg = AuthOk {
			publish: patterns(&["room/*/cam", "ch1"]),
			subscribe: patterns(&[""]),
			expires: None,
		};
		let buf = encode_msg(&msg, VERSION).unwrap();
		assert_eq!(&buf[..], b"\x02\x03ch1\x0aroom/*/cam\x01\x00\x00");
	}

	#[test]
	fn auth_error_round_trips() {
		let msg = AuthError {
			code: UNAUTHORIZED,
			reason: "expired".to_string(),
		};
		assert_eq!(round_trip(&msg), msg);
	}

	/// An oversized reason is refused from its length prefix, before any of it arrives.
	#[test]
	fn auth_error_reason_is_capped_by_its_prefix() {
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, VERSION.into());
		w.varint(UNAUTHORIZED).unwrap();
		w.varint(MAX_REASON as u64 + 1).unwrap();
		let err = AuthError::decode_msg(&mut Decoder::new(&buf, VERSION.into()), VERSION).unwrap_err();
		assert!(matches!(err, DecodeError::InvalidValue), "{err:?}");
	}

	/// A reply with bytes past its message is refused, like any other framed message.
	#[test]
	fn replies_with_trailing_bytes_are_refused() {
		let ok = AuthOk {
			publish: patterns(&["room"]),
			subscribe: Patterns::new(),
			expires: None,
		};
		let refused = AuthError {
			code: UNAUTHORIZED,
			reason: "expired".to_string(),
		};
		let body = |msg: Vec<u8>, extra: &[u8]| super::super::Body(Bytes::from([&msg[..], extra].concat()));

		let exact = encode_msg(&ok, VERSION).unwrap();
		assert_eq!(decode_reply::<AuthOk>(&body(exact.clone(), &[]), VERSION).unwrap(), ok);
		let err = decode_reply::<AuthOk>(&body(exact, &[0]), VERSION).unwrap_err();
		assert!(matches!(err, Error::WrongSize), "{err:?}");

		let exact = encode_msg(&refused, VERSION).unwrap();
		assert_eq!(
			decode_reply::<AuthError>(&body(exact.clone(), &[]), VERSION).unwrap(),
			refused
		);
		let err = decode_reply::<AuthError>(&body(exact, &[0]), VERSION).unwrap_err();
		assert!(matches!(err, Error::WrongSize), "{err:?}");
	}

	/// NOT_SUPPORTED is the acceptor unable to tell a grant, never a refusal.
	#[test]
	fn not_supported_is_unsupported() {
		assert!(matches!(from_code(NOT_SUPPORTED), Error::Unsupported));
		assert!(matches!(
			from_code(UNAUTHORIZED),
			Error::Session(SessionError::Unauthorized)
		));
		assert_eq!(to_code(SessionError::Unauthorized), UNAUTHORIZED);
		assert!(matches!(from_code(EXPIRED_AUTH_TOKEN), Error::Expired));
		assert_eq!(to_code(SessionError::Expired), EXPIRED_AUTH_TOKEN);
		assert!(matches!(
			from_code(MALFORMED_AUTH_TOKEN),
			Error::Session(SessionError::MalformedAuthToken)
		));
		assert_eq!(to_code(SessionError::MalformedAuthToken), MALFORMED_AUTH_TOKEN);
	}
}
