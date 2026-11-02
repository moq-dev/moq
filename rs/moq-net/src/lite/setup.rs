//! The lite-05+ SETUP message: each endpoint advertises its capabilities once, as
//! the sole message on a unidirectional Setup Stream, then closes it.

use crate::coding::*;

use super::{Message, Parameters, Version};

/// Setup Parameter id for the Probe capability level.
const PARAM_PROBE: u64 = 0x1;
/// Setup Parameter id for the request Path (client-only, URI-less transports).
const PARAM_PATH: u64 = 0x2;
/// Setup Parameter id for the client's intended [`Role`] (client-only).
const PARAM_ROLE: u64 = 0x3;
/// Setup Parameter id for the link cost the dialer assigns to this connection.
const PARAM_COST: u64 = 0x4;
/// Setup Parameter id for the endpoint's Hop ID.
const PARAM_HOP: u64 = 0x5;
/// Setup Parameter id for the sender's implementation, as `name/version` product tokens.
const PARAM_IMPLEMENTATION: u64 = 0x6;
/// Setup Parameter id for the client's connection credential (client-only, lite-07+).
const PARAM_TOKEN: u64 = 0x7;
/// Setup Parameter id offering the `ietf` extension (draft-lcurley-moq-ietf): "IETF" in ASCII.
const PARAM_IETF: u64 = 0x4945_5446;

/// The cost of crossing a link that neither end priced.
///
/// One, so a mesh that configures no costs accumulates a route cost equal to the
/// hop count and ranks routes exactly as pre-lite-06 shortest-path routing did. Pricing
/// a link at 0 makes it free (a sibling in the same datacenter); pricing it higher
/// makes it a last resort (a metered backbone).
pub const DEFAULT_COST: u64 = 1;

/// The probe capability an endpoint advertises in SETUP.
///
/// Monotonic: a higher level implies every lower one. An unknown (future) value
/// decodes as the highest level we understand, so a peer that gains a new level is
/// treated as at least [`Increase`](Self::Increase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ProbeLevel {
	/// No probing. Equivalent to omitting the parameter.
	#[default]
	None,
	/// The publisher can measure and periodically report at least one of the PROBE
	/// metrics: its estimated bitrate, its round-trip time, or both. Either may be
	/// unknown in any given report, since the two are independent on the wire.
	Report,
	/// The publisher can additionally pad the connection (or send redundant data).
	Increase,
}

impl ProbeLevel {
	/// The level to advertise for `session`, from what its transport actually exposes.
	///
	/// [`Report`](Self::Report) claims the publisher can measure and periodically
	/// report. A transport that exposes neither a send-rate estimate nor an RTT can
	/// honour neither, and the draft requires such a publisher to reset any Probe
	/// Stream a subscriber opens. Advertising [`None`](Self::None) instead stops the
	/// subscriber opening one at all.
	///
	/// Both metrics are sampled rather than declared, so this only works for a
	/// transport whose figures exist by the time the session starts. QUIC and TCP
	/// both qualify: their RTT comes from the handshake, which has already happened.
	#[cfg(feature = "async")]
	pub fn detect(session: &crate::driver::Transport) -> Self {
		use crate::transport::Metrics as _;
		use crate::transport::poll::Session as _;
		let metrics = session.metrics();
		match metrics.estimated_send_rate().is_some() || metrics.rtt().is_some() {
			true => Self::Report,
			false => Self::None,
		}
	}

	/// Map the wire value to a level, saturating unknown values to [`Increase`](Self::Increase).
	fn from_code(code: u64) -> Self {
		match code {
			0 => Self::None,
			1 => Self::Report,
			_ => Self::Increase,
		}
	}

	/// The wire value for this level.
	fn to_code(self) -> u64 {
		match self {
			Self::None => 0,
			Self::Report => 1,
			Self::Increase => 2,
		}
	}
}

