//! dimpl — DTLS 1.2 implementation (Sans‑IO, Sync)
//!
//! dimpl is a DTLS 1.2 implementation aimed at WebRTC. It is a Sans‑IO
//! state machine you embed into your own UDP/RTC event loop: you feed incoming
//! datagrams, poll for outgoing records or timers, and wire up certificate
//! verification and SRTP key export yourself.
//!
//! # Goals
//! - **DTLS 1.2**: Implements the DTLS handshake and record layer used by WebRTC.
//! - **Safety**: `forbid(unsafe_code)` throughout the crate.
//! - **Minimal Rust‑only deps**: Uses small, well‑maintained Rust crypto crates.
//! - **Low overhead**: Tight control over allocations and buffers; Sans‑IO integration.
//!
//! ## Non‑goals
//! - **DTLS 1.0**
//! - **Async** (the crate is Sans‑IO and event‑loop agnostic)
//! - **no_std** (at least not without allocation)
//! - **RSA**
//! - **DHE**
//!
//! ## Constructors
//!
//! - [`Dtls::new_12`][new_12] — certificate‑based
//! - [`Dtls::new_12_psk`][new_12_psk] — PSK, no certificates
//!
//! # Cryptography surface
//! - **Cipher suites (TLS 1.2 over DTLS)**
//!   - `ECDHE_ECDSA_AES256_GCM_SHA384`
//!   - `ECDHE_ECDSA_AES128_GCM_SHA256`
//!   - `ECDHE_ECDSA_CHACHA20_POLY1305_SHA256`
//! - **PSK cipher suites (TLS 1.2 over DTLS)**
//!   - `PSK_AES128_CCM_8`
//! - **AEAD**: AES‑GCM 128/256, ChaCha20‑Poly1305 (no CBC/EtM modes).
//! - **Key exchange**: ECDHE (P‑256/P‑384), X25519
//! - **Signatures**: ECDSA P‑256/SHA‑256, ECDSA P‑384/SHA‑384
//! - **DTLS‑SRTP**: Exports keying material for `SRTP_AEAD_AES_256_GCM`,
//!   `SRTP_AEAD_AES_128_GCM`, and `SRTP_AES128_CM_SHA1_80` ([RFC 5764], [RFC 7714]).
//! - **Extended Master Secret** ([RFC 7627]) is negotiated and enforced.
//!
//! ## Certificate model
//! During the handshake the engine emits
//! [`Output::PeerCert`][peer_cert] with the peer's leaf
//! certificate (DER). The crate uses that certificate to verify DTLS
//! handshake messages, but it does not perform any PKI validation. Your
//! application is responsible for validating the peer certificate according to
//! your policy (fingerprint, chain building, name/EKU checks, pinning, etc.).
//!
//! ## Sans‑IO integration model
//! Drive the engine with three calls:
//! - [`Dtls::handle_packet`][handle_packet] — feed an entire
//!   received UDP datagram.
//! - [`Dtls::poll_output`][poll_output] — drain pending output:
//!   DTLS records, timers, events.
//! - [`Dtls::handle_timeout`][handle_timeout] — trigger
//!   retransmissions/time‑based progress.
//!
//! The output is an [`Output`][output] enum with borrowed
//! references into your provided buffer:
//! - `Packet(&[u8])`: send on your UDP socket
//! - `BufferTooSmall { needed }`: resize the poll buffer and retry
//! - `Timeout(Instant)`: schedule a timer and call `handle_timeout` at/after it
//! - `Connected`: handshake complete
//! - `PeerCert(&[u8])`: peer leaf certificate (DER) — validate in your app
//! - `KeyingMaterial(KeyingMaterial, SrtpProfile)`: DTLS‑SRTP export
//! - `ApplicationData(&[u8])`: plaintext received from peer
//! - `CloseNotify`: peer sent a `close_notify` alert (graceful shutdown)
//!
//! # Error handling
//!
//! Every `Error` returned by the public API
//! ([`handle_packet`][handle_packet], [`handle_timeout`][handle_timeout],
//! `send_application_data`, and `close`) is **fatal**: the connection is no
//! longer usable and must be thrown away. The engine has no recoverable
//! error states, so the correct response is always to drop the `Dtls`
//! instance — and start a fresh handshake if you still need a connection.
//!
//! Transient, non‑fatal conditions inherent to running over an unreliable
//! transport — malformed datagrams, replayed or out‑of‑window records, and
//! other parser noise — are handled internally and never surface as an
//! `Error`. Such packets are discarded (logged at `debug!`) while the
//! connection keeps running. You therefore never need to distinguish
//! "retry" from "give up": a returned `Error` always means give up on this
//! connection.
//!
//! # Example (Sans‑IO loop)
//!
//! ```ignore
//! # #[cfg(all())]
//! # {
//! use std::sync::Arc;
//! use std::time::Instant;
//!
//! use crate::vendor::dimpl::{certificate, Config, Dtls, Output};
//!
//! // Stub I/O to keep the example focused on the state machine
//! enum Event { Udp(Vec<u8>, Instant), Timer(Instant) }
//! fn wait_next_event(_next_wake: Option<Instant>) -> Event { Event::Udp(Vec::new(), Instant::now()) }
//! fn send_udp(_bytes: &[u8]) {}
//!
//! fn example_event_loop(mut dtls: Dtls) -> Result<(), crate::vendor::dimpl::Error> {
//!     let mut next_wake: Option<Instant> = None;
//!     let mut received_packet: Option<Vec<u8>> = None;
//!     loop {
//!         // Drain engine output until we have to wait for I/O or a timer
//!         let mut out_buf = vec![0u8; 2048];
//!         loop {
//!             match dtls.poll_output(&mut out_buf) {
//!                 Output::Packet(p) => send_udp(p),
//!                 Output::BufferTooSmall { needed } => {
//!                     out_buf.resize(needed, 0);
//!                 }
//!                 Output::Timeout(t) => { next_wake = Some(t); break; }
//!                 Output::Connected => {
//!                     // DTLS established — application may start sending
//!                 }
//!                 Output::PeerCert(_der) => {
//!                     // Inspect peer leaf certificate if desired
//!                 }
//!                 Output::KeyingMaterial(_km, _profile) => {
//!                     // Provide to SRTP stack
//!                 }
//!                 Output::ApplicationData(_data) => {
//!                     // Deliver plaintext to application
//!                 }
//!                 Output::CloseNotify => {
//!                     // Peer initiated graceful shutdown — leave the event loop
//!                     return Ok(());
//!                 }
//!                 _ => {}
//!             }
//!         }
//!
//!         if let Some(packet) = received_packet.take() {
//!             dtls.handle_packet(&packet)?;
//!             continue;
//!         }
//!
//!         // Block waiting for either UDP input or the scheduled timeout
//!         match wait_next_event(next_wake) {
//!             Event::Udp(pkt, now) => {
//!                 dtls.handle_timeout(now)?;
//!                 received_packet = Some(pkt);
//!             }
//!             Event::Timer(now) => dtls.handle_timeout(now)?,
//!         }
//!     }
//! }
//!
//! fn mk_dtls_client() -> Dtls {
//!     let cert = certificate::generate_self_signed_certificate().unwrap();
//!     let cfg = Arc::new(Config::default());
//!     let mut dtls = Dtls::new_12(cfg, cert, Instant::now());
//!     dtls.set_active(true); // client role
//!     dtls
//! }
//!
//! // Putting it together
//! let dtls = mk_dtls_client();
//! let _ = example_event_loop(dtls);
//! # }
//! ```
//!
//! # Example (PSK client)
//!
//! ```ignore
//! use std::sync::Arc;
//! use std::time::Instant;
//!
//! use crate::vendor::dimpl::{Config, Dtls, PskResolver};
//!
//! struct MyPsk;
//!
//! impl PskResolver for MyPsk {
//!     fn resolve(&self, identity: &[u8]) -> Option<Vec<u8>> {
//!         if identity == b"device-01" {
//!             Some(b"shared-secret-key".to_vec())
//!         } else {
//!             None
//!         }
//!     }
//! }
//!
//! let config = Arc::new(
//!     Config::builder()
//!         .with_psk_client(b"device-01".to_vec(), Arc::new(MyPsk))
//!         .build()
//!         .unwrap(),
//! );
//!
//! let mut dtls = Dtls::new_12_psk(config, Instant::now());
//! dtls.set_active(true); // client role
//! ```
//!
//! ### MSRV
//! Rust 1.85.0
//!
//! ### Status
//! - Session resumption is not implemented (WebRTC does a full handshake on ICE restart).
//! - Renegotiation is not implemented (WebRTC does full restart).
//!
//! [new_12]: https://docs.rs/dimpl/latest/dimpl/struct.Dtls.html#method.new_12
//! [new_12_psk]: https://docs.rs/dimpl/latest/dimpl/struct.Dtls.html#method.new_12_psk
//! [peer_cert]: https://docs.rs/dimpl/latest/dimpl/enum.Output.html#variant.PeerCert
//! [handle_packet]: https://docs.rs/dimpl/latest/dimpl/struct.Dtls.html#method.handle_packet
//! [poll_output]: https://docs.rs/dimpl/latest/dimpl/struct.Dtls.html#method.poll_output
//! [handle_timeout]: https://docs.rs/dimpl/latest/dimpl/struct.Dtls.html#method.handle_timeout
//! [output]: https://docs.rs/dimpl/latest/dimpl/enum.Output.html
//! [RFC 5764]: https://www.rfc-editor.org/rfc/rfc5764
//! [RFC 7714]: https://www.rfc-editor.org/rfc/rfc7714
//! [RFC 7627]: https://www.rfc-editor.org/rfc/rfc7627
//!
#![forbid(unsafe_code)]
#![allow(unknown_lints)]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]

