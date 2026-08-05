//! LAN peer discovery over mDNS (DNS-SD), so MoQ processes on the same network
//! find each other with no relay, DNS, or certificate authority.
//!
//! [`Config::advertise`] registers this process as a `_moq._udp.local.` service
//! and starts browsing for the others, yielding [`Event`]s. Discovery is all
//! this does: what to do with a peer (dial it, list it, ignore it) stays with
//! the caller. [`Discovery::should_dial`] offers the pair tiebreaker so both
//! sides agree on who opens the connection.
//!
//! The advertisement carries the listener's port plus, optionally, its
//! certificate fingerprint (so a peer with a generated certificate can be
//! pinned without a CA) and its canonical node URL (so a peer with real DNS and
//! a real certificate is dialed the same way any other cluster peer is).
//!
//! Anyone can advertise on mDNS, so an unauthenticated record proves nothing:
//! an attacker can publish its own address and certificate fingerprint and be
//! discovered like any other peer. [`Config::with_secret`] closes that off. Each
//! side proves possession of a shared [`Secret`] with an HMAC-SHA256 proof bound
//! to its own nonce, fingerprint, and node identity, so a peer without the
//! secret is never reported at all, and the [`Peer::credential`] a dialer then
//! presents is bound to that one listener and replays nowhere else.
//!
//! Without a secret this is discovery only: treat the peer list as a hint and
//! authenticate the session by other means before trusting it with anything.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::str::FromStr;

use hmac::{KeyInit, Mac};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use subtle::ConstantTimeEq;
use url::Url;

/// The DNS-SD service MoQ processes advertise under.
const SERVICE_TYPE: &str = "_moq._udp.local.";

/// The TXT key carrying the listener certificate's hex SHA-256 fingerprint.
const TXT_FINGERPRINT: &str = "fp";

/// The TXT key carrying the peer's canonical node URL.
const TXT_NODE: &str = "node";

/// The TXT key carrying the random nonce the secret proofs are bound to.
const TXT_NONCE: &str = "n";

/// The TXT key carrying the advertiser's proof that it knows the secret.
const TXT_PROOF: &str = "a";

/// Domain separators for the two proofs, so the one an advertiser publishes can
/// never stand in for the one a dialer has to present.
const CONTEXT_ADVERT: &str = "moq-mdns-advert";
const CONTEXT_DIAL: &str = "moq-mdns-dial";

/// An error while advertising on or browsing the LAN.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error(ErrorKind);

#[derive(Debug, thiserror::Error)]
enum ErrorKind {
	#[error(transparent)]
	Mdns(#[from] mdns_sd::Error),

	#[error(
		"secret must be exactly 32 bytes encoded as 64 hexadecimal characters, or a path to a file containing them"
	)]
	InvalidSecret,

	#[error("secret must be 64 hexadecimal characters or a path to a file containing them, and {0} is neither")]
	SecretFile(String, #[source] std::io::Error),
}

impl From<mdns_sd::Error> for Error {
	fn from(err: mdns_sd::Error) -> Self {
		Self(ErrorKind::Mdns(err))
	}
}

type Result<T> = std::result::Result<T, Error>;

/// A 32-byte pre-shared key proving membership of a discovery group.
///
/// Peers holding different secrets (or none) never see each other. The proofs
/// are HMACs over a public nonce, so the secret never travels the network but a
/// weak one can be brute-forced offline by anyone watching it; generate a random
/// 32 bytes rather than choosing one.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 32]);

impl Secret {
	/// Decode a key from exactly 64 hexadecimal characters.
	pub fn new(secret: impl AsRef<str>) -> Result<Self> {
		let secret = secret.as_ref();
		let mut key = [0; 32];
		if secret.len() != key.len() * 2 || hex::decode_to_slice(secret, &mut key).is_err() {
			return Err(Error(ErrorKind::InvalidSecret));
		}
		Ok(Self(key))
	}

