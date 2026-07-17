//! Single-UDP-port media mux for the WHIP/WHEP servers.
//!
//! str0m is sans-IO, so each [`Session`](crate::session::Session) needs UDP
//! datagrams fed to it. The naive approach (one ephemeral socket per session)
//! makes the media port unpredictable, so a deployment behind a firewall would
//! have to open the whole ephemeral range. Instead every server session shares
//! **one** UDP socket bound to a configured port; a demux task reads that socket
//! and routes each datagram to the right session.
//!
//! Routing key: ICE. A session is registered under the local ICE ufrag we mint
//! for it ([`IceCreds::new`]). The peer's first packets are STUN binding
//! requests whose USERNAME is `<our-ufrag>:<their-ufrag>`, so we parse the local
//! ufrag out of the STUN message and look the session up. Once we've seen a
//! source address we cache `addr -> session`, so subsequent DTLS/RTP/RTCP (which
//! carry no ufrag) route by address on the fast path.
//!
//! Backpressure mirrors a UDP socket buffer: each session has a bounded inbox
//! and a full inbox drops the datagram (WebRTC tolerates loss). A closed inbox
//! (session ended) evicts the address-cache entry; the ufrag entry is removed by
//! the [`Registration`] guard the accept path holds for the session's lifetime.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use str0m::ice::{IceCreds, StunMessage};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::Result;
use crate::session::{Packet, SESSION_INBOX, advertised_candidates};

/// Per-session routing table, shared between the demux task and the accept path.
#[derive(Default)]
struct Registry {
	/// Local ICE ufrag -> session inbox. Populated at registration, the only
	/// way a brand-new peer (STUN binding request) finds its session.
	by_ufrag: HashMap<String, mpsc::Sender<Packet>>,
	/// Source address -> session inbox. Cached after first contact so non-STUN
	/// packets (DTLS/RTP/RTCP, which carry no ufrag) route without parsing.
	by_addr: HashMap<SocketAddr, mpsc::Sender<Packet>>,
}

/// The shared media socket plus its demux routing table.
///
/// One per [`Server`](crate::server::Server), bound lazily on the first
/// WHIP/WHEP accept so `Server::new` can stay synchronous.
pub(crate) struct Mux {
	socket: Arc<UdpSocket>,
	registry: Arc<Mutex<Registry>>,
	/// ICE host candidates to advertise: the configured public IP(s) (or the
	/// bound address if none) paired with the shared socket's actual port.
	candidates: Vec<SocketAddr>,
}

/// Removes a session's ufrag entry (and sweeps any dead address entries) when
/// dropped. The accept path hands this to the session task, so the registration
/// lives exactly as long as the session.
pub(crate) struct Registration {
	ufrag: String,
	registry: Arc<Mutex<Registry>>,
}

impl Drop for Registration {
	fn drop(&mut self) {
		let mut registry = self.registry.lock().unwrap();
		registry.by_ufrag.remove(&self.ufrag);
		// Sweep address-cache entries whose session inbox has closed (this one,
		// and any other session that ended without another inbound packet to
		// trigger the per-packet eviction below).
		registry.by_addr.retain(|_, tx| !tx.is_closed());
	}
}

impl Mux {
	/// Bind the shared socket to `udp_bind` and spawn the demux task. The
	/// advertised candidates are `ice_candidates` (each reusing the socket's
	/// real port), or the bound address when none are configured. An unspecified
	/// bind falls back to loopback for local testing.
	pub(crate) async fn bind(udp_bind: SocketAddr, ice_candidates: &[SocketAddr]) -> Result<Self> {
		let socket = Arc::new(UdpSocket::bind(udp_bind).await?);
		let candidates = advertised_candidates(ice_candidates, socket.local_addr()?)?;

		let registry = Arc::new(Mutex::new(Registry::default()));
		tokio::spawn(demux(socket.clone(), registry.clone()));

		tracing::info!(?candidates, bound = %socket.local_addr()?, "webrtc media mux listening");
		Ok(Self {
			socket,
			registry,
			candidates,
		})
	}

	/// Mint ICE credentials for a new session and register its inbox. Returns
	/// the credentials (set on the session's [`Rtc`](str0m::Rtc) so the demux's
	/// ufrag lookup matches), the inbox receiver the session reads from, and a
	/// [`Registration`] guard the session task must hold for its lifetime.
	pub(crate) fn register(&self) -> (IceCreds, mpsc::Receiver<Packet>, Registration) {
		let creds = IceCreds::new();
		let (tx, rx) = mpsc::channel(SESSION_INBOX);
		self.registry.lock().unwrap().by_ufrag.insert(creds.ufrag.clone(), tx);
		let registration = Registration {
			ufrag: creds.ufrag.clone(),
			registry: self.registry.clone(),
		};
		(creds, rx, registration)
	}

	/// The shared socket, handed to each session for sending.
	pub(crate) fn socket(&self) -> Arc<UdpSocket> {
		self.socket.clone()
	}

	/// ICE host candidates to advertise in the SDP answer. The session tags each
	/// inbound datagram with the family-matching candidate; never empty (falls
	/// back to the bound address or loopback).
	pub(crate) fn candidates(&self) -> &[SocketAddr] {
		&self.candidates
	}
}