use std::fmt;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

mod types;
pub use types::{
	CompressionMethod, ContentType, HashAlgorithm, NamedGroup, ProtocolVersion, Sequence, SignatureAlgorithm,
};

mod dtls12;

use dtls12::{Client as Client12, Server as Server12};

mod time_tricks;

pub(crate) mod buffer;
mod window;

mod util;

mod error;
pub(crate) use error::InternalError;
pub use error::{
	CertificateError, ConfigError, CryptoError, CryptoOperation, CryptoProviderValidationError, Error,
	InvalidStateError, PskError, SecurityError, TimeoutError, UnexpectedMessageError,
};

mod config;
pub use config::{Config, ConfigBuilder, Psk, PskResolver};

pub mod certificate;

pub mod crypto;

pub use crypto::{KeyingMaterial, SrtpProfile};

mod timer;

mod rng;
pub(crate) use rng::SeededRng;

/// Certificate and private key pair.
#[derive(Clone)]
pub struct DtlsCertificate {
	/// Certificate in DER format.
	pub certificate: Vec<u8>,
	/// Private key in DER format.
	pub private_key: Vec<u8>,
}

impl fmt::Debug for DtlsCertificate {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("DtlsCertificate")
			.field("certificate", &self.certificate.len())
			.field("private_key", &self.private_key.len())
			.finish()
	}
}