	/// Decode a key given inline, or read it from the file `value` names.
	///
	/// What a `--*-secret <HEX_OR_PATH>` flag accepts. A path that exists but
	/// holds something other than a key is an error rather than a silent fallback,
	/// and a path that doesn't exist is never created.
	pub fn load(value: impl AsRef<str>) -> Result<Self> {
		let value = value.as_ref();
		if let Ok(secret) = Self::new(value) {
			return Ok(secret);
		}
		// Not a key, so it can only have been meant as a path. A read failure here
		// is the useful error: it names the file the operator actually typed.
		let contents =
			std::fs::read_to_string(value).map_err(|err| Error(ErrorKind::SecretFile(value.to_string(), err)))?;
		Self::new(contents.trim())
	}
}

impl FromStr for Secret {
	type Err = Error;

	fn from_str(secret: &str) -> Result<Self> {
		Self::new(secret)
	}
}

impl fmt::Debug for Secret {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("Secret([redacted])")
	}
}

/// Everything a proof is bound to, so one captured off the network
/// authenticates nowhere else.
///
/// The service `instance` and `port` are the load-bearing pair: they are always
/// present, and a peer resolves under its own instance name, so an attacker that
/// copies another member's nonce and proof verbatim cannot make them verify
/// against its own record. `fingerprint` and `node` are folded in too, but only
/// as extra binding, never as the only binding, since a caller may advertise
/// neither.
struct Identity<'a> {
	instance: &'a str,
	port: u16,
	nonce: &'a str,
	fingerprint: Option<&'a str>,
	node: Option<&'a str>,
}

/// An HMAC-SHA256 proof of knowing `secret`, bound to one listener's
/// [`Identity`], plus a `context` separating the advertise and dial roles.
fn proof(secret: &Secret, context: &str, id: &Identity<'_>) -> String {
	let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&secret.0).expect("HMAC accepts any key length");
	let port = id.port.to_string();
	for field in [
		context,
		id.instance,
		&port,
		id.nonce,
		id.fingerprint.unwrap_or_default(),
		id.node.unwrap_or_default(),
	] {
		mac.update(field.as_bytes());
		mac.update(&[0]);
	}
	hex::encode(mac.finalize().into_bytes())
}

/// Constant-time equality, for comparing a credential against the expected one
/// without leaking it byte-by-byte through timing.
pub fn ct_eq(a: &str, b: &str) -> bool {
	a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// One peer's advertisement: the fields it published, plus the record they
/// arrived in. Every one of them is attacker-supplied until [`verify`] says
/// otherwise.
struct Advert<'a> {
	instance: &'a str,
	port: u16,
	fingerprint: Option<&'a str>,
	node: Option<String>,
	nonce: Option<&'a str>,
	proof: Option<&'a str>,
}

/// Check an advertisement's proof, returning the credential to present when
/// dialing that peer back.
///
/// `Err(())` means the peer belongs to a different group: it advertised no proof
/// while we require one, its proof doesn't verify, or it advertised one while we
/// run without a secret. Those cases are mutually invisible on purpose, so two
/// groups can share a network without half-connecting.
fn verify(secret: Option<&Secret>, advert: Advert<'_>) -> std::result::Result<Option<String>, ()> {
	let Some(secret) = secret else {
		return match advert.proof {
			Some(_) => Err(()),
			None => Ok(None),
		};
	};

	let (nonce, presented) = (advert.nonce.ok_or(())?, advert.proof.ok_or(())?);
	let id = Identity {
		instance: advert.instance,
		port: advert.port,
		nonce,
		fingerprint: advert.fingerprint,
		node: advert.node.as_deref(),
	};
	if !ct_eq(presented, &proof(secret, CONTEXT_ADVERT, &id)) {
		return Err(());
	}
	Ok(Some(proof(secret, CONTEXT_DIAL, &id)))
}

/// What to advertise about this process.
///
/// Build with [`new`](Self::new), then [`advertise`](Self::advertise).
#[derive(Clone, Debug)]
pub struct Config {
	port: u16,
	fingerprint: Option<String>,
	node: Option<Url>,
	secret: Option<Secret>,
}

impl Config {
	/// Advertise a MoQ listener on `port` of this host's addresses.
	pub fn new(port: u16) -> Self {
		Self {
			port,
			fingerprint: None,
			node: None,
			secret: None,
		}
	}