/// Read the shared socket forever, routing each datagram to its session.
async fn demux(socket: Arc<UdpSocket>, registry: Arc<Mutex<Registry>>) {
	let mut buf = vec![0u8; 65_535];
	loop {
		let (len, src) = match socket.recv_from(&mut buf).await {
			Ok(v) => v,
			// recv errors on UDP are typically transient (e.g. an ICMP
			// port-unreachable surfacing as ECONNREFUSED); keep serving.
			Err(err) => {
				tracing::warn!(%err, "webrtc media mux recv failed");
				continue;
			}
		};
		// A dual-stack bind reports IPv4 peers as `::ffff:a.b.c.d`; unmap before
		// this address becomes a registry key or reaches ICE (see crate::net).
		let src = crate::net::canonical(src);
		let data = &buf[..len];

		// Fast path: a source we've already paired with a session.
		let sender = registry.lock().unwrap().by_addr.get(&src).cloned();
		let sender = match sender {
			Some(sender) => Some(sender),
			// New source: only a STUN binding request (carrying the local
			// ufrag) can introduce one. Parse outside the lock.
			None => match local_ufrag(data) {
				Some(ufrag) => {
					let mut registry = registry.lock().unwrap();
					match registry.by_ufrag.get(&ufrag).cloned() {
						// Cache addr -> session so this peer's later non-STUN
						// packets route without re-parsing.
						Some(sender) => {
							registry.by_addr.insert(src, sender.clone());
							Some(sender)
						}
						None => None,
					}
				}
				None => None,
			},
		};

		let Some(sender) = sender else {
			// Unknown source and no matching ufrag: not our session, drop it.
			continue;
		};

		// Bounded like a socket buffer: drop on full (WebRTC tolerates loss),
		// evict on closed (the session ended).
		match sender.try_send((data.to_vec(), src)) {
			Ok(()) => {}
			Err(mpsc::error::TrySendError::Full(_)) => {}
			Err(mpsc::error::TrySendError::Closed(_)) => {
				registry.lock().unwrap().by_addr.remove(&src);
			}
		}
	}
}

/// Extract the local ICE ufrag from a STUN binding request, if `data` is one.
/// The USERNAME is `<local-ufrag>:<remote-ufrag>`; we route on the local half.
fn local_ufrag(data: &[u8]) -> Option<String> {
	let msg = StunMessage::parse(data).ok()?;
	if !msg.is_binding_request() {
		return None;
	}
	msg.split_username().map(|(local, _remote)| local.to_string())
}

#[cfg(test)]
mod tests {
	use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
	use std::time::Duration;

	use str0m::ice::{StunMessage, TransId};

	use super::*;

	/// SHA1-HMAC for a STUN MESSAGE-INTEGRITY, matching what str0m signs with.
	fn sha1_hmac(key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
		use aws_lc_rs::hmac;

		let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key);
		let mut ctx = hmac::Context::with_key(&key);
		for payload in payloads {
			ctx.update(payload);
		}
		let mut out = [0u8; 20];
		out.copy_from_slice(ctx.sign().as_ref());
		out
	}

	/// Forge the STUN binding request a peer opens an ICE session with, addressed
	/// to the session holding `creds`. Signed with that session's password, which
	/// is what str0m authenticates an inbound request against.
	fn binding_request(creds: &IceCreds) -> Vec<u8> {
		let username = format!("{}:peer", creds.ufrag);
		let msg = StunMessage::binding_request(&username, TransId::new(), true, 0, 0, false);
		let mut buf = vec![0u8; 1024];
		let len = msg
			.to_bytes(Some(creds.pass.as_bytes()), &mut buf, sha1_hmac)
			.expect("serialize binding request");
		buf.truncate(len);
		buf
	}

	/// An IPv4 peer reaching a dual-stack (`[::]`) mux must route to its session
	/// with its source unmapped to the `V4` it really is. Without the
	/// canonicalization the source stays `::ffff:127.0.0.1`, which pairs the peer
	/// against the wrong-family ICE host candidate.
	#[tokio::test]
	async fn dual_stack_mux_routes_an_ipv4_peer_with_a_canonical_source() {
		let bind = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
		let advertise = [
			SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
			SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
		];
		let mux = Mux::bind(bind, &advertise).await.expect("dual-stack bind");
		let port = mux.socket.local_addr().unwrap().port();
		let (creds, mut rx, _registration) = mux.register();

		// Dial the mux over IPv4, which a dual-stack socket accepts as v4-mapped.
		let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
		let peer_addr = peer.local_addr().unwrap();
		peer.send_to(&binding_request(&creds), (Ipv4Addr::LOCALHOST, port))
			.await
			.expect("IPv4 peer must reach a dual-stack mux");

		let (_data, src) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
			.await
			.expect("the demux must route the IPv4 peer's binding request")
			.expect("session inbox stayed open");

		assert_eq!(src, peer_addr, "an IPv4 peer's source must arrive unmapped");
		assert!(src.is_ipv4());
	}
}