/// The single direction a client intends to use the session for.
///
/// A client advertises this in its SETUP so the server can reject a token that lacks
/// the matching scope during the handshake, instead of accepting a connection that
/// then silently carries no media (a subscribe-only token used to publish, or vice
/// versa). It only ever narrows what the server grants, so it is not a security
/// boundary: the server still enforces the token's scope regardless.
///
/// A session is bidirectional by default, which the wire says by omitting the
/// parameter. `Option<Role>` mirrors that: `None` is the default, and it's also what
/// a client that predates the parameter decodes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
	/// The client will publish tracks (ingest); the server must consume.
	Publisher,
	/// The client will subscribe to tracks (egress); the server must publish.
	Subscriber,
}

impl Role {
	/// Map the wire value to a role. `0` and any unrecognized future value are `None`
	/// (bidirectional): the draft requires a receiver that does not recognize the value
	/// to treat it as both directions, so a newer client can't break an older server (it
	/// just loses the early reject and defers fully to the token's scope).
	fn from_code(code: u64) -> Option<Self> {
		match code {
			1 => Some(Role::Publisher),
			2 => Some(Role::Subscriber),
			_ => None,
		}
	}

	/// The wire value for this role.
	fn to_code(self) -> u64 {
		match self {
			Role::Publisher => 1,
			Role::Subscriber => 2,
		}
	}

	/// Derive the advertised role from which origins a client wired up: publish-only is
	/// a [`Publisher`](Role::Publisher), consume-only a [`Subscriber`](Role::Subscriber),
	/// and both (or neither) advertises nothing. This keeps the advertised role from
	/// drifting away from what the session actually does.
	pub(crate) fn from_origins(publishes: bool, consumes: bool) -> Option<Self> {
		match (publishes, consumes) {
			(true, false) => Some(Role::Publisher),
			(false, true) => Some(Role::Subscriber),
			_ => None,
		}
	}

	/// Lowercase label for this role (`"publisher"` / `"subscriber"`).
	pub fn as_str(self) -> &'static str {
		match self {
			Role::Publisher => "publisher",
			Role::Subscriber => "subscriber",
		}
	}
}

impl std::fmt::Display for Role {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

/// The SETUP message, sent once per endpoint on the unidirectional Setup Stream.
///
/// lite-05+ only. The two endpoints' SETUP messages are independent: neither side
/// blocks on the peer's before opening other streams, but a stream whose encoding
/// depends on a negotiated capability (e.g. PROBE) must wait for it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Setup {
	/// The probe capability this endpoint supports. [`ProbeLevel::None`] when absent.
	pub probe: ProbeLevel,
	/// The request path, for transports that carry no request URI (native QUIC,
	/// qmux over TCP/TLS, unix sockets), with `?` and the URI query appended when
	/// there is one. Sent only by the client; a server never sends one and a relay
	/// never forwards it. `None` when the transport URI carries the target, where it would
	/// be a protocol violation, even empty. Otherwise an empty path selects the root, like
	/// `None`.
	pub path: Option<String>,
	/// The single direction the client intends to use, or `None` for a bidirectional
	/// session. `None` is sent as the absence of the parameter, which is also how a
	/// client that predates the parameter decodes.
	pub role: Option<Role>,
	/// What subscribing from this endpoint costs (lite-06+), added by the peer to the
	/// route cost of every announcement we forward it.
	///
	/// Directional: it prices the sender's own egress, so both ends declare their own
	/// and the two need not match. `None` means the default cost of 1.
	pub cost: Option<u64>,
	/// This endpoint's Hop ID, the identity forwarded announcements name it by.
	/// The peer uses it to serve this endpoint's subscriptions from a route that
	/// does not flow through it (the same split horizon the announce filter
	/// applies). `None` when the endpoint has no meaningful identity (a
	/// leaf that never forwards); a wire value of 0 decodes as `None`.
	pub hop: Option<crate::Hop>,
	/// The connection credential (lite-07+), which the empty-token Auth Stream names.
	/// Sent only by the client. The parameter carries no Token Type, so it is always
	/// [`Token::OUT_OF_BAND`](crate::setup::Token::OUT_OF_BAND).
	pub token: Option<crate::setup::Token>,
	/// Whether this endpoint offers the `ietf` extension (lite-07+). Negotiated only when
	/// both endpoints offer it.
	pub ietf: bool,
	/// The sender's implementation, as space-separated `name/version` product tokens
	/// (`moq-net-rs/0.3.10`). Opaque; `None` omits the parameter.
	pub implementation: Option<String>,
}

