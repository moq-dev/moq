//! The MoQ Auth extension (draft-lcurley-moq-auth-00).
//!
//! The moq-transport binding of the lite-06 Auth Stream (see [`crate::auth`]): each
//! token rides a request stream of its own, answered with the namespace prefixes it
//! grants. Negotiated with the AUTH Setup Option on draft-17+ only, where SETUP is a
//! Key-Value-Pair block.
//!
//! The wire carries prefixes, so a grant is told only when it is a union of subtrees.
//! Anything narrower is refused with NOT_SUPPORTED rather than widened.

use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;

use crate::auth::{Grant, Handle, Issue, Reply, Request};
use crate::coding::{Decode, DecodeError, Encode, EncodeError, Sizer, Stream};
use crate::{Error, Path, Pattern, Patterns, SessionError};

use super::namespace::{decode_namespace, encode_namespace};
use super::{Control, Message, RequestId, Version, cluster, peer};

/// AUTH Setup Option: the sender speaks this extension. Even, so the value is a bare
/// varint.
pub const AUTH: u64 = 0x40B60;

/// Whether a version negotiates this extension: draft-17+, like MoQ Cluster.
pub fn supported(version: Version) -> bool {
	cluster::supported(version)
}

/// What the peer declared: `None` for no option, otherwise whether it offered the
/// extension. Only an explicit 1 negotiates it.
pub fn from_setup(params: &super::Parameters, version: Version) -> Option<bool> {
	if !supported(version) {
		return None;
	}
	params.get_varint(super::ParameterVarInt::Auth).map(|value| value == 1)
}

/// Offer the extension, on the versions that negotiate it.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	if supported(version) {
		params.set_varint(super::ParameterVarInt::Auth, 1);
	}
}

/// Refuse a message on a version that cannot negotiate the extension.
fn check_version(version: Version) -> Result<(), DecodeError> {
	match supported(version) {
		true => Ok(()),
		false => Err(DecodeError::Version),
	}
}

/// AUTH: the first message on an Auth request stream, presenting a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
	pub request_id: RequestId,
	/// Empty presents the credential the connection already carried.
	pub token: Bytes,
}

