//! Address validation tokens sent to clients

use std::{fmt, sync::Arc};

use crate::{BloomTokenLog, Duration, TokenLog};

/// Configuration for sending and handling validation tokens in incoming connections
///
/// Default values should be suitable for most internet applications.
///
/// ## QUIC Tokens
///
/// The QUIC protocol defines a concept of "[address validation][1]". Essentially, one side of a
/// QUIC connection may appear to be receiving QUIC packets from a particular remote UDP address,
/// but it will only consider that remote address "validated" once it has convincing evidence that
/// the address is not being [spoofed][2].
///
/// Validation is important primarily because of QUIC's "anti-amplification limit." This limit
/// prevents a QUIC server from sending a client more than three times the number of bytes it has
/// received from the client on a given address until that address is validated. This is designed
/// to mitigate the ability of attackers to use QUIC-based servers as reflectors in [amplification
/// attacks][3].
///
/// A path may become validated in several ways. The server is always considered validated by the
/// client. The client usually begins in an unvalidated state upon first connecting or migrating,
/// but then becomes validated through various mechanisms that usually take one network round trip.
/// However, in some cases, a client which has previously attempted to connect to a server may have
/// been given a one-time use cryptographically secured "token" that it can send in a subsequent
/// connection attempt to be validated immediately.
///
/// There are two ways these tokens can originate:
///
/// - If the server responds to an incoming connection with `retry`, a "retry token" is minted and
///   sent to the client, which the client immediately uses to attempt to connect again. Retry
///   tokens operate on short timescales, such as 15 seconds.
/// - If a client's path within an active connection is validated, the server may send the client
///   one or more "validation tokens," which the client may store for use in later connections to
///   the same server. Validation tokens may be valid for much longer lifetimes than retry token.
///
/// Validation tokens let the server validate the client's address before the handshake
/// completes, avoiding the amplification limit while sending a large handshake flight.
///
/// [1]: https://www.rfc-editor.org/rfc/rfc9000.html#section-8
/// [2]: https://en.wikipedia.org/wiki/IP_address_spoofing
/// [3]: https://en.wikipedia.org/wiki/Denial-of-service_attack#Amplification
///
/// These tokens should not be confused with "stateless reset tokens," which are similarly named
/// but entirely unrelated.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Duration after an address validation token was issued for which it's considered valid
	///
	/// This refers only to tokens sent in NEW_TOKEN frames, in contrast to retry tokens.
	/// Defaults to 2 weeks.
	pub lifetime: Duration,

	/// Number of address validation tokens sent to a client when its path is validated
	///
	/// This refers only to tokens sent in NEW_TOKEN frames, in contrast to retry tokens.
	/// Defaults to 2.
	pub sent: u32,

	/// Remembers which tokens were spent, so each is accepted once
	pub(crate) log: Arc<dyn TokenLog>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			lifetime: Duration::from_secs(2 * 7 * 24 * 60 * 60),
			sent: 2,
			log: Arc::new(BloomTokenLog::default()),
		}
	}
}

impl fmt::Debug for Config {
	fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt.debug_struct("Config")
			.field("lifetime", &self.lifetime)
			// log not debug
			.field("sent", &self.sent)
			.finish_non_exhaustive()
	}
}