impl std::fmt::Debug for Setup {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Setup")
			.field("probe", &self.probe)
			// Native transports carry the JWT in this request target.
			.field(
				"path",
				&self.path.as_deref().map(|path| path.split('?').next().unwrap()),
			)
			.field("role", &self.role)
			.field("cost", &self.cost)
			.field("hop", &self.hop)
			.field("token", &self.token)
			.field("ietf", &self.ietf)
			.field("implementation", &self.implementation)
			.finish()
	}
}

impl<'a> Message<'a> for Setup {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Session);
	const MAX_SIZE: usize = crate::setup::MAX_SETUP_SIZE;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		ReceivedSetup::decode_msg(r, version).map(|received| received.setup)
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_setup_stream() {
			return Err(EncodeError::Version);
		}

		let mut params = Parameters::default();
		// None is the wire default, so omit it to keep the message empty when nothing is set.
		if self.probe != ProbeLevel::None {
			params.set_varint(PARAM_PROBE, self.probe.to_code(), version)?;
		}
		if let Some(path) = &self.path {
			params.set_bytes(PARAM_PATH, path.as_bytes().to_vec());
		}
		// Bidirectional is the wire default (absence of the parameter), so only a
		// directional role is encoded.
		if let Some(role) = self.role {
			params.set_varint(PARAM_ROLE, role.to_code(), version)?;
		}
		if let Some(cost) = self.cost {
			params.set_varint(PARAM_COST, cost, version)?;
		}
		if let Some(hop) = self.hop {
			params.set_varint(PARAM_HOP, hop.wire_id(version.into()), version)?;
		}
		if let Some(implementation) = &self.implementation {
			params.set_bytes(PARAM_IMPLEMENTATION, implementation.as_bytes().to_vec());
		}
		if let Some(token) = &self.token {
			if !version.has_auth() {
				return Err(EncodeError::Version);
			}
			// An empty value is no credential, and the untyped parameter cannot carry another type.
			if token.kind != crate::setup::Token::OUT_OF_BAND || token.value.is_empty() {
				return Err(EncodeError::Unsupported);
			}
			params.set_bytes(PARAM_TOKEN, token.value.clone());
		}
		if self.ietf {
			// The framing it changes is unpublished, so a published version cannot offer it.
			if !version.has_ietf() {
				return Err(EncodeError::Version);
			}
			params.set_bytes(PARAM_IETF, Vec::new());
		}

		params.encode(w, version)
	}
}

/// Raw presence is needed even when an unknown Role decodes as bidirectional.
#[derive(Debug)]
pub(super) struct ReceivedSetup {
	pub setup: Setup,
	role_present: bool,
}

impl<'a> Message<'a> for ReceivedSetup {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Session);
	const MAX_SIZE: usize = Setup::MAX_SIZE;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_setup_stream() {
			return Err(DecodeError::Version);
		}

		let params = Parameters::decode(r, version)?;
		let probe = params
			.get_varint(PARAM_PROBE, version)?
			.map(ProbeLevel::from_code)
			.unwrap_or_default();
		let path = match params.get_bytes(PARAM_PATH) {
			Some(bytes) => Some(
				std::str::from_utf8(bytes)
					.map_err(|_| DecodeError::InvalidValue)?
					.to_string(),
			),
			None => None,
		};
		let role_code = params.get_varint(PARAM_ROLE, version)?;
		let role = role_code.and_then(Role::from_code);
		let cost = params.get_varint(PARAM_COST, version)?;
		// 0 is legal on the wire but carries no identity (it can't be excluded),
		// so it decodes as "not declared" rather than an error.
		let hop = params
			.get_varint(PARAM_HOP, version)?
			.and_then(|id| crate::Hop::new(id).ok());
		let implementation = if version.has_implementation() {
			crate::setup::decode_implementation(params.get_bytes(PARAM_IMPLEMENTATION))?
		} else {
			None
		};
		// An older version does not know these parameters, so it ignores them like any other.
		let token = match params.get_bytes(PARAM_TOKEN) {
			Some([]) if version.has_auth() => return Err(DecodeError::InvalidValue),
			Some(value) if version.has_auth() => Some(crate::setup::Token {
				kind: crate::setup::Token::OUT_OF_BAND,
				value: value.to_vec(),
			}),
			_ => None,
		};
		let ietf = match params.get_bytes(PARAM_IETF) {
			Some(value) if version.has_ietf() => match value.is_empty() {
				true => true,
				false => return Err(DecodeError::InvalidValue),
			},
			_ => false,
		};

		Ok(Self {
			role_present: role_code.is_some(),
			setup: Setup {
				probe,
				path,
				role,
				cost,
				hop,
				token,
				ietf,
				implementation,
			},
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.setup.encode_msg(w, version)
	}
}