/// Sans-IO DTLS endpoint (client or server).
///
/// New instances start in the **server role**. Call
/// [`set_active(true)`](Self::set_active) to switch to client before
/// the handshake begins.
///
/// Drive the state machine with [`handle_packet`](Self::handle_packet),
/// [`poll_output`](Self::poll_output), and
/// [`handle_timeout`](Self::handle_timeout).
pub struct Dtls {
	inner: Option<Inner>,
}

enum Inner {
	Client12(Client12),
	Server12(Server12),
}

impl Dtls {
	/// Create a new DTLS 1.2 instance in the server role.
	///
	/// Call [`set_active(true)`](Self::set_active) to switch to client
	/// before the handshake begins. The `now` parameter seeds the internal
	/// time tracking for timeouts and retransmissions.
	///
	/// During the handshake, the peer's leaf certificate is surfaced via
	/// [`Output::PeerCert`]. It is up to the application to validate that
	/// certificate according to its security policy.
	pub fn new_12(config: Arc<Config>, certificate: DtlsCertificate, now: Instant) -> Self {
		let inner = Inner::Server12(Server12::new(config, certificate, now));
		Dtls { inner: Some(inner) }
	}

	/// Create a new DTLS 1.2 PSK-only instance (no certificate).
	///
	/// Call [`set_active(true)`](Self::set_active) to switch to client
	/// before the handshake begins. The `config` must have a
	/// [`PskResolver`] configured, and for clients a PSK identity
	/// via [`ConfigBuilder::with_psk_client`](ConfigBuilder).
	///
	/// Panics if `config` has no PSK configured. Without PSK data the
	/// PSK suite filter would leave zero negotiable suites, so failing
	/// fast at construction is preferable to a late handshake error.
	pub fn new_12_psk(config: Arc<Config>, now: Instant) -> Self {
		assert!(
			config.psk().is_some(),
			"Dtls::new_12_psk requires a PSK configuration; \
             set one via ConfigBuilder::with_psk_client or with_psk_server"
		);
		let inner = Inner::Server12(Server12::new_psk(config, now));
		Dtls { inner: Some(inner) }
	}