	/// Advertise the listener certificate's hex SHA-256 fingerprint, so peers can
	/// pin it instead of validating against a certificate authority.
	///
	/// Required to reach a listener with a generated certificate.
	pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
		self.fingerprint = Some(fingerprint.into());
		self
	}

	/// Advertise a canonical node URL, which peers dial in preference to the
	/// discovered socket address.
	///
	/// Use it when this process is reachable by name with a certificate peers
	/// already trust. It also becomes the discovery identity, so a restart keeps
	/// the same one.
	pub fn with_node(mut self, node: Url) -> Self {
		self.node = Some(node);
		self
	}

	/// Restrict discovery to peers holding the same secret, and prove this process
	/// holds it.
	///
	/// Without this, any mDNS responder is reported as a peer, including one that
	/// made up its address and fingerprint. With it, a peer is only reported once
	/// its advertised proof verifies, and [`Peer::credential`] carries the matching
	/// proof to present when dialing it.
	pub fn with_secret(mut self, secret: Secret) -> Self {
		self.secret = Some(secret);
		self
	}

	/// Register the advertisement and start browsing for peers.
	pub fn advertise(self) -> Result<Discovery> {
		Discovery::new(self)
	}
}

/// A MoQ process discovered on the LAN.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
	/// The advertised node URL when there is one, otherwise the per-run service
	/// instance. Stable across a re-resolve, and what [`Discovery::should_dial`]
	/// compares.
	pub id: String,

	/// The socket addresses the peer advertised, IPv4 first, with IPv6 scope ids
	/// preserved.
	pub addrs: Vec<SocketAddr>,

	/// The hex SHA-256 fingerprint of the peer's certificate, to pin when dialing.
	/// Absent when the peer serves a certificate it expects to be validated normally.
	pub fingerprint: Option<String>,

	/// The peer's canonical node URL, when it advertised one.
	pub node: Option<Url>,

	/// The proof of shared-secret membership to present when dialing this peer,
	/// or `None` when discovery runs without a secret.
	///
	/// Bound to this peer's advertisement, so it authenticates nowhere else. The
	/// peer expects it as its listener's [`Discovery::credential`].
	pub credential: Option<String>,
}

impl Peer {
	/// Every URL worth trying to reach this peer at, best first.
	///
	/// A canonical [`node`](Self::node) URL is the only candidate when there is
	/// one, since it comes with a name its certificate can match. Otherwise these
	/// are the advertised addresses, and there is usually more than one worth
	/// keeping: mDNS reports every interface the peer answered on, so its
	/// loopback, a container bridge, and a VPN address arrive alongside the LAN
	/// address that actually routes from here. Nothing in the record says which
	/// is which, so a dialer has to try them (collect them into [`Addrs`] and pass
	/// that to [`Client::connect`](crate::Client::connect)).
	///
	/// [`Addrs`]: crate::Addrs
	///
	/// Ordered LAN-routable first and loopback last, and an IPv6 link-local
	/// address is dropped: dialing one needs an interface scope a URL host cannot
	/// carry.
	pub fn urls(&self) -> Vec<Url> {
		if let Some(node) = &self.node {
			return vec![node.clone()];
		}

		let mut addrs = self.addrs.clone();
		// A peer's own loopback only reaches us if it shares this host, in which
		// case its LAN address reaches it too, so it is a last resort rather than
		// the default the numeric sort would otherwise make it.
		addrs.sort_by_key(|addr| (addr.ip().is_loopback(), addr.is_ipv6(), *addr));
		addrs.iter().filter_map(|addr| addr_url(*addr)).collect()
	}
}

/// A change in the set of discovered peers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Event {
	/// A peer appeared on the LAN, or re-advertised with different details.
	Found(Peer),

	/// A previously discovered peer (by [`Peer::id`]) stopped advertising.
	Lost(String),
}

/// Advertises this process on the LAN and discovers the others.
///
/// Dropping it stops advertising and browsing.
pub struct Discovery {
	/// Our own service instance, to recognize and skip our own record.
	instance: String,
	/// Our identity for the [`should_dial`] tiebreaker: the node URL, or `instance`.
	id: String,
	/// The shared secret, when membership is restricted.
	secret: Option<Secret>,
	/// The proof an inbound peer must present, mirroring what we advertised.
	credential: Option<String>,
	/// Service instance -> peer id, so a `Lost` event can name what a `Found` did.
	peers: HashMap<String, String>,
	daemon: ServiceDaemon,
	events: mdns_sd::Receiver<ServiceEvent>,
}