/// Shared slot for the peer's SETUP, written once when its Setup stream is read.
///
/// Streams whose encoding depends on a negotiated capability (e.g. the PROBE
/// stream) wait on this before deciding what to do. Cheap to clone: every handle
/// shares the same slot.
#[derive(Clone)]
pub(crate) struct PeerSetup(kio::Shared<PeerSetupState>);

struct PeerSetupState {
	client: bool,
	address: crate::transport::Address,
	/// Whether our own SETUP offers the `ietf` extension.
	ietf: bool,
	seen: bool,
	setup: Option<Setup>,
	/// Where the peer's advertised implementation is published for the session handle.
	implementation: crate::setup::PeerImplementation,
	/// The session's auth link to tell once the peer's SETUP accepts the connection.
	accept: Option<crate::auth::Link>,
}

impl PeerSetup {
	pub fn new(client: bool, address: crate::transport::Address) -> Self {
		Self(kio::Shared::new(PeerSetupState {
			client,
			address,
			ietf: false,
			seen: false,
			setup: None,
			implementation: Default::default(),
			accept: None,
		}))
	}

	/// Tell `link` once the peer's SETUP arrives that it accepted the connection credential.
	pub fn accepting(self, link: crate::auth::Link) -> Self {
		link.await_setup();
		self.0.lock().accept = Some(link);
		self
	}

	/// Publish the peer's advertised implementation to `slot` once its SETUP arrives.
	pub fn with_implementation(self, slot: crate::setup::PeerImplementation) -> Self {
		self.0.lock().implementation = slot;
		self
	}

	pub(super) fn receive(&self, received: ReceivedSetup) -> Result<(), crate::Error> {
		let mut state = self.0.lock();
		if ((state.client || state.address == crate::transport::Address::Uri) && received.setup.path.is_some())
			|| (state.client && (received.role_present || received.setup.token.is_some()))
		{
			return Err(crate::Error::ProtocolViolation);
		}
		state.seen = true;
		state.implementation.set(received.setup.implementation.clone());
		state.setup = Some(received.setup);
		let accept = state.accept.take();
		drop(state);
		if let Some(link) = accept {
			link.accepted();
		}
		Ok(())
	}

	/// Note whether our own SETUP offers the `ietf` extension, before any stream depends on it.
	pub fn offer_ietf(self, ietf: bool) -> Self {
		self.0.lock().ietf = ietf;
		self
	}

	/// Poll whether the `ietf` extension is negotiated: both SETUPs offer it.
	///
	/// Resolves at once when we did not offer it, and otherwise once the peer's SETUP arrives.
	pub fn poll_ietf(&self, waiter: &kio::Waiter) -> std::task::Poll<bool> {
		if !self.0.lock().ietf {
			return std::task::Poll::Ready(false);
		}
		self.poll_get(waiter, |setup| setup.ietf)
	}

	/// Whether the `ietf` extension is negotiated, or `None` before the peer's SETUP arrives.
	pub fn ietf(&self) -> Option<bool> {
		let state = self.0.lock();
		match state.ietf {
			false => Some(false),
			true => state.setup.as_ref().map(|setup| setup.ietf),
		}
	}

	/// Record the peer's SETUP.
	pub fn set(&self, setup: Setup) {
		let mut state = self.0.lock();
		state.seen = true;
		state.implementation.set(setup.implementation.clone());
		state.setup = Some(setup);
	}

