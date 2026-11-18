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
//! ufrag out of the STUN message and look the session up. A binding request
//! signed with that session's password caches `addr -> session`, so subsequent
//! DTLS/RTP/RTCP (which carry no ufrag) route by address.
//!
//! A binding request always routes by its ufrag, but one for an address another
//! live session holds only claims it: anyone holding a session can sign a request,
//! and a spoofed source proves nothing. The claim takes the address once the
//! claimant's own ICE connects on a datagram from it that proves the peer is there
//! ([`Route::confirm`]): the answer to a check sent to that address, which only it
//! saw (STUN responses go to both sessions meanwhile), or once the holder ends. So
//! a peer that re-offers from the same address reaches its new session, and neither
//! a forged request nor a session answering its own check from a spoofed source
//! moves anyone's traffic.
//!
//! Backpressure mirrors a UDP socket buffer: each session has a bounded inbox
//! and a full inbox drops the datagram (WebRTC tolerates loss). A closed inbox
//! (session ended) evicts the address-cache entry; the ufrag entry is removed by
//! the [`Registration`] guard the accept path holds for the session's lifetime.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::ice::{Inbound, Packet};
use str0m::ice::{IceCreds, StunMessage};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::Result;
use crate::session::advertised_candidates;

/// Per-session routing table, shared between the demux task and the accept path.
struct Registry {
	/// Checks a binding request's MESSAGE-INTEGRITY, as str0m does.
	hmac: &'static dyn str0m::crypto::Sha1HmacProvider,
	/// Local ICE ufrag -> session inbox. Populated at registration, the only
	/// way a brand-new peer (STUN binding request) finds its session.
	by_ufrag: HashMap<String, Arc<Entry>>,
	/// Source address -> session inbox. Cached after first contact so non-STUN
	/// packets (DTLS/RTP/RTCP, which carry no ufrag) route without parsing.
	by_addr: HashMap<SocketAddr, Arc<Entry>>,
	/// Source address -> the session a signed binding request claimed it for while
	/// another session holds it, until [`Route::confirm`] or the holder's end.
	claims: HashMap<SocketAddr, Arc<Entry>>,
}

struct Entry {
	ufrag: String,
	/// The session's ICE password, which only its peer can sign a binding request with.
	pass: String,
	sender: mpsc::Sender<Packet>,
}

/// The shared media socket plus its demux routing table.
///
/// One per [`Server`](crate::server::Server), bound when the server is.
pub(crate) struct Mux {
	socket: Arc<UdpSocket>,
	/// The address `socket` is bound to.
	local: SocketAddr,
	registry: Arc<Mutex<Registry>>,
	/// ICE host candidates to advertise: the configured addresses (or the
	/// bound address if none).
	candidates: Vec<SocketAddr>,
	/// The task reading `socket`, which holds it open until aborted.
	demux: tokio::task::JoinHandle<()>,
}

impl Drop for Mux {
	fn drop(&mut self) {
		// Release the port once the server and every session it accepted are gone.
		self.demux.abort();
	}
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
		registry
			.claims
			.retain(|_, entry| entry.ufrag != self.ufrag && !entry.sender.is_closed());
	}
}

/// A session's handle on its routing, which it confirms an address on.
pub(crate) struct Route {
	ufrag: String,
	registry: Arc<Mutex<Registry>>,
}

impl Route {
	/// Take `src` if this session claimed it: its ICE connected on a datagram that proves
	/// the peer is at `src` (see `ice::agent::is_symmetric`).
	pub(crate) fn confirm(&self, src: SocketAddr) {
		let mut registry = self.registry.lock().unwrap();
		if registry.claims.get(&src).is_some_and(|claim| claim.ufrag == self.ufrag) {
			let claim = registry.claims.remove(&src).expect("the claim was found");
			registry.by_addr.insert(src, claim);
		}
	}
}

