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

use moq_ice::{Inbound, Packet};
use str0m::ice::{IceCreds, StunMessage};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::Result;
use crate::session::advertised_candidates;

/// Per-session routing table, shared between the demux task and the accept path.
#[derive(Default)]
struct Registry {
	/// Local ICE ufrag -> session inbox. Populated at registration, the only
	/// way a brand-new peer (STUN binding request) finds its session.
	by_ufrag: HashMap<String, Arc<Entry>>,
	/// Source address -> session inbox. Cached after first contact so non-STUN
	/// packets (DTLS/RTP/RTCP, which carry no ufrag) route without parsing.
	by_addr: HashMap<SocketAddr, Arc<Entry>>,
}

struct Entry {
	ufrag: String,
	sender: mpsc::Sender<Packet>,
	wire: moq_ice::Wire,
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
		registry
			.by_addr
			.retain(|_, entry| entry.ufrag != self.ufrag && !entry.sender.is_closed());
	}
}

impl Registry {
	fn route(&mut self, len: usize, src: SocketAddr, ufrag: Option<&str>) -> Option<Arc<Entry>> {
		let entry = self.by_addr.get(&src).cloned().or_else(|| {
			let entry = self.by_ufrag.get(ufrag?).cloned()?;
			self.by_addr.insert(src, entry.clone());
			Some(entry)
		})?;
		if entry.sender.is_closed() {
			self.by_addr.remove(&src);
			return None;
		}
		// Count before the bounded inbox, even when it drops a received packet.
		// The routing lock also prevents a late bump after unregistration.
		entry.wire.received(len);
		Some(entry)
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
	pub(crate) fn register(&self) -> (IceCreds, Inbound, Registration) {
		let creds = IceCreds::new();
		let (tx, rx) = mpsc::channel(moq_ice::INBOX);
		let wire = moq_ice::Wire::default();
		let entry = Arc::new(Entry {
			ufrag: creds.ufrag.clone(),
			sender: tx,
			wire: wire.clone(),
		});
		self.registry
			.lock()
			.unwrap()
			.by_ufrag
			.insert(creds.ufrag.clone(), entry);
		let registration = Registration {
			ufrag: creds.ufrag.clone(),
			registry: self.registry.clone(),
		};
		(creds, Inbound { packets: rx, wire }, registration)
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
		// this address becomes a registry key or reaches ICE (see `moq_ice::canonical`).
		let src = moq_ice::canonical(src);
		let data = &buf[..len];

		// Cached peers need one routing lock; only a new address parses STUN.
		let entry = registry.lock().unwrap().route(data.len(), src, None);
		let entry = entry.or_else(|| {
			let ufrag = local_ufrag(data)?;
			registry.lock().unwrap().route(data.len(), src, Some(&ufrag))
		});
		let Some(entry) = entry else {
			continue;
		};

		// Bounded like a socket buffer: drop on full (WebRTC tolerates loss),
		// evict on closed (the session ended).
		match entry.sender.try_send((data.to_vec(), src)) {
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

	#[tokio::test]
	async fn wire_sources_are_per_peer_and_count_full_inbox_drops() {
		let mux = Mux::bind("127.0.0.1:0".parse().unwrap(), &[]).await.unwrap();
		let (a, inbound_a, registration_a) = mux.register();
		let (b, inbound_b, _registration_b) = mux.register();
		let source_a = inbound_a.wire.clone();
		let source_b = inbound_b.wire.clone();
		let address_a = "127.0.0.1:10001".parse().unwrap();
		let address_b = "127.0.0.1:10002".parse().unwrap();
		let mut registry = mux.registry.lock().unwrap();
		assert!(registry.route(100, address_a, Some("unknown")).is_none());
		for _ in 0..moq_ice::INBOX {
			let entry = registry.route(11, address_a, Some(&a.ufrag)).unwrap();
			entry.sender.try_send((vec![0; 11], address_a)).unwrap();
		}
		let entry = registry.route(23, address_a, None).unwrap();
		assert!(matches!(
			entry.sender.try_send((vec![0; 23], address_a)),
			Err(mpsc::error::TrySendError::Full(_))
		));
		registry.route(37, address_b, Some(&b.ufrag)).unwrap();
		drop(registry);
		assert_eq!(
			source_a.stats(),
			moq_ice::Stats {
				tx: 0,
				rx: 11 * moq_ice::INBOX as u64 + 23
			}
		);
		assert_eq!(source_b.stats(), moq_ice::Stats { tx: 0, rx: 37 });

		// Unregister even while an inbox is still held: cached addresses cannot
		// keep attributing traffic after the peer ends.
		drop(registration_a);
		let final_stats = source_a.stats();
		let mut registry = mux.registry.lock().unwrap();
		assert!(registry.route(59, address_a, None).is_none());
		assert!(registry.route(59, address_a, Some(&a.ufrag)).is_none());
		drop(registry);
		drop(inbound_a);
		assert_eq!(source_a.stats(), final_stats);
	}

	#[test]
	#[expect(
		clippy::disallowed_methods,
		clippy::disallowed_types,
		reason = "the test measures route lookup cost on the host clock"
	)]
	fn wire_routes_scale_with_peers_and_packets() {
		use std::hint::black_box;
		use std::time::Instant;

		for peers in [1, 16, 256] {
			for packets in [1_000, 10_000, 100_000] {
				let registry = Arc::new(Mutex::new(Registry::default()));
				let mut receivers = Vec::new();
				let mut sources = Vec::new();
				let mut addresses = Vec::new();
				for peer in 0..peers {
					let (sender, receiver) = mpsc::channel(1);
					let wire = moq_ice::Wire::default();
					let address = SocketAddr::from(([127, 0, 0, 1], peer as u16 + 1));
					let entry = Arc::new(Entry {
						ufrag: peer.to_string(),
						sender,
						wire: wire.clone(),
					});
					registry.lock().unwrap().by_addr.insert(address, entry);
					receivers.push(receiver);
					sources.push(wire);
					addresses.push(address);
				}
				let start = Instant::now();
				for packet in 0..packets {
					let entry = registry
						.lock()
						.unwrap()
						.by_addr
						.get(&addresses[packet % peers])
						.cloned()
						.unwrap();
					black_box(entry);
				}
				let lookup = start.elapsed();
				let start = Instant::now();
				for packet in 0..packets {
					let entry = registry
						.lock()
						.unwrap()
						.route(17, addresses[packet % peers], None)
						.unwrap();
					black_box(entry);
				}
				let counted = start.elapsed();
				eprintln!(
					"wire-route peers={peers} packets={packets} lookup_ns={} counted_ns={}",
					lookup.as_nanos(),
					counted.as_nanos()
				);
				for (peer, wire) in sources.iter().enumerate() {
					let expected = packets / peers + usize::from(peer < packets % peers);
					assert_eq!(wire.stats().rx, 17 * expected as u64);
				}
			}
		}
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
		let (creds, mut rx, registration) = mux.register();
		let wire = rx.wire.clone();

		// Dial the mux over IPv4, which a dual-stack socket accepts as v4-mapped.
		let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
		let peer_addr = peer.local_addr().unwrap();
		let request = binding_request(&creds);
		peer.send_to(&request, (Ipv4Addr::LOCALHOST, port))
			.await
			.expect("IPv4 peer must reach a dual-stack mux");

		let (_data, src) = moq_clock::timeout(Duration::from_secs(5), rx.packets.recv())
			.await
			.expect("the demux must route the IPv4 peer's binding request")
			.expect("session inbox stayed open");

		assert_eq!(src, peer_addr, "an IPv4 peer's source must arrive unmapped");
		assert!(src.is_ipv4());
		assert_eq!(wire.stats().rx, request.len() as u64);
		drop(registration);
		drop(rx);
		assert_eq!(wire.stats().rx, request.len() as u64);
	}
}