impl Message for Auth {
	const ID: u64 = 0x40B61;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		check_version(version).map_err(|_| EncodeError::Version)?;
		self.request_id.encode(w, version)?;
		self.token.encode(w, version)
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		check_version(version)?;
		Ok(Self {
			request_id: RequestId::decode(r, version)?,
			token: Bytes::decode(r, version)?,
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

/// Encode patterns as namespace prefix tuples, refusing any that is not a subtree:
/// sending `room` for a grant of the literal `room/alice` would hand out more than was
/// granted.
fn encode_prefixes<W: bytes::BufMut>(patterns: &Patterns, w: &mut W, version: Version) -> Result<(), EncodeError> {
	patterns.len().encode(w, version)?;
	for pattern in patterns {
		let prefix = pattern.as_prefix().ok_or(EncodeError::Unsupported)?;
		encode_namespace(w, &Path::new(prefix), version)?;
	}
	Ok(())
}

fn decode_prefixes<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Patterns, DecodeError> {
	let count = usize::decode(r, version)?;
	let mut patterns = Patterns::new();
	// No preallocation: the count is peer-controlled, and the message size limit bounds
	// how many prefixes actually fit.
	for _ in 0..count {
		let prefix = decode_namespace(r, version)?;
		let pattern = Pattern::subtree(prefix.as_str()).map_err(|_| DecodeError::InvalidValue)?;
		patterns.insert(pattern);
	}
	Ok(patterns)
}

impl Message for AuthOk {
	const ID: u64 = 0x40B62;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		check_version(version).map_err(|_| EncodeError::Version)?;
		encode_prefixes(&self.publish, w, version)?;
		encode_prefixes(&self.subscribe, w, version)?;
		// 0 means never, so a grant that has already lapsed rounds up to the smallest
		// value that still reads as an expiry.
		let expires = match self.expires {
			None => 0,
			Some(expires) => (expires.as_nanos().div_ceil(1_000_000).min(MAX_EXPIRES_MS as u128) as u64).max(1),
		};
		expires.encode(w, version)
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		check_version(version)?;
		let publish = decode_prefixes(r, version)?;
		let subscribe = decode_prefixes(r, version)?;
		let expires = match u64::decode(r, version)? {
			0 => None,
			ms => Some(Duration::from_millis(ms)),
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

/// Longest AUTH_ERROR reason, in bytes, matching the lite wire.
const MAX_REASON: usize = 8192;

impl Message for AuthError {
	const ID: u64 = 0x40B63;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		check_version(version).map_err(|_| EncodeError::Version)?;
		if self.reason.len() > MAX_REASON {
			return Err(EncodeError::TooLarge);
		}
		self.code.encode(w, version)?;
		self.reason.as_str().encode(w, version)
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		check_version(version)?;
		let code = u64::decode(r, version)?;
		let reason = String::decode(r, version)?;
		if reason.len() > MAX_REASON {
			return Err(DecodeError::InvalidValue);
		}
		Ok(Self { code, reason })
	}
}

/// The REQUEST_ERROR codes AUTH_ERROR carries.
const UNAUTHORIZED: u64 = 0x1;
const NOT_SUPPORTED: u64 = 0x3;

/// The AUTH_ERROR code for a refusal. The public API speaks session codes, and the
/// only one this registry distinguishes is a version mismatch.
fn to_code(code: SessionError) -> u64 {
	match code {
		SessionError::Version => NOT_SUPPORTED,
		_ => UNAUTHORIZED,
	}
}

/// Read an AUTH_ERROR code. NOT_SUPPORTED means the acceptor could not tell the grant,
/// not that it refused one; every other code is a refusal.
fn from_code(code: u64) -> Error {
	match code {
		NOT_SUPPORTED => Error::Unsupported,
		_ => Error::Session(SessionError::Unauthorized),
	}
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
pub(super) async fn run_present<S>(
	runtime: crate::time::Clock,
	session: S,
	control: Control,
	handle: Handle,
	peer_setup: peer::PeerSetup,
	version: Version,
	going_away: crate::goaway::GoingAway,
) where
	S: crate::transport::poll::Boxable,
{
	if !peer_setup.get().await.auth {
		handle.unsupported();
		return std::future::pending().await;
	}

	let mut tasks = crate::util::TaskSet::owned();
	while let Some((id, token)) = tasks.drive(|waiter| handle.poll_opening(waiter)).await {
		let present = Present {
			runtime: runtime.clone(),
			session: session.clone(),
			control: control.clone(),
			handle: handle.clone(),
			version,
			id,
		};
		let going_away = going_away.clone();
		tasks.push(async move {
			// After a GOAWAY the peer must not see new requests.
			let err = match going_away.is_set() {
				true => Error::GoingAway,
				false => present.run(token).await,
			};
			match &err {
				Error::Cancel | Error::Unsupported | Error::Transport(_) | Error::Session(_) => {
					tracing::debug!(%err, "auth token ended")
				}
				err => tracing::warn!(%err, "auth token ended"),
			}
			present.handle.ended(present.id, err);
		});
	}
	std::future::pending().await
}

/// One token's Auth request.
struct Present<S: crate::transport::poll::Session> {
	runtime: crate::time::Clock,
	session: S,
	control: Control,
	handle: Handle,
	version: Version,
	id: u64,
}

impl<S: crate::transport::poll::Boxable> Present<S> {
	/// Send the token, then track its grant until either side ends it, resolving with why.
	async fn run(&self, token: Bytes) -> Error {
		let request_id = match self.control.next_request_id(&self.runtime).await {
			Ok(id) => id,
			Err(err) => return err,
		};
		let mut stream = match Stream::open(&mut self.session.clone(), self.version).await {
			Ok(stream) => stream,
			Err(err) => return err,
		};
		if let Err(err) = stream.writer.encode_message(&Auth { request_id, token }).await {
			return err;
		}

		let mut answered = false;
		loop {
			enum Next {
				Withdrawn,
				Reply(Result<Option<(u64, Bytes)>, Error>),
			}

			let next = {
				let mut read = std::pin::pin!(read_message(&mut stream));
				kio::wait(|waiter| {
					if self.handle.poll_withdrawn(self.id, waiter).is_ready() {
						return Poll::Ready(Next::Withdrawn);
					}
					waiter.poll_future(read.as_mut()).map(Next::Reply)
				})
				.await
			};

			let (id, mut data) = match next {
				// Resetting our side is what tells the peer.
				Next::Withdrawn => return Error::Cancel,
				Next::Reply(Ok(Some(msg))) => msg,
				// The peer ended the grant without revoking it.
				Next::Reply(Ok(None)) if answered => return Error::Cancel,
				// A peer that closes before answering could not tell us anything.
				Next::Reply(Ok(None)) => return Error::Unsupported,
				Next::Reply(Err(err)) => return err,
			};

			match id {
				AuthOk::ID => {
					let ok = match AuthOk::decode_msg(&mut data, self.version) {
						Ok(ok) => ok,
						Err(err) => return err.into(),
					};
					let now = crate::runtime::Timers::now(&self.runtime);
					// The expiry is the peer's number: one past the local clock's range is
					// malformed, not a reason to panic.
					let expires = match ok.expires.map(|expires| now.checked_add(expires)) {
						Some(None) => return Error::ProtocolViolation,
						expires => expires.flatten(),
					};
					answered = true;
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
					let refused = match AuthError::decode_msg(&mut data, self.version) {
						Ok(refused) => refused,
						Err(err) => return err.into(),
					};
					let err = from_code(refused.code);
					tracing::warn!(%err, code = refused.code, reason = %refused.reason, "auth token refused");
					// A grant the acceptor could not tell is unknown, not empty, so only a
					// real refusal settles the union.
					if !matches!(err, Error::Unsupported) {
						self.handle.refused(self.id);
					}
					return err;
				}
				_ => return Error::UnexpectedMessage,
			}
		}
	}
}

/// Read one `[type][size][body]` message, or `None` once the peer finished the stream.
async fn read_message<S: crate::transport::poll::Session>(
	stream: &mut Stream<S, Version>,
) -> Result<Option<(u64, Bytes)>, Error> {
	let Some(id) = stream.reader.decode_maybe::<u64>().await? else {
		return Ok(None);
	};
	let size: u16 = stream.reader.decode().await?;
	let data = stream.reader.read_exact(size as usize).await?;
	Ok(Some((id, data)))
}

/// Answers the peer's Auth requests: the app's verdict when it took the requests,
/// otherwise the default grant for the connection's own credential.
#[derive(Clone)]
pub(super) struct Serve {
	pub runtime: crate::time::Clock,
	pub handle: Handle,
	/// What the peer's connection credential earns by default.
	pub peer_grant: Grant,
}

impl Serve {
	/// Answer one Auth request. The stream lives as long as the token.
	pub(super) async fn run<S: crate::transport::poll::Session>(
		self,
		mut stream: Stream<S, Version>,
		msg: Auth,
		version: Version,
	) {
		let issue = Issue::shared();
		match self.handle.acceptor() {
			Some(requests) => {
				// A closed queue hands the request back, and dropping it refuses the token.
				let _ = requests.try_push(Request::new(msg.token, issue.clone()));
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
				return;
			}
			None => issue.lock().outbox.push_back(Reply::Grant(self.peer_grant)),
		}

		let err = match serve_issue(&self.runtime, &issue, &mut stream, version).await {
			Ok(()) => Error::Cancel,
			Err(err) => {
				match &err {
					Error::Cancel | Error::Unsupported | Error::Stream(_) | Error::Session(_) | Error::Transport(_) => {
						tracing::debug!(%err, "auth request ended")
					}
					err => tracing::warn!(%err, "auth request error"),
				}
				err
			}
		};
		issue.lock().peer.get_or_insert(err);
	}
}

/// Write the acceptor's replies until either side ends the token.
async fn serve_issue<S: crate::transport::poll::Session>(
	runtime: &crate::time::Clock,
	issue: &kio::Shared<Issue>,
	stream: &mut Stream<S, Version>,
	version: Version,
) -> Result<(), Error> {
	enum Next {
		Withdrawn(Result<(), Error>),
		Reply(Option<Reply>),
	}

	loop {
		let next = kio::wait(|waiter| {
			let mut cx = std::task::Context::from_waker(waiter.waker());
			if let Poll::Ready(res) = stream.reader.poll_closed(&mut cx) {
				return Poll::Ready(Next::Withdrawn(res));
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
			// The presenter withdrew the token, by FIN or by a cancelling reset.
			Next::Withdrawn(Ok(()) | Err(Error::Stream(crate::StreamError::Cancel))) => break,
			Next::Withdrawn(Err(err)) => return Err(err),
			Next::Reply(Some(Reply::Grant(grant))) => {
				let now = crate::runtime::Timers::now(runtime);
				let ok = AuthOk {
					publish: grant.publish,
					subscribe: grant.subscribe,
					expires: grant.expires.map(|at| at.saturating_duration_since(now)),
				};
				// Validated before anything is written, so an unrepresentable grant never
				// leaves half a message on the wire. Never widen it: refuse, which revokes
				// whatever this stream granted before.
				if let Err(EncodeError::Unsupported) = ok.encode_msg(&mut Sizer::default(), version) {
					tracing::debug!("auth grant not representable as prefixes; refusing the token");
					issue.lock().done = true;
					let refused = AuthError {
						code: NOT_SUPPORTED,
						reason: "grant not representable as namespace prefixes".to_string(),
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

	const VERSION: Version = Version::Draft17;

	fn patterns(prefixes: &[&str]) -> Patterns {
		prefixes.iter().map(|p| Pattern::subtree(p).unwrap()).collect()
	}

	fn round_trip<T: Message + PartialEq>(msg: &T) -> T {
		let mut buf = bytes::BytesMut::new();
		msg.encode_msg(&mut buf, VERSION).unwrap();
		let mut slice = &buf[..];
		let got = T::decode_msg(&mut slice, VERSION).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		got
	}

	/// Every draft that negotiates the extension round-trips the option; the drafts
	/// before the unified SETUP carry none.
	#[test]
	fn setup_option_round_trips_on_supported_drafts() {
		for version in [
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			let mut params = super::super::Parameters::default();
			assert_eq!(from_setup(&params, version), None);
			into_setup(&mut params, version);
			assert_eq!(from_setup(&params, version), Some(true), "{version:?}");
		}
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert_eq!(params.get_varint(super::super::ParameterVarInt::Auth), None);
			assert_eq!(from_setup(&params, version), None, "{version:?}");
		}
	}

	/// An explicit value other than 1 is an implementation that declined, which is
	/// not the same statement as saying nothing.
	#[test]
	fn only_one_negotiates() {
		let mut params = super::super::Parameters::default();
		params.set_varint(super::super::ParameterVarInt::Auth, 0);
		assert_eq!(from_setup(&params, VERSION), Some(false));
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

	/// The root grant and a union of prefixes survive the trip, as distinct from the
	/// empty grant.
	#[test]
	fn auth_ok_round_trips() {
		for (publish, subscribe, expires) in [
			(patterns(&[""]), patterns(&[]), None),
			(
				patterns(&["room/alice", "room/bob"]),
				patterns(&["room"]),
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

	/// A prefix is a namespace tuple, the way SUBSCRIBE_NAMESPACE spells one.
	#[test]
	fn prefixes_are_namespace_tuples() {
		let msg = AuthOk {
			publish: patterns(&["room/alice"]),
			subscribe: Patterns::new(),
			expires: None,
		};
		let mut buf = bytes::BytesMut::new();
		msg.encode_msg(&mut buf, VERSION).unwrap();
		assert_eq!(&buf[..], b"\x01\x02\x04room\x05alice\x00\x00");
	}

	#[test]
	fn auth_error_round_trips() {
		let msg = AuthError {
			code: UNAUTHORIZED,
			reason: "expired".to_string(),
		};
		assert_eq!(round_trip(&msg), msg);
	}

	/// Only subtrees fit the prefix encoding, alone or in a union; anything narrower is
	/// refused, never widened to its head.
	#[test]
	fn unrepresentable_grants_are_refused() {
		for union in [&["room/alice"][..], &["room/*/cam"], &["**/cam"], &["room/**", "lobby"]] {
			let msg = AuthOk {
				publish: union.iter().map(|p| Pattern::try_from(*p).unwrap()).collect(),
				subscribe: Patterns::new(),
				expires: None,
			};
			let mut buf = bytes::BytesMut::new();
			assert!(
				matches!(msg.encode_msg(&mut buf, VERSION), Err(EncodeError::Unsupported)),
				"{union:?} encoded"
			);
		}
	}

	#[test]
	fn older_drafts_have_no_auth() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let mut buf = bytes::BytesMut::new();
			let msg = Auth {
				request_id: RequestId(0),
				token: Bytes::new(),
			};
			assert!(matches!(msg.encode_msg(&mut buf, version), Err(EncodeError::Version)));
			let mut slice: &[u8] = &[0, 0];
			assert!(matches!(
				Auth::decode_msg(&mut slice, version),
				Err(DecodeError::Version)
			));
		}
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
	}
}