impl Registry {
	/// The sessions a datagram from `src` goes to.
	///
	/// A binding request goes to the session it names. Signed with that session's password,
	/// it takes `src` if no live session holds it, or else claims it. A forged one moves
	/// nothing, and the named session drops it. A STUN answer goes to both the holder and
	/// the claimant, since either may have sent the check; anything else to the holder.
	fn route(&mut self, src: SocketAddr, datagram: &Datagram) -> [Option<Arc<Entry>>; 2] {
		match datagram {
			Datagram::Request(request) => {
				let named = (request.split_username())
					.and_then(|(local, _remote)| self.by_ufrag.get(local))
					.cloned();
				let Some(entry) = named else {
					return [self.holder(src), None];
				};
				let hmac = self.hmac;
				if request.verify(entry.pass.as_bytes(), |key, payloads| hmac.sha1_hmac(key, payloads)) {
					match self.holder(src) {
						None => {
							self.by_addr.insert(src, entry.clone());
						}
						Some(holder) if Arc::ptr_eq(&holder, &entry) => {
							self.claims.remove(&src);
						}
						Some(_) => {
							self.claims.insert(src, entry.clone());
						}
					}
				}
				[Some(entry), None]
			}
			Datagram::Answer => [self.holder(src), self.claims.get(&src).cloned()],
			Datagram::Other => [self.holder(src), None],
		}
	}

	/// The live session holding `src`: its holder, or once that ends, its claimant.
	fn holder(&mut self, src: SocketAddr) -> Option<Arc<Entry>> {
		if let Some(entry) = self.by_addr.get(&src)
			&& !entry.sender.is_closed()
		{
			return Some(entry.clone());
		}
		self.by_addr.remove(&src);
		let claim = self.claims.remove(&src).filter(|claim| !claim.sender.is_closed())?;
		self.by_addr.insert(src, claim.clone());
		Some(claim)
	}
}

/// What a datagram is, as far as routing it goes.
enum Datagram<'a> {
	/// A STUN binding request, which names its session.
	Request(Box<StunMessage<'a>>),
	/// Any other STUN message, such as the answer to a check.
	Answer,
	/// DTLS, RTP, or RTCP, which carries no session.
	Other,
}

impl<'a> Datagram<'a> {
	fn parse(data: &'a [u8]) -> Self {
		// Only STUN starts with 0..=3 (RFC 7983), so media skips the parse.
		if data.first().is_none_or(|first| *first > 3) {
			return Self::Other;
		}
		match StunMessage::parse(data) {
			Ok(message) if message.is_binding_request() => Self::Request(Box::new(message)),
			_ => Self::Answer,
		}
	}
}

impl Mux {
	/// Bind the shared socket to `udp_bind` and spawn the demux task. The
	/// advertised candidates are `ice_candidates` as given (a port of 0 taking
	/// the bound port), or the bound address when none are configured. An
	/// unspecified bind falls back to loopback for local testing.
	pub(crate) async fn bind(udp_bind: SocketAddr, ice_candidates: &[SocketAddr]) -> Result<Self> {
		let socket = Arc::new(UdpSocket::bind(udp_bind).await?);
		let local = socket.local_addr()?;
		let candidates = advertised_candidates(ice_candidates, local)?;

		moq_crypto::cpu::check()?;
		// rind-str0m repeats the check above, so its error is unreachable here.
		let crypto = rind_str0m::default_provider().expect("the CPU check passed");
		let registry = Arc::new(Mutex::new(Registry {
			hmac: crypto.sha1_hmac_provider,
			by_ufrag: HashMap::new(),
			by_addr: HashMap::new(),
			claims: HashMap::new(),
		}));
		let demux = tokio::spawn(demux(socket.clone(), registry.clone()));

		tracing::info!(?candidates, bound = %local, "webrtc media mux listening");
		Ok(Self {
			socket,
			local,
			registry,
			candidates,
			demux,
		})
	}

	/// The address the shared socket is bound to.
	pub(crate) fn local_addr(&self) -> SocketAddr {
		self.local
	}