	/// Claim the Setup Stream before decoding its body, refusing any second stream.
	pub fn claim(&self) -> Result<(), crate::Error> {
		let mut state = self.0.lock();
		if state.seen {
			return Err(crate::Error::ProtocolViolation);
		}
		state.seen = true;
		Ok(())
	}

	/// Poll for the peer's advertised probe level, waiting until its SETUP arrives.
	pub fn poll_probe_level(&self, waiter: &kio::Waiter) -> std::task::Poll<ProbeLevel> {
		self.poll_get(waiter, |setup| setup.probe)
	}

	/// Poll for the link cost the peer (the dialing side) declared in its SETUP.
	/// `None` when it declared none, meaning the default cost of 1.
	pub fn poll_cost(&self, waiter: &kio::Waiter) -> std::task::Poll<Option<u64>> {
		self.poll_get(waiter, |setup| setup.cost)
	}

	/// Poll for the [`Hop`](crate::Hop) id the peer declared in its SETUP `Hop`
	/// parameter. `None` when it declared none: a leaf with no identity worth excluding.
	pub fn poll_hop(&self, waiter: &kio::Waiter) -> std::task::Poll<Option<crate::Hop>> {
		self.poll_get(waiter, |setup| setup.hop)
	}

	/// Poll for a field of the peer's SETUP.
	///
	/// The peer MUST send exactly one SETUP, so this resolves once that stream is read.
	/// Pends forever if it never does; the caller is session state, dropped with the
	/// driver.
	fn poll_get<T>(&self, waiter: &kio::Waiter, f: impl FnOnce(&Setup) -> T) -> std::task::Poll<T> {
		let slot = std::task::ready!(self.0.poll(waiter, |setup| {
			if setup.setup.is_some() {
				std::task::Poll::Ready(())
			} else {
				std::task::Poll::Pending
			}
		}));
		std::task::Poll::Ready(f(slot.setup.as_ref().expect("waited for Some")))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn round_trip(msg: &Setup) -> Setup {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| Setup::decode(r, v)).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		got
	}

	#[test]
	fn empty_round_trip() {
		let msg = Setup::default();
		assert_eq!(round_trip(&msg), msg);
	}

	/// A transport exposing neither metric can't honour a `Report` claim, and the
	/// draft makes such a publisher reset any Probe Stream a subscriber opens. It
	/// must advertise `None` so the subscriber never opens one.
	#[test]
	#[cfg(feature = "async")]
	fn detect_reports_nothing_without_stats() {
		use crate::lite::test_transport::{SinkSession, SinkStats};
		let session = SinkSession::new(Default::default()).with_stats(SinkStats::default());
		assert_eq!(
			ProbeLevel::detect(&crate::driver::Transport::new(session.clone())),
			ProbeLevel::None
		);
	}

	/// Either metric alone is enough to report, since the two PROBE fields are
	/// independent on the wire.
	#[test]
	#[cfg(feature = "async")]
	fn detect_reports_with_either_metric() {
		use crate::lite::test_transport::{SinkSession, SinkStats};

		let rtt_only = SinkStats::default().with_rtt(std::time::Duration::from_millis(40));
		let session = SinkSession::new(Default::default()).with_stats(rtt_only);
		assert_eq!(
			ProbeLevel::detect(&crate::driver::Transport::new(session.clone())),
			ProbeLevel::Report
		);

		let rate_only = SinkStats::default().with_send_rate(1_000_000);
		let session = SinkSession::new(Default::default()).with_stats(rate_only);
		assert_eq!(
			ProbeLevel::detect(&crate::driver::Transport::new(session.clone())),
			ProbeLevel::Report
		);
	}

	/// Probing to a local target never reaches the wire, so a sender alone promises the
	/// peer nothing more than a report.
	#[test]
	#[cfg(feature = "async")]
	fn a_sender_still_reports() {
		use crate::lite::test_transport::{SinkSession, SinkStats};

		let stats = SinkStats::default().with_send_rate(1_000_000);
		let session = SinkSession::new(Default::default()).with_stats(stats).with_probe();
		assert_eq!(
			ProbeLevel::detect(&crate::driver::Transport::new(session.clone())),
			ProbeLevel::Report
		);
	}