	/// Returns the negotiated DTLS protocol version.
	pub fn protocol_version(&self) -> Option<ProtocolVersion> {
		Some(ProtocolVersion::DTLS1_2)
	}

	/// Return true while DTLS shutdown is in progress.
	///
	/// This becomes true after local shutdown starts, or while a received
	/// `close_notify` still needs to be reported by
	/// [`poll_output`](Self::poll_output). Callers should continue polling
	/// until this returns false or [`is_closed`](Self::is_closed) returns true.
	pub fn is_closing(&self) -> bool {
		match self.inner.as_ref() {
			Some(Inner::Client12(client)) => client.is_closing(),
			Some(Inner::Server12(server)) => server.is_closing(),
			None => false,
		}
	}

	/// Return true once DTLS shutdown is terminal for the embedder.
	///
	/// This only becomes true after no application data can be sent and no
	/// close-related packets or events remain to be drained through
	/// [`poll_output`](Self::poll_output). In particular, DTLS 1.2 can enter
	/// its internal closed state before the queued `close_notify` packet has
	/// been polled out; this method remains false until that output is drained.
	pub fn is_closed(&self) -> bool {
		match self.inner.as_ref() {
			Some(Inner::Client12(client)) => client.is_closed(),
			Some(Inner::Server12(server)) => server.is_closed(),
			None => false,
		}
	}

	/// Return true if the instance is operating in the client role.
	pub fn is_active(&self) -> bool {
		matches!(self.inner, Some(Inner::Client12(_)))
	}

	/// Switch between server and client roles.
	///
	/// Set `active` to true for client role, false for server role.
	pub fn set_active(&mut self, active: bool) {
		// unwrap is ok: the inner is only taken to convert it in place.
		self.inner = Some(match (self.inner.take().unwrap(), active) {
			(Inner::Client12(c), false) => Inner::Server12(c.into_server()),
			(Inner::Server12(s), true) => Inner::Client12(s.into_client()),
			(inner, _) => inner,
		});
	}

	/// Process an incoming DTLS datagram.
	///
	/// Refresh the logical clock with [`Self::handle_timeout`] before receiving
	/// packets at a new instant, even if no advertised timer is due. A server's
	/// overall handshake deadline starts at that time when the first structurally
	/// valid ClientHello fragment is accepted. Later body validation failures do
	/// not reset this peer's deadline. Unrelated input does not start the deadline.
	/// Poll until [`Output::Timeout`] after each mutation.
	pub fn handle_packet(&mut self, packet: &[u8]) -> Result<(), Error> {
		match self.inner.as_mut().unwrap() {
			Inner::Client12(client) => client.handle_packet(packet),
			Inner::Server12(server) => server.handle_packet(packet),
		}
	}