	/// Mint ICE credentials for a new session and register its inbox. Returns
	/// the credentials (set on the session's [`Rtc`](str0m::Rtc) so the demux's
	/// ufrag lookup matches), the inbox receiver the session reads from, and a
	/// [`Registration`] guard the session task must hold for its lifetime.
	pub(crate) fn register(&self) -> (IceCreds, Inbound, Registration) {
		let creds = IceCreds::new();
		let (tx, rx) = mpsc::channel(crate::ice::INBOX);
		let entry = Arc::new(Entry {
			ufrag: creds.ufrag.clone(),
			pass: creds.pass.clone(),
			sender: tx,
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
		let route = Route {
			ufrag: creds.ufrag.clone(),
			registry: self.registry.clone(),
		};
		let inbound = Inbound {
			packets: rx,
			route: Some(route),
		};
		(creds, inbound, registration)
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
		// this address becomes a registry key or reaches ICE (see `crate::ice::canonical`).
		let src = crate::ice::canonical(src);
		let data = &buf[..len];

		let datagram = Datagram::parse(data);
		let targets = registry.lock().unwrap().route(src, &datagram);
		for entry in targets.into_iter().flatten() {
			// Bounded like a socket buffer: drop on full, as WebRTC tolerates loss. A closed
			// inbox is evicted on its address's next route.
			let _ = entry.sender.try_send((data.to_vec(), src));
		}
	}
}

#[cfg(test)]
mod tests {
	use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
	use std::time::Duration;

	use str0m::ice::{StunMessage, TransId};

	use super::*;

	/// SHA1-HMAC for a STUN MESSAGE-INTEGRITY, matching what str0m signs with.
	fn sha1_hmac(key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
		use rind::hmac;

		let key = hmac::Key::new(&hmac::legacy::HMAC_SHA1, key);
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
	async fn routes_per_peer_until_unregistered() {
		let mux = Mux::bind("127.0.0.1:0".parse().unwrap(), &[]).await.unwrap();
		let (a, mut inbound_a, registration_a) = mux.register();
		let (b, mut inbound_b, _registration_b) = mux.register();
		let unknown = IceCreds::new();
		let (request_a, request_b, request_unknown) =
			(binding_request(&a), binding_request(&b), binding_request(&unknown));
		fn parse(request: &[u8]) -> Datagram<'_> {
			let datagram = Datagram::parse(request);
			assert!(matches!(datagram, Datagram::Request(_)), "not a binding request");
			datagram
		}
		let address_a = "127.0.0.1:10001".parse().unwrap();
		let address_b = "127.0.0.1:10002".parse().unwrap();
		let mut registry = mux.registry.lock().unwrap();
		assert!(registry.route(address_a, &parse(&request_unknown))[0].is_none());
		registry.route(address_a, &parse(&request_a))[0]
			.clone()
			.unwrap()
			.sender
			.try_send((vec![1], address_a))
			.unwrap();
		// A cached address routes without a ufrag.
		registry.route(address_a, &Datagram::Other)[0]
			.clone()
			.unwrap()
			.sender
			.try_send((vec![2], address_a))
			.unwrap();
		registry.route(address_b, &parse(&request_b))[0]
			.clone()
			.unwrap()
			.sender
			.try_send((vec![3], address_b))
			.unwrap();
		drop(registry);
		assert_eq!(inbound_a.packets.try_recv().unwrap().0, [1]);
		assert_eq!(inbound_a.packets.try_recv().unwrap().0, [2]);
		assert_eq!(inbound_b.packets.try_recv().unwrap().0, [3]);

		// Unregister even while an inbox is still held: cached addresses cannot
		// keep routing traffic after the peer ends.
		drop(registration_a);
		let mut registry = mux.registry.lock().unwrap();
		assert!(registry.route(address_a, &Datagram::Other)[0].is_none());
		assert!(registry.route(address_a, &parse(&request_a))[0].is_none());
	}

	/// A signed binding request for an address another session holds only claims it: the
	/// claimant gets the answers to its checks, and takes the address once its ICE confirms the
	/// path, or the holder ends. A forged request claims nothing.
	#[tokio::test]
	async fn a_claimed_address_moves_once_confirmed() {
		let mux = Mux::bind("127.0.0.1:0".parse().unwrap(), &[]).await.unwrap();
		let port = mux.local_addr().port();
		let (old, mut old_inbound, _old_registration) = mux.register();
		let (new, mut new_inbound, _new_registration) = mux.register();
		let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
		let send = async |data: &[u8]| peer.send_to(data, (Ipv4Addr::LOCALHOST, port)).await.unwrap();
		let recv = async |inbound: &mut Inbound| {
			kio::clock::timeout(Duration::from_secs(5), inbound.packets.recv())
				.await
				.expect("the demux routes the datagram")
				.expect("session inbox stayed open")
				.0
		};
		const DTLS: &[u8] = &[22, 254, 253];
		const ANSWER: &[u8] = &[1, 1, 0, 0];

		let request = binding_request(&old);
		send(&request).await;
		assert_eq!(recv(&mut old_inbound).await, request);

		// Forged: it reaches the session it names, which drops it, and claims nothing.
		let forged = binding_request(&IceCreds {
			ufrag: new.ufrag.clone(),
			pass: old.pass.clone(),
		});
		send(&forged).await;
		assert_eq!(recv(&mut new_inbound).await, forged);
		send(ANSWER).await;
		assert_eq!(recv(&mut old_inbound).await, ANSWER);

		// Signed: a claim, so the new session gets the answers to its checks, while the old one
		// keeps the address.
		let request = binding_request(&new);
		send(&request).await;
		assert_eq!(recv(&mut new_inbound).await, request);
		send(ANSWER).await;
		assert_eq!(recv(&mut old_inbound).await, ANSWER);
		assert_eq!(recv(&mut new_inbound).await, ANSWER);
		send(DTLS).await;
		assert_eq!(recv(&mut old_inbound).await, DTLS, "a claim moved the address");

		// Confirmed: the address moves.
		let peer_addr = peer.local_addr().unwrap();
		new_inbound.route.as_ref().unwrap().confirm(peer_addr);
		send(DTLS).await;
		assert_eq!(recv(&mut new_inbound).await, DTLS);
		assert!(
			old_inbound.packets.try_recv().is_err(),
			"the old session kept the address"
		);
	}

	/// A session that answers its own check from a victim's address, while the check went to
	/// the attacker's real one, connects but never takes the victim's address. The same answer
	/// from the address the check went to, as a peer re-offering from it sends, takes it.
	#[tokio::test]
	async fn only_an_answer_from_where_the_check_went_moves_an_address() {
		assert!(!take_over(true).await, "a spoofed answer moved the victim's address");
		assert!(take_over(false).await, "a genuine re-offer stayed with the old session");
	}

	/// Connect a str0m offerer to a new session on a mux where another session holds an
	/// address, and report whether the new session took it. The offerer's answers to the
	/// session's checks wait until it nominates, so the session's ICE connects on one of
	/// them. With `spoof`, the offerer lives elsewhere and sends those answers, and one
	/// signed request, from the held address; without, it lives at the held address.
	async fn take_over(spoof: bool) -> bool {
		use crate::ice::{Agent, Socket};
		use kio::clock::Instant;
		use str0m::net::{Protocol, Receive};
		use str0m::{Candidate, Event, Input, Output};

		let mux = Mux::bind("127.0.0.1:0".parse().unwrap(), &[]).await.unwrap();
		let local = mux.local_addr();
		let real = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
		let real_addr = real.local_addr().unwrap();
		// Kept open so checks sent to the victim land somewhere.
		let victim_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
		let held = if spoof {
			victim_socket.local_addr().unwrap()
		} else {
			real_addr
		};

		let (victim, _victim_inbound, _victim_registration) = mux.register();
		let deliver = |data: &[u8], src: SocketAddr| {
			let targets = mux.registry.lock().unwrap().route(src, &Datagram::parse(data));
			for entry in targets.into_iter().flatten() {
				let _ = entry.sender.try_send((data.to_vec(), src));
			}
		};
		deliver(&binding_request(&victim), held);

		let now = Instant::now().into_std();
		let (creds, inbound, _registration) = mux.register();
		let ufrag = creds.ufrag.clone();
		let mut answerer = crate::session::rtc_config()
			.unwrap()
			.set_local_ice_credentials(creds)
			.build(now);
		answerer.add_local_candidate(Candidate::host(local, "udp").unwrap());
		let mut offerer = crate::session::rtc_config().unwrap().build(now);
		offerer.add_local_candidate(Candidate::host(real_addr, "udp").unwrap());
		let mut api = offerer.sdp_api();
		api.add_media(
			str0m::media::MediaKind::Audio,
			str0m::media::Direction::RecvOnly,
			None,
			None,
			None,
		);
		let (offer, pending) = api.apply().unwrap();
		let answer = answerer.sdp_api().accept_offer(offer).unwrap();
		offerer.sdp_api().accept_answer(pending, answer).unwrap();
		let mut agent = Agent::new(answerer, Socket::new(mux.socket(), vec![local], inbound).unwrap()).unwrap();

		let mut claimed = !spoof;
		let mut nominated = false;
		let mut answers: Vec<Vec<u8>> = Vec::new();
		let mut buf = vec![0u8; 2048];
		let connect = async {
			loop {
				let timeout = loop {
					match offerer.poll_output().unwrap() {
						Output::Timeout(timeout) => break timeout,
						Output::Event(_) => {}
						Output::Transmit(transmit) => {
							let data = transmit.contents.to_vec();
							let Ok(message) = StunMessage::parse(&data) else {
								continue;
							};
							if message.is_binding_request() {
								if !claimed {
									deliver(&data, held);
									claimed = true;
								}
								nominated |= message.use_candidate();
								deliver(&data, real_addr);
								if nominated {
									for answer in answers.drain(..) {
										deliver(&answer, held);
									}
								}
							} else if nominated {
								deliver(&data, held);
							} else {
								answers.push(data);
							}
						}
					}
				};
				tokio::select! {
					received = real.recv_from(&mut buf) => {
						let (len, src) = received.unwrap();
						let now = Instant::now().into_std();
						if let Ok(receive) = Receive::new(Protocol::Udp, src, real_addr, &buf[..len]) {
							offerer.handle_input(Input::Receive(now, receive)).unwrap();
						}
					}
					_ = kio::clock::sleep_until(Instant::from_std(timeout)) => {
						offerer.handle_input(Input::Timeout(Instant::now().into_std())).unwrap();
					}
					event = agent.next() => {
						if let Event::IceConnectionStateChange(state) = event.unwrap()
							&& state.is_connected()
						{
							return;
						}
					}
				}
			}
		};
		kio::clock::timeout(Duration::from_secs(10), connect)
			.await
			.expect("the session's ICE connects");
		assert!(nominated, "ICE connected before the offerer nominated");

		let registry = mux.registry.lock().unwrap();
		registry.by_addr.get(&held).is_some_and(|entry| entry.ufrag == ufrag)
	}

	/// A claimant takes the address once its holder ends, confirmed or not.
	#[tokio::test]
	async fn a_claim_outlives_its_holder() {
		let mux = Mux::bind("127.0.0.1:0".parse().unwrap(), &[]).await.unwrap();
		let (old, old_inbound, old_registration) = mux.register();
		let (new, mut new_inbound, _new_registration) = mux.register();
		let (request_old, request_new) = (binding_request(&old), binding_request(&new));
		let address = "127.0.0.1:10001".parse().unwrap();
		let mut registry = mux.registry.lock().unwrap();
		registry.route(address, &Datagram::parse(&request_old));
		registry.route(address, &Datagram::parse(&request_new));
		drop(registry);
		drop((old_inbound, old_registration));

		let mut registry = mux.registry.lock().unwrap();
		let [holder, claimant] = registry.route(address, &Datagram::Other);
		assert!(claimant.is_none());
		holder.unwrap().sender.try_send((vec![1], address)).unwrap();
		drop(registry);
		assert_eq!(new_inbound.packets.try_recv().unwrap().0, [1]);
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
		let port = mux.local_addr().port();
		let (creds, mut rx, registration) = mux.register();

		// Dial the mux over IPv4, which a dual-stack socket accepts as v4-mapped.
		let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
		let peer_addr = peer.local_addr().unwrap();
		let request = binding_request(&creds);
		peer.send_to(&request, (Ipv4Addr::LOCALHOST, port))
			.await
			.expect("IPv4 peer must reach a dual-stack mux");

		let (_data, src) = kio::clock::timeout(Duration::from_secs(5), rx.packets.recv())
			.await
			.expect("the demux must route the IPv4 peer's binding request")
			.expect("session inbox stayed open");

		assert_eq!(src, peer_addr, "an IPv4 peer's source must arrive unmapped");
		assert!(src.is_ipv4());
		drop(registration);
	}
}