impl Discovery {
	fn new(config: Config) -> Result<Self> {
		let instance = format!("{:016x}", rand::random::<u64>());
		let id = match &config.node {
			Some(node) => node.to_string(),
			None => instance.clone(),
		};

		let mut txt = HashMap::new();
		if let Some(fingerprint) = &config.fingerprint {
			txt.insert(TXT_FINGERPRINT.to_string(), fingerprint.clone());
		}
		if let Some(node) = &config.node {
			txt.insert(TXT_NODE.to_string(), node.to_string());
		}

		// Publish a proof that we hold the secret, and keep the matching dial proof
		// as what an inbound peer has to present. Both are bound to this nonce and
		// this listener, so neither authenticates anywhere else.
		let credential = config.secret.as_ref().map(|secret| {
			let nonce = format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>());
			let node = config.node.as_ref().map(Url::to_string);
			let id = Identity {
				instance: &instance,
				port: config.port,
				nonce: &nonce,
				fingerprint: config.fingerprint.as_deref(),
				node: node.as_deref(),
			};
			let advert = proof(secret, CONTEXT_ADVERT, &id);
			let dial = proof(secret, CONTEXT_DIAL, &id);
			txt.insert(TXT_PROOF.to_string(), advert);
			txt.insert(TXT_NONCE.to_string(), nonce);
			dial
		});

		let daemon = ServiceDaemon::new()?;
		let service = ServiceInfo::new(
			SERVICE_TYPE,
			&instance,
			&format!("{instance}.local."),
			"",
			config.port,
			txt,
		)?
		.enable_addr_auto();
		daemon.register(service)?;
		let events = daemon.browse(SERVICE_TYPE)?;

		tracing::info!(%id, port = config.port, "advertising on the LAN");
		Ok(Self {
			instance,
			id,
			secret: config.secret,
			credential,
			peers: HashMap::new(),
			daemon,
			events,
		})
	}

	/// This process's discovery identity, as peers see it.
	pub fn id(&self) -> &str {
		&self.id
	}

	/// The proof an inbound peer must present to prove it saw this advertisement
	/// and holds the secret, or `None` when discovery runs without one.
	///
	/// Compare an inbound session's credential against this with
	/// [`verify_credential`](Self::verify_credential).
	pub fn credential(&self) -> Option<&str> {
		self.credential.as_deref()
	}

	/// Whether `presented` is the credential this listener expects.
	///
	/// Always true without a secret, since there is then nothing to prove.
	pub fn verify_credential(&self, presented: Option<&str>) -> bool {
		match (&self.credential, presented) {
			(None, _) => true,
			(Some(expected), Some(presented)) => ct_eq(expected, presented),
			(Some(_), None) => false,
		}
	}

	/// The next discovery event, or `None` once discovery shuts down.
	///
	/// A peer already reported may be reported again when its advertisement
	/// changes or is periodically re-resolved, so compare against what you have
	/// rather than assuming each [`Event::Found`] is new.
	pub async fn recv(&mut self) -> Option<Event> {
		while let Ok(event) = self.events.recv_async().await {
			match event {
				ServiceEvent::ServiceResolved(info) => {
					let Some(instance) = instance_id(&info.fullname) else {
						continue;
					};
					if instance == self.instance {
						continue;
					}

					let node = info
						.txt_properties
						.get_property_val_str(TXT_NODE)
						.and_then(|node| Url::parse(node).ok());
					let fingerprint = info.txt_properties.get_property_val_str(TXT_FINGERPRINT);

					// Everything in this record is attacker-supplied until the proof
					// says otherwise, so a peer that can't produce one is not reported
					// at all rather than reported and left for the caller to filter.
					let advert = Advert {
						instance,
						port: info.port,
						fingerprint,
						node: node.as_ref().map(Url::to_string),
						nonce: info.txt_properties.get_property_val_str(TXT_NONCE),
						proof: info.txt_properties.get_property_val_str(TXT_PROOF),
					};
					let credential = match verify(self.secret.as_ref(), advert) {
						Ok(credential) => credential,
						Err(()) => {
							tracing::debug!(peer = %instance, "ignoring a peer that failed the membership check");
							continue;
						}
					};

					let id = match &node {
						Some(node) => node.to_string(),
						None => instance.to_string(),
					};

					let mut addrs: Vec<SocketAddr> = info
						.addresses
						.iter()
						.map(|addr| discovered_addr(addr, info.port))
						.collect();
					// Deterministic order, preferring IPv4 when both families are advertised.
					addrs.sort_by_key(|addr| (addr.is_ipv6(), *addr));

					self.peers.insert(instance.to_string(), id.clone());
					return Some(Event::Found(Peer {
						id,
						addrs,
						fingerprint: fingerprint.map(str::to_string),
						node,
						credential,
					}));
				}
				ServiceEvent::ServiceRemoved(_ty, fullname) => {
					let Some(instance) = instance_id(&fullname) else {
						continue;
					};
					let Some(id) = self.peers.remove(instance) else {
						continue;
					};
					// A peer that restarted advertises a fresh instance under the same
					// node URL, so it is only lost once the last instance goes away.
					if self.peers.values().any(|other| other == &id) {
						continue;
					}
					return Some(Event::Lost(id));
				}
				_ => continue,
			}
		}
		None
	}

	/// Whether this process dials `remote`, or waits for `remote` to dial it.
	///
	/// Both sides of a pair discover each other, and a MoQ session is
	/// bidirectional, so one connection suffices: the lower id dials and the
	/// higher accepts. Deterministic on both sides with no coordination.
	pub fn should_dial(&self, remote: &str) -> bool {
		should_dial(&self.id, remote)
	}
}