	/// Poll for pending output from the DTLS engine.
	///
	/// Emitting the first ClientHello fragment starts the client's handshake
	/// deadline; queuing output or returning [`Output::BufferTooSmall`] does not.
	/// Uses the logical time from [`Self::handle_timeout`]; refresh it first if
	/// time has advanced. Caller-side transport delays are outside dimpl.
	/// Continue polling until [`Output::Timeout`].
	pub fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> Output<'a> {
		match self.inner.as_mut().unwrap() {
			Inner::Client12(client) => client.poll_output(buf),
			Inner::Server12(server) => server.poll_output(buf),
		}
	}

	/// Handle time-based events such as retransmission timers.
	///
	/// Advances the logical clock for subsequent operations, even when no timer
	/// is due. Time alone does not start handshake or unsent-flight timers.
	/// Supply monotonically increasing instants and poll until [`Output::Timeout`]
	/// after each mutation. See [`ConfigBuilder::handshake_timeout`] for the
	/// overall deadline and retry policy.
	pub fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
		match self.inner.as_mut().unwrap() {
			Inner::Client12(client) => client.handle_timeout(now),
			Inner::Server12(server) => server.handle_timeout(now),
		}
	}

	/// Send application data over the established DTLS session.
	///
	/// Returns the number of bytes accepted for buffering or transmission, not
	/// delivery. Currently accepts the entire slice or none of it.
	///
	/// Data sent before handshake completion is buffered internally and returns
	/// `Ok(data.len())`. An empty slice returns `Ok(0)` on success; this does not
	/// indicate handshake readiness.
	pub fn send_application_data(&mut self, data: &[u8]) -> Result<usize, Error> {
		match self.inner.as_mut().unwrap() {
			Inner::Client12(client) => client.send_application_data(data),
			Inner::Server12(server) => server.send_application_data(data),
		}
		.map(|()| data.len())
	}

	/// Initiate graceful shutdown by sending a `close_notify` alert.
	///
	/// **Connected** (`AwaitApplicationData`): queues a `close_notify` alert;
	/// the next [`poll_output`](Self::poll_output) cycle yields it as
	/// [`Output::Packet`].
	///
	/// **Handshake in progress**: aborts immediately without sending an
	/// alert (no authenticated channel exists). Subsequent calls to
	/// [`send_application_data`](Self::send_application_data) will return
	/// an error.
	///
	/// The alert is not retransmitted (per RFC 6347 §4.2.7).
	pub fn close(&mut self) -> Result<(), Error> {
		match self.inner.as_mut().unwrap() {
			Inner::Client12(client) => client.close(),
			Inner::Server12(server) => server.close(),
		}
	}
}

impl fmt::Debug for Dtls {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let (role, state) = match &self.inner {
			Some(Inner::Client12(c)) => ("Client12", c.state_name()),
			Some(Inner::Server12(s)) => ("Server12", s.state_name()),
			None => ("None", ""),
		};
		f.debug_struct("Dtls")
			.field("role", &role)
			.field("state", &state)
			.finish()
	}
}

/// Output events produced by the DTLS engine when polled.
#[non_exhaustive]
pub enum Output<'a> {
	/// A DTLS record to transmit on the wire.
	Packet(&'a [u8]),
	/// The provided output buffer is too small for the next pending output.
	///
	/// Retry [`Dtls::poll_output`] with a buffer of at least `needed` bytes.
	/// The pending output is retained until it can be emitted.
	BufferTooSmall {
		/// Minimum buffer length required to emit the pending output.
		needed: usize,
	},
	/// Schedule a timer and call [`Dtls::handle_timeout`] at this instant.
	///
	/// This is always the last variant returned by a poll cycle.
	/// Internal state is only consistent after reaching `Timeout`.
	Timeout(Instant),
	/// The handshake completed and the connection is established.
	Connected,
	/// The peer's leaf certificate in DER encoding.
	///
	/// Applications must validate this certificate independently (chain,
	/// name/EKU checks, pinning, etc.).
	PeerCert(&'a [u8]),
	/// Extracted DTLS-SRTP keying material and selected SRTP profile.
	KeyingMaterial(KeyingMaterial, SrtpProfile),
	/// Received application data plaintext.
	ApplicationData(&'a [u8]),
	/// The peer sent a `close_notify` alert, indicating graceful connection closure.
	CloseNotify,
}

impl fmt::Debug for Output<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Packet(v) => write!(f, "Packet({})", v.len()),
			Self::BufferTooSmall { needed } => write!(f, "BufferTooSmall({needed})"),
			Self::Timeout(v) => write!(f, "Timeout({:?})", v),
			Self::Connected => write!(f, "Connected"),
			Self::PeerCert(v) => write!(f, "PeerCert({})", v.len()),
			Self::KeyingMaterial(v, p) => write!(f, "KeyingMaterial({}, {:?})", v.len(), p),
			Self::ApplicationData(v) => write!(f, "ApplicationData({})", v.len()),
			Self::CloseNotify => write!(f, "CloseNotify"),
		}
	}
}