	#[test]
	fn probe_levels_round_trip() {
		for probe in [ProbeLevel::None, ProbeLevel::Report, ProbeLevel::Increase] {
			let msg = Setup {
				probe,
				..Default::default()
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	#[test]
	fn cost_round_trip() {
		// Zero is a meaningful price (a free same-datacenter link), so it must survive
		// the round trip as `Some(0)` rather than collapsing into "unpriced".
		for cost in [None, Some(0), Some(1), Some(7)] {
			let msg = Setup {
				cost,
				..Default::default()
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	/// Parameter values follow the session's varint codec, not a fixed one: a value that
	/// takes the two-byte QUIC form on lite-06 fits one leading-ones byte on lite-07.
	#[test]
	fn parameter_values_use_the_version_codec() {
		let msg = Setup {
			cost: Some(100),
			..Default::default()
		};
		for (version, wire) in [
			(Version::Lite06, &[0x05, 0x01, 0x04, 0x02, 0x40, 0x64][..]),
			(Version::Lite07, &[0x04, 0x01, 0x04, 0x01, 0x64][..]),
		] {
			let buf = msg.encode_bytes(version).unwrap();
			assert_eq!(buf, wire, "{version}");
			assert_eq!(
				Setup::decode_slice(&buf, version).unwrap(),
				(msg.clone(), buf.len()),
				"{version}"
			);
		}
	}

	/// The `ietf` offer is an empty parameter that only a version with the extension sends,
	/// and an older one ignores like any unknown parameter.
	#[test]
	fn ietf_offer_is_lite07_only() {
		let msg = Setup {
			ietf: true,
			..Default::default()
		};
		let buf = msg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(Setup::decode_slice(&buf, Version::Lite07).unwrap().0, msg);
		assert!(msg.encode_bytes(Version::Lite06).is_err());

		// Length, one parameter, its 8-byte QUIC varint ID, and an empty value.
		let older = [0x0a, 0x01, 0xc0, 0, 0, 0, 0x49, 0x45, 0x54, 0x46, 0x00];
		assert_eq!(
			Setup::decode_slice(&older, Version::Lite06).unwrap().0,
			Setup::default()
		);

		// A value it does not define is refused.
		let mut valued = buf.to_vec();
		assert_eq!(valued.pop(), Some(0), "the offer ends in its empty value");
		valued.extend([1, 7]);
		valued[0] += 1;
		assert!(Setup::decode_slice(&valued, Version::Lite07).is_err());
	}

	/// The connection credential rides lite-07+, which an older version ignores like any
	/// unknown parameter, and only an untyped, non-empty token can be sent.
	#[test]
	fn token_is_lite07_only() {
		let token = crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: b"jwt".to_vec(),
		};
		let msg = Setup {
			token: Some(token.clone()),
			..Default::default()
		};
		let buf = msg.encode_bytes(Version::Lite07).unwrap();
		// Length, one parameter, its id, then the token's length and bytes.
		assert_eq!(&buf[..], [0x06, 0x01, 0x07, 0x03, b'j', b'w', b't']);
		assert_eq!(Setup::decode_slice(&buf, Version::Lite07).unwrap().0, msg);
		assert!(matches!(msg.encode_bytes(Version::Lite06), Err(EncodeError::Version)));
		assert_eq!(Setup::decode_slice(&buf, Version::Lite06).unwrap().0, Setup::default());

		for unsendable in [
			crate::setup::Token {
				// Any registered type but out-of-band, which moq-lite cannot carry.
				kind: 0x1,
				..token.clone()
			},
			crate::setup::Token {
				value: Vec::new(),
				..token
			},
		] {
			let msg = Setup {
				token: Some(unsendable),
				..Default::default()
			};
			assert!(matches!(
				msg.encode_bytes(Version::Lite07),
				Err(EncodeError::Unsupported)
			));
		}

		// An empty value carries no credential, so it is refused rather than read as none.
		assert!(Setup::decode_slice(&[0x03, 0x01, 0x07, 0x00], Version::Lite07).is_err());
	}

	/// Only the client presents a connection credential.
	#[test]
	fn a_server_token_is_a_protocol_violation() {
		let setup = Setup {
			token: Some(crate::setup::Token {
				kind: crate::setup::Token::OUT_OF_BAND,
				value: b"jwt".to_vec(),
			}),
			..Default::default()
		};
		let buf = setup.encode_bytes(Version::Lite07).unwrap();
		let received = |buf: &[u8]| ReceivedSetup::decode_slice(buf, Version::Lite07).unwrap().0;

		let client = PeerSetup::new(true, crate::transport::Address::Setup);
		assert!(matches!(
			client.receive(received(&buf)),
			Err(crate::Error::ProtocolViolation)
		));
		let server = PeerSetup::new(false, crate::transport::Address::Setup);
		server.receive(received(&buf)).unwrap();
	}

	/// Never emit a SETUP our own receiver would refuse.
	#[test]
	fn encode_enforces_the_setup_limit() {
		// Count, id, and a 4-byte length varint precede the path.
		let at_limit = crate::setup::MAX_SETUP_SIZE - 6;
		let msg = Setup {
			path: Some("a".repeat(at_limit)),
			..Default::default()
		};
		assert_eq!(round_trip(&msg), msg);

		let msg = Setup {
			path: Some("a".repeat(at_limit + 1)),
			..Default::default()
		};
		let mut buf = Vec::new();
		assert!(matches!(
			msg.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05),
			Err(EncodeError::TooLarge)
		));
		assert!(buf.is_empty());
	}

	#[test]
	fn path_round_trip() {
		let msg = Setup {
			probe: ProbeLevel::Report,
			path: Some("/room/123".to_string()),
			..Default::default()
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn implementation_round_trip() {
		for implementation in [None, Some("moq-ffi/0.4.12 moq-net-rs/0.3.10".to_string())] {
			let msg = Setup {
				implementation,
				..Default::default()
			};
			let buf = msg.encode_bytes(Version::Lite07).unwrap();
			assert_eq!(Setup::decode_slice(&buf, Version::Lite07).unwrap().0, msg);
		}
	}

	/// The value is UTF-8, so bytes that are not refuse the whole SETUP.
	#[test]
	fn implementation_must_be_utf8() {
		let msg = Setup {
			implementation: Some("ab".to_string()),
			..Default::default()
		};
		let mut buf = msg.encode_bytes(Version::Lite07).unwrap().to_vec();
		let last = buf.len() - 1;
		buf[last] = 0xff;
		assert!(matches!(
			Setup::decode_slice(&buf, Version::Lite07),
			Err(DecodeError::InvalidValue)
		));
	}

	#[test]
	fn older_versions_ignore_implementation_before_parsing_it() {
		for version in [Version::Lite05, Version::Lite06] {
			for value in [b"x/1".as_slice(), &[0xff]] {
				let mut params = Parameters::default();
				params.set_bytes(PARAM_IMPLEMENTATION, value.to_vec());
				let mut data = Vec::new();
				let mut writer = Encoder::new(&mut data, version.into());
				let prefix = writer.prefix_varint();
				params.encode(&mut writer, version).unwrap();
				writer.fill(prefix).unwrap();
				assert!(Setup::decode_slice(&data, version).unwrap().0.implementation.is_none());
			}
		}
	}

	#[test]
	fn hop_round_trip() {
		let msg = Setup {
			hop: Some(crate::Hop::new(42).unwrap()),
			..Default::default()
		};
		assert_eq!(round_trip(&msg), msg);
	}

	// A hop past 2^62 - 1 travels as 0 on a QUIC-varint version, which reads as undeclared.
	#[test]
	fn wide_hop_needs_leading_ones() {
		let msg = Setup {
			hop: Some(crate::Hop::new(u64::MAX).unwrap()),
			..Default::default()
		};
		for (version, want) in [(Version::Lite06, None), (Version::Lite07, msg.hop)] {
			let mut buf = Vec::new();
			msg.encode(&mut Encoder::new(&mut buf, version.into()), version)
				.unwrap();
			let got = crate::coding::decode_buf(&mut &buf[..], version, |r, v| Setup::decode(r, v)).unwrap();
			assert_eq!(got.hop, want, "{version:?}");
		}
	}

	// A declared id of 0 carries no identity (it cannot be excluded), so it
	// decodes as absent rather than erroring.
	#[test]
	fn hop_zero_decodes_as_none() {
		use crate::coding::Encode;

		let version = Version::Lite05;
		let mut params = Parameters::default();
		params.set_varint(super::PARAM_HOP, 0, version).unwrap();
		let mut body = Vec::new();
		params
			.encode(&mut Encoder::new(&mut body, version.into()), version)
			.unwrap();
		// Frame the body with the Message Length prefix `Setup::decode` expects.
		let mut buf = Vec::new();
		Encoder::new(&mut buf, version.into())
			.varint(body.len() as u64)
			.unwrap();
		buf.extend_from_slice(&body);
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, version, |r, v| Setup::decode(r, v)).unwrap();
		assert_eq!(got.hop, None);
	}

	#[test]
	fn empty_path_round_trips() {
		// An empty path is valid and distinct from absent only on the wire; both mean
		// the root, so a client doesn't have to special-case it.
		let msg = Setup {
			path: Some(String::new()),
			..Default::default()
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn roles_round_trip() {
		for role in [Some(Role::Publisher), Some(Role::Subscriber), None] {
			let msg = Setup {
				path: Some("/room/123".to_string()),
				role,
				..Default::default()
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	#[test]
	fn unknown_probe_level_saturates_to_increase() {
		// Frame a SETUP message carrying an unknown probe level (99) by hand: the
		// parameters body, prefixed with its length (the lite Message size prefix).
		let mut params = Parameters::default();
		params.set_varint(PARAM_PROBE, 99, Version::Lite05).unwrap();
		let mut body = Vec::new();
		params
			.encode(&mut Encoder::new(&mut body, Version::Lite05.into()), Version::Lite05)
			.unwrap();

		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite05.into())
			.varint(body.len() as u64)
			.unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| Setup::decode(r, v)).unwrap();
		assert_eq!(got.probe, ProbeLevel::Increase);
	}

	/// Roles, pinned to the draft. A swap still round trips against our own decoder but
	/// breaks every other implementation.
	#[test]
	fn role_code_points() {
		for (role, spec) in [(Role::Publisher, 1u64), (Role::Subscriber, 2)] {
			assert_eq!(role.to_code(), spec, "{role:?}");
			assert_eq!(Role::from_code(spec), Some(role));
		}
	}

	#[test]
	fn unknown_role_decodes_as_bidirectional() {
		// A role value the receiver doesn't recognize (a future extension, or an explicit
		// 0) decodes to `None` rather than failing, so a newer client can't break an older
		// server. The draft mandates this fallback.
		for code in [0u64, 9, 250] {
			let mut params = Parameters::default();
			params.set_varint(PARAM_ROLE, code, Version::Lite05).unwrap();
			let mut body = Vec::new();
			params
				.encode(&mut Encoder::new(&mut body, Version::Lite05.into()), Version::Lite05)
				.unwrap();

			let mut buf = Vec::new();
			Encoder::new(&mut buf, Version::Lite05.into())
				.varint(body.len() as u64)
				.unwrap();
			buf.extend_from_slice(&body);

			let mut slice = &buf[..];
			let got = crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| Setup::decode(r, v)).unwrap();
			assert_eq!(got.role, None, "role code {code} should decode as bidirectional");
		}
	}

	#[test]
	fn rejects_before_lite05() {
		let msg = Setup::default();
		let mut buf = Vec::new();
		assert!(matches!(
			msg.encode(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn ignores_unknown_parameters() {
		// Frame a SETUP carrying an unknown parameter ID alongside the path.
		let mut params = Parameters::default();
		params.set_bytes(PARAM_PATH, b"/foo".to_vec());
		params.set_bytes(0xbeef, b"whatever".to_vec());

		let mut body = Vec::new();
		params
			.encode(&mut Encoder::new(&mut body, Version::Lite05.into()), Version::Lite05)
			.unwrap();

		// Wrap with the message size prefix the Message impl expects.
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite05.into())
			.varint(body.len() as u64)
			.unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| Setup::decode(r, v)).unwrap();
		assert_eq!(got.path.as_deref(), Some("/foo"));
	}
}