impl Drop for Discovery {
	fn drop(&mut self) {
		// Best-effort goodbye; peers otherwise learn of the exit via TTL expiry.
		self.daemon.shutdown().ok();
	}
}

/// The [`Discovery::should_dial`] tiebreaker: the lower id dials.
fn should_dial(local: &str, remote: &str) -> bool {
	local < remote
}

/// Turn an mDNS address into a socket address without discarding an IPv6
/// interface scope.
fn discovered_addr(addr: &mdns_sd::ScopedIp, port: u16) -> SocketAddr {
	match addr {
		mdns_sd::ScopedIp::V6(addr) => scoped_addr(IpAddr::V6(*addr.addr()), port, addr.scope_id().index),
		_ => scoped_addr(addr.to_ip_addr(), port, 0),
	}
}

fn scoped_addr(ip: IpAddr, port: u16, scope_id: u32) -> SocketAddr {
	match ip {
		IpAddr::V4(ip) => SocketAddr::new(IpAddr::V4(ip), port),
		IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope_id)),
	}
}

/// The URL that reaches `addr`, or `None` for an address a URL host cannot express.
fn addr_url(addr: SocketAddr) -> Option<Url> {
	let url = match addr.ip() {
		IpAddr::V4(ip) => format!("moqt://{ip}:{}", addr.port()),
		// A link-local address is only meaningful together with the interface it
		// was seen on, and a URL host has nowhere to put that scope.
		IpAddr::V6(ip) if ip.is_unicast_link_local() => return None,
		IpAddr::V6(ip) => format!("moqt://[{ip}]:{}", addr.port()),
	};
	url.parse().ok()
}

/// The instance portion of a DNS-SD fullname like `<instance>._moq._udp.local.`.
fn instance_id(fullname: &str) -> Option<&str> {
	fullname.strip_suffix(SERVICE_TYPE)?.strip_suffix('.')
}

#[cfg(test)]
mod tests {
	use super::*;

	const KEY_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
	const KEY_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

	/// Exactly one side of every pair dials: the lower id. Symmetric ids never
	/// dial themselves.
	#[test]
	fn tiebreak_is_asymmetric() {
		assert!(should_dial("aaaa", "bbbb"));
		assert!(!should_dial("bbbb", "aaaa"));
		assert!(!should_dial("aaaa", "aaaa"));
	}

	#[test]
	fn instance_id_strips_the_service_suffix() {
		assert_eq!(
			instance_id("0123456789abcdef._moq._udp.local."),
			Some("0123456789abcdef")
		);
		assert_eq!(instance_id("weird name._moq._udp.local."), Some("weird name"));
		assert_eq!(instance_id("not-a-moq-service._http._tcp.local."), None);
	}