#[cfg(test)]
mod test {
	use std::panic::UnwindSafe;

	use crate::vendor::dimpl::certificate::generate_self_signed_certificate;

	use super::*;

	fn new_instance() -> Dtls {
		let client_cert = generate_self_signed_certificate().expect("Failed to generate client cert");
		let config = Arc::new(Config::default());
		Dtls::new_12(config, client_cert, Instant::now())
	}

	fn new_instance_12_no_cookie() -> Dtls {
		let cert = generate_self_signed_certificate().expect("Failed to generate cert");
		let config = Arc::new(Config::builder().use_server_cookie(false).build().expect("config"));
		Dtls::new_12(config, cert, Instant::now())
	}

	#[test]
	fn test_dtls_default() {
		let mut dtls = new_instance();
		assert!(!dtls.is_active());
		dtls.set_active(true);
		assert!(dtls.is_active());
		dtls.set_active(false);
	}

	#[test]
	fn is_send() {
		fn is_send<T: Send>(_t: T) {}
		fn is_sync<T: Sync>(_t: T) {}
		is_send(new_instance());
		is_sync(new_instance());
	}

	#[test]
	fn is_unwind_safe() {
		fn is_unwind_safe<T: UnwindSafe>(_t: T) {}
		is_unwind_safe(new_instance());
	}

	#[test]
	fn test_protocol_version_12() {
		let dtls = new_instance();
		assert_eq!(dtls.protocol_version(), Some(ProtocolVersion::DTLS1_2));
	}

	#[test]
	#[should_panic(expected = "requires a PSK configuration")]
	fn new_12_psk_panics_without_psk_config() {
		let config = Arc::new(Config::default());
		let _ = Dtls::new_12_psk(config, Instant::now());
	}

	#[test]
	#[should_panic(expected = "Server certificate cannot be empty")]
	fn new_12_panics_on_empty_certificate() {
		let cert = generate_self_signed_certificate().expect("Failed to generate cert");
		let config = Arc::new(Config::default());
		let empty = DtlsCertificate {
			certificate: vec![],
			private_key: cert.private_key,
		};
		let _ = Dtls::new_12(config, empty, Instant::now());
	}

	fn make_record(content_type: u8, body: &[u8]) -> Vec<u8> {
		let mut pkt = Vec::with_capacity(13 + body.len());
		pkt.push(content_type);
		pkt.extend_from_slice(&[0xFE, 0xFD]); // version
		pkt.extend_from_slice(&[0x00, 0x00]); // epoch
		pkt.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // seq
		pkt.extend_from_slice(&(body.len() as u16).to_be_bytes());
		pkt.extend_from_slice(body);
		pkt
	}

	/// Build a handshake message: 12-byte header + body bytes.
	fn make_handshake(msg_type: u8, length: u32, frag_off: u32, frag_len: u32, body: &[u8]) -> Vec<u8> {
		let mut hs = Vec::with_capacity(12 + body.len());
		hs.push(msg_type);
		hs.extend_from_slice(&length.to_be_bytes()[1..]); // 3-byte length
		hs.extend_from_slice(&[0x00, 0x00]); // message_seq
		hs.extend_from_slice(&frag_off.to_be_bytes()[1..]); // 3-byte fragment_offset
		hs.extend_from_slice(&frag_len.to_be_bytes()[1..]); // 3-byte fragment_length
		hs.extend_from_slice(body);
		hs
	}

	fn dtls12_ch_body_with_extensions(extensions: &[u8]) -> Vec<u8> {
		let mut body = Vec::new();
		body.extend_from_slice(&[0xFE, 0xFD]); // client_version
		body.extend_from_slice(&[0u8; 32]); // random
		body.push(0); // session_id length
		body.push(0); // cookie length
		body.extend_from_slice(&[0x00, 0x02]); // cipher_suites length
		body.extend_from_slice(&[0xC0, 0x2B]); // ECDHE_ECDSA_AES128_GCM_SHA256
		body.push(1); // compression_methods length
		body.push(0); // null compression
		body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
		body.extend_from_slice(extensions);
		body
	}

	#[test]
	fn dtls12_server_discards_oversized_ec_point_formats_extension() {
		let mut extensions = Vec::new();
		extensions.extend_from_slice(&[0x00, 0x0B]); // ec_point_formats
		extensions.extend_from_slice(&[0x00, 0x05]); // extension body length
		extensions.extend_from_slice(&[
			0x04, // Four point formats.
			0x00, // Uncompressed.
			0x00, // Uncompressed.
			0x00, // Uncompressed.
			0x00, // Uncompressed.
		]);
		extensions.extend_from_slice(&[0x00, 0x17]); // extended_master_secret
		extensions.extend_from_slice(&[0x00, 0x00]); // empty extension body

		let body = dtls12_ch_body_with_extensions(&extensions);
		let len = body.len() as u32;
		let hs = make_handshake(0x01, len, 0, len, &body);
		let pkt = make_record(0x16, &hs);

		let mut dtls = new_instance_12_no_cookie();
		dtls.handle_packet(&pkt)
			.expect("malformed extension should be discarded");
	}

	#[test]
	fn dtls12_server_discards_trailing_ec_point_formats_extension() {
		let mut extensions = Vec::new();
		extensions.extend_from_slice(&[0x00, 0x0B]); // ec_point_formats
		extensions.extend_from_slice(&[0x00, 0x03]); // extension body length
		extensions.extend_from_slice(&[
			0x01, // One point format.
			0x00, // Uncompressed.
			0xFF, // Trailing extension body byte beyond the inner vector.
		]);
		extensions.extend_from_slice(&[0x00, 0x17]); // extended_master_secret
		extensions.extend_from_slice(&[0x00, 0x00]); // empty extension body

		let body = dtls12_ch_body_with_extensions(&extensions);
		let len = body.len() as u32;
		let hs = make_handshake(0x01, len, 0, len, &body);
		let pkt = make_record(0x16, &hs);

		let mut dtls = new_instance_12_no_cookie();
		dtls.handle_packet(&pkt)
			.expect("malformed extension should be discarded");
	}

	#[test]
	fn dtls12_server_accepts_unknown_ec_point_formats_extension() {
		let mut extensions = Vec::new();
		extensions.extend_from_slice(&[0x00, 0x0B]); // ec_point_formats
		extensions.extend_from_slice(&[0x00, 0x04]); // extension body length
		extensions.extend_from_slice(&[
			0x03, // Three point formats.
			0x02, // ANSI X9.62 compressed char2.
			0x00, // Uncompressed.
			0xFF, // Unknown/private point format.
		]);
		extensions.extend_from_slice(&[0x00, 0x17]); // extended_master_secret
		extensions.extend_from_slice(&[0x00, 0x00]); // empty extension body

		let body = dtls12_ch_body_with_extensions(&extensions);
		let len = body.len() as u32;
		let hs = make_handshake(0x01, len, 0, len, &body);
		let pkt = make_record(0x16, &hs);

		let mut dtls = new_instance_12_no_cookie();
		dtls.handle_timeout(Instant::now()).unwrap();
		dtls.handle_packet(&pkt)
			.expect("unknown ec_point_formats values should not fail server-side parsing");
	}
}

#[cfg(test)]
mod tests;