	#[test]
	fn secret_requires_exactly_32_hex_bytes_and_redacts_debug() {
		for invalid in ["", "00", &"x".repeat(64), &"00".repeat(33)] {
			assert!(Secret::new(invalid).is_err(), "{invalid:?} must not parse");
		}
		assert_eq!(
			format!("{:?}", Secret::new(KEY_A).expect("valid")),
			"Secret([redacted])"
		);
	}

	fn identity<'a>(instance: &'a str, port: u16, fingerprint: Option<&'a str>, node: Option<&'a str>) -> Identity<'a> {
		Identity {
			instance,
			port,
			nonce: "nonce",
			fingerprint,
			node,
		}
	}

	/// Each proof is bound to the secret, the role, and every field identifying
	/// the listener, so one captured anywhere stands in nowhere else.
	#[test]
	fn proofs_are_bound_to_one_listener() {
		let a = Secret::new(KEY_A).expect("valid secret");
		let b = Secret::new(KEY_B).expect("valid secret");
		let base = proof(
			&a,
			CONTEXT_DIAL,
			&identity("inst", 443, Some("fp1"), Some("moqt://node-a")),
		);

		assert_eq!(
			base,
			proof(
				&a,
				CONTEXT_DIAL,
				&identity("inst", 443, Some("fp1"), Some("moqt://node-a"))
			)
		);
		for other in [
			// A different role, secret, or nonce.
			proof(
				&a,
				CONTEXT_ADVERT,
				&identity("inst", 443, Some("fp1"), Some("moqt://node-a")),
			),
			proof(
				&b,
				CONTEXT_DIAL,
				&identity("inst", 443, Some("fp1"), Some("moqt://node-a")),
			),
			proof(
				&a,
				CONTEXT_DIAL,
				&Identity {
					nonce: "other",
					..identity("inst", 443, Some("fp1"), Some("moqt://node-a"))
				},
			),
			// A different listener, by any of the four fields that name one.
			proof(
				&a,
				CONTEXT_DIAL,
				&identity("other", 443, Some("fp1"), Some("moqt://node-a")),
			),
			proof(
				&a,
				CONTEXT_DIAL,
				&identity("inst", 4443, Some("fp1"), Some("moqt://node-a")),
			),
			proof(
				&a,
				CONTEXT_DIAL,
				&identity("inst", 443, Some("fp2"), Some("moqt://node-a")),
			),
			proof(
				&a,
				CONTEXT_DIAL,
				&identity("inst", 443, Some("fp1"), Some("moqt://node-b")),
			),
			proof(&a, CONTEXT_DIAL, &identity("inst", 443, Some("fp1"), None)),
			proof(&a, CONTEXT_DIAL, &identity("inst", 443, None, Some("moqt://node-a"))),
		] {
			assert_ne!(base, other);
			assert!(!ct_eq(&base, &other));
		}

		// Each field ends with a NUL, so shifting a boundary between two NUL-free
		// fields cannot produce the same input.
		assert_ne!(
			proof(&a, CONTEXT_DIAL, &identity("ab", 1, Some("c"), None)),
			proof(&a, CONTEXT_DIAL, &identity("a", 1, Some("bc"), None))
		);
		assert!(!ct_eq("abc", "ab"));
	}

	/// Loopback is a last resort, link-local is unusable, and everything else is
	/// kept: nothing in an mDNS record says which interface routes from here.
	#[test]
	fn every_routable_address_is_a_candidate() {
		let peer = Peer {
			id: "peer".into(),
			// As advertised: loopback sorts numerically ahead of the LAN address,
			// and a container bridge sits between them.
			addrs: [
				"127.0.0.1:4443",
				"172.17.0.1:4443",
				"192.168.1.5:4443",
				"[fe80::1]:4443",
			]
			.into_iter()
			.map(|addr| addr.parse().expect("valid address"))
			.collect(),
			fingerprint: None,
			node: None,
			credential: None,
		};

		let urls: Vec<String> = peer.urls().into_iter().map(String::from).collect();
		assert_eq!(
			urls,
			[
				"moqt://172.17.0.1:4443",
				"moqt://192.168.1.5:4443",
				"moqt://127.0.0.1:4443",
			],
			"loopback last, link-local dropped, the rest kept for failover"
		);
	}

	/// The canonical node URL wins outright: it comes with a name the peer's
	/// certificate can match, so there is nothing to fail over between.
	#[test]
	fn a_node_url_is_the_only_candidate() {
		let mut peer = Peer {
			id: "peer".into(),
			addrs: vec!["192.168.1.5:4443".parse().expect("valid address")],
			fingerprint: None,
			node: Some("https://peer.example.com".parse().expect("valid url")),
			credential: None,
		};
		assert_eq!(peer.urls(), ["https://peer.example.com/".parse().expect("valid url")]);

		peer.node = None;
		assert_eq!(peer.urls(), ["moqt://192.168.1.5:4443".parse().expect("valid url")]);

		peer.addrs = vec!["[fe80::1]:4443".parse().expect("valid address")];
		assert!(peer.urls().is_empty(), "a link-local address is not dialable");
	}

	/// One peer's advertisement, owning its strings so [`Advert`] can borrow them.
	#[derive(Clone)]
	struct Advertised {
		instance: String,
		port: u16,
		fingerprint: Option<String>,
		node: Option<String>,
		nonce: String,
		proof: String,
	}

	impl Advertised {
		/// What an honest holder of `secret` publishes about itself.
		fn new(secret: &Secret, instance: &str, fingerprint: Option<&str>, node: Option<&str>) -> Self {
			let (nonce, port) = ("nonce".to_string(), 443);
			let id = Identity {
				instance,
				port,
				nonce: &nonce,
				fingerprint,
				node,
			};
			Self {
				instance: instance.to_string(),
				port,
				fingerprint: fingerprint.map(str::to_string),
				node: node.map(str::to_string),
				proof: proof(secret, CONTEXT_ADVERT, &id),
				nonce,
			}
		}

		fn advert(&self) -> Advert<'_> {
			Advert {
				instance: &self.instance,
				port: self.port,
				fingerprint: self.fingerprint.as_deref(),
				node: self.node.clone(),
				nonce: Some(&self.nonce),
				proof: Some(&self.proof),
			}
		}
	}

	/// The membership decision, every branch, without touching the network.
	///
	/// This is the check that stops an attacker from being dialed at all: it can
	/// advertise any address and fingerprint it likes, but without the secret it
	/// cannot produce a proof over them, so it is never reported as a peer and
	/// never receives a credential.
	#[test]
	fn verify_admits_only_the_secret_holder() {
		let ours = Secret::new(KEY_A).expect("valid secret");
		let theirs = Secret::new(KEY_B).expect("valid secret");

		// The holder is admitted, and gets the proof that peer will check.
		let honest = Advertised::new(&ours, "honest", Some("fp"), None);
		assert_eq!(
			verify(Some(&ours), honest.advert()).expect("admitted"),
			Some(proof(&ours, CONTEXT_DIAL, &identity("honest", 443, Some("fp"), None)))
		);

		// A peer proving a different secret is invisible.
		assert!(
			verify(
				Some(&ours),
				Advertised::new(&theirs, "honest", Some("fp"), None).advert()
			)
			.is_err()
		);

		// So is one with no proof, or a proof with no nonce to bind it.
		let mut stripped = honest.advert();
		stripped.proof = None;
		assert!(verify(Some(&ours), stripped).is_err(), "no proof");
		let mut stripped = honest.advert();
		stripped.nonce = None;
		assert!(verify(Some(&ours), stripped).is_err(), "no nonce");

		// Without a secret, an unproven peer is fine and a proven one is not ours,
		// so the two groups stay mutually invisible instead of half-connecting.
		let bare = Advert {
			instance: "bare",
			port: 443,
			fingerprint: Some("fp"),
			node: None,
			nonce: None,
			proof: None,
		};
		assert_eq!(verify(None, bare), Ok(None));
		assert!(verify(None, honest.advert()).is_err());
	}

	/// An eavesdropper cannot lift a member's nonce and proof into its own
	/// advertisement. Both TXT values are public, so the only thing stopping a
	/// verbatim copy is that the proof is bound to the record it arrived in.
	///
	/// Every combination matters, including the one where a caller advertises
	/// neither a fingerprint nor a node URL: with nothing else to bind to, the
	/// service instance and port are what make the proof non-transferable.
	#[test]
	fn a_stolen_proof_does_not_transfer() {
		let ours = Secret::new(KEY_A).expect("valid secret");

		for (fingerprint, node) in [
			(None, None),
			(Some("victim-fp"), None),
			(None, Some("moqt://victim.example")),
			(Some("victim-fp"), Some("moqt://victim.example")),
		] {
			let victim = Advertised::new(&ours, "victim", fingerprint, node);
			assert!(verify(Some(&ours), victim.advert()).is_ok(), "the victim is a member");

			// The attacker republishes the stolen nonce and proof under its own
			// service instance, keeping everything else it can copy.
			let mut thief = victim.clone();
			thief.instance = "thief".to_string();
			assert!(
				verify(Some(&ours), thief.advert()).is_err(),
				"a proof lifted onto another instance must not verify ({fingerprint:?}, {node:?})"
			);

			// Or on the victim's instance but its own port, which is what a second
			// listener on the same host would look like.
			let mut thief = victim.clone();
			thief.port = 4443;
			assert!(
				verify(Some(&ours), thief.advert()).is_err(),
				"a proof lifted onto another port must not verify ({fingerprint:?}, {node:?})"
			);

			// And the two it could otherwise swap to redirect the dial.
			let mut thief = victim.clone();
			thief.fingerprint = Some("thief-fp".to_string());
			assert!(verify(Some(&ours), thief.advert()).is_err(), "another fingerprint");
			let mut thief = victim.clone();
			thief.node = Some("moqt://thief.example".to_string());
			assert!(verify(Some(&ours), thief.advert()).is_err(), "another node");
		}
	}

	/// Advertise on a port nothing listens on, purely to exercise the record.
	fn discovery(secret: Option<&str>, fingerprint: &str) -> Discovery {
		let mut config = Config::new(4443).with_fingerprint(fingerprint);
		if let Some(secret) = secret {
			config = config.with_secret(Secret::new(secret).expect("valid secret"));
		}
		config.advertise().expect("advertise")
	}

	/// Wait for the peer advertising `fingerprint`, or give up.
	async fn find(discovery: &mut Discovery, fingerprint: &str) -> Peer {
		let wait = async {
			loop {
				if let Some(Event::Found(peer)) = discovery.recv().await
					&& peer.fingerprint.as_deref() == Some(fingerprint)
				{
					return peer;
				}
			}
		};
		tokio::time::timeout(std::time::Duration::from_secs(30), wait)
			.await
			.unwrap_or_else(|_| panic!("timed out discovering {fingerprint}"))
	}

	/// Two processes sharing a secret find each other over real mDNS, and each
	/// ends up holding exactly the credential the other expects.
	///
	/// This covers the plumbing: that the nonce and proof survive the TXT record
	/// and that both sides derive matching credentials from them. It deliberately
	/// does not try to show the rejection half. Over multicast, "never reported"
	/// is indistinguishable from "the record never arrived", so such a test would
	/// pass whether or not the filter ran. [`verify_admits_only_the_secret_holder`]
	/// and [`a_stolen_proof_does_not_transfer`] prove that half deterministically.
	///
	/// Ignored because it multicasts on the host network, which CI runners may
	/// block; run it by hand when touching discovery:
	/// `just rs test -p moq-native --features mdns --run-ignored ignored-only`.
	#[tokio::test]
	#[ignore = "needs multicast on the host network; run manually"]
	async fn a_shared_secret_carries_through_real_mdns() {
		let mut ours = discovery(Some(KEY_A), "ours");
		let mut theirs = discovery(Some(KEY_A), "theirs");

		let (on_ours, on_theirs) = tokio::join!(find(&mut ours, "theirs"), find(&mut theirs, "ours"));

		// Each side computed the credential the other will check, and neither the
		// secret nor the credential ever crossed the network.
		assert!(theirs.verify_credential(on_ours.credential.as_deref()));
		assert!(ours.verify_credential(on_theirs.credential.as_deref()));
		assert!(!ours.verify_credential(on_ours.credential.as_deref()), "not its own");
		assert!(!ours.verify_credential(None));

		assert_ne!(
			ours.should_dial(&on_ours.id),
			theirs.should_dial(&on_theirs.id),
			"exactly one side of the pair dials"
		);
	}
}
