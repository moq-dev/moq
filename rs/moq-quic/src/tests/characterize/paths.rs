//! Packet-level defenses and path changes

use std::any::Any;

use super::*;
use crate::crypto::{self, KeyPair, Keys};

const MSG: &[u8] = b"hello";

/// Failed decryptions a [`Limited`] connection tolerates
const INTEGRITY_LIMIT: u64 = 3;

/// Client crypto whose packet keys tolerate only [`INTEGRITY_LIMIT`] failed decryptions
struct Limited(Arc<dyn crypto::ClientConfig>);

impl crypto::ClientConfig for Limited {
	fn start_session(
		self: Arc<Self>,
		version: Version,
		server_name: &str,
		params: &TransportParameters,
	) -> Result<Box<dyn crypto::Session>, crate::endpoint::ConnectError> {
		let inner = self.0.clone().start_session(version, server_name, params)?;
		Ok(Box::new(LimitedSession(inner)))
	}
}

struct LimitedSession(Box<dyn crypto::Session>);

struct LimitedKey(Box<dyn crypto::PacketKey>);

impl crypto::PacketKey for LimitedKey {
	fn encrypt(&self, packet: u64, buf: &mut [u8], header_len: usize) {
		self.0.encrypt(packet, buf, header_len)
	}
	fn decrypt(&self, packet: u64, header: &[u8], payload: &mut BytesMut) -> Result<(), crypto::CryptoError> {
		self.0.decrypt(packet, header, payload)
	}
	fn tag_len(&self) -> usize {
		self.0.tag_len()
	}
	fn confidentiality_limit(&self) -> u64 {
		self.0.confidentiality_limit()
	}
	fn integrity_limit(&self) -> u64 {
		INTEGRITY_LIMIT
	}
}

fn limit_pair(keys: KeyPair<Box<dyn crypto::PacketKey>>) -> KeyPair<Box<dyn crypto::PacketKey>> {
	KeyPair {
		local: Box::new(LimitedKey(keys.local)),
		remote: Box::new(LimitedKey(keys.remote)),
	}
}

fn limit(keys: Keys) -> Keys {
	Keys {
		header: keys.header,
		packet: limit_pair(keys.packet),
	}
}

impl crypto::Session for LimitedSession {
	fn initial_keys(&self, dst_cid: ConnectionId, side: Side) -> Keys {
		limit(self.0.initial_keys(dst_cid, side))
	}
	fn handshake_data(&self) -> Option<Box<dyn Any>> {
		self.0.handshake_data()
	}
	fn peer_identity(&self) -> Option<Box<dyn Any>> {
		self.0.peer_identity()
	}
	fn is_handshaking(&self) -> bool {
		self.0.is_handshaking()
	}
	fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
		self.0.read_handshake(buf)
	}
	fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
		self.0.transport_parameters()
	}
	fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
		self.0.write_handshake(buf).map(limit)
	}
	fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn crypto::PacketKey>>> {
		self.0.next_1rtt_keys().map(limit_pair)
	}
	fn is_valid_retry(&self, orig_dst_cid: ConnectionId, header: &[u8], payload: &[u8]) -> bool {
		self.0.is_valid_retry(orig_dst_cid, header, payload)
	}
	fn export_keying_material(
		&self,
		output: &mut [u8],
		label: &[u8],
		context: &[u8],
	) -> Result<(), crypto::ExportKeyingMaterialError> {
		self.0.export_keying_material(output, label, context)
	}
}

/// Packets that fail authentication past the integrity limit kill the connection silently
#[test]
fn aead_integrity_limit() {
	let _guard = subscribe();
	let mut pair = pair_with(|_| {});
	let mut config = client::Config::new(Arc::new(Limited(Arc::new(client_crypto()))));
	config.transport = transport();
	let (client_ch, server_ch) = pair.connect_with(config);

	for sent in 1..=INTEGRITY_LIMIT + 1 {
		let now = pair.time;
		let mut packet = pair
			.server_conn_mut(server_ch)
			.forge_packet(now, SpaceId::Data, &[0; 32]);
		// Flip a byte of the AEAD tag
		*packet.last_mut().unwrap() ^= 0xff;
		pair.client.inbound.push_back((now, None, packet.as_slice().into()));
		pair.drive_client();
		let lost = lost(pair.client_conn_mut(client_ch));
		if sent <= INTEGRITY_LIMIT {
			assert_eq!(lost, None);
		} else {
			assert_matches!(
				lost,
				Some(ConnectionError::TransportError(TransportError {
					code: TransportErrorCode::AEAD_LIMIT_REACHED,
					..
				}))
			);
		}
	}

	// The client sends no CONNECTION_CLOSE
	assert!(pair.client_conn_mut(client_ch).is_drained());
	assert!(pair.server.inbound.is_empty());
}

/// A replayed packet is discarded before its frames are processed
#[test]
fn duplicate_packet_is_discarded() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();
	pair.drive();
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(MSG).unwrap();
	pair.drive_client();
	assert_eq!(pair.server.inbound.len(), 1);
	let duplicate = pair.server.inbound[0].clone();
	pair.server.inbound.push_back(duplicate);
	let before = pair.server_conn_mut(server_ch).stats().frame_rx.stream;
	pair.drive();

	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.stream, before + 1);
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_eq!(chunks.next(usize::MAX).unwrap().unwrap().bytes, MSG);
	assert_eq!(chunks.next(usize::MAX), Err(ReadError::Blocked));
	let _ = chunks.finalize();
}

/// Rebuilds a Retry datagram with `src_cid`, under a valid integrity tag
fn forge_retry(retry: &[u8], src_cid: &[u8], orig_dst_cid: ConnectionId) -> Vec<u8> {
	let version = Version::from_wire(u32::from_be_bytes(retry[1..5].try_into().unwrap())).unwrap();
	let dst_cid_end = 6 + retry[5] as usize;
	let src_cid_end = dst_cid_end + 1 + retry[dst_cid_end] as usize;
	let mut packet = retry[..dst_cid_end].to_vec();
	packet.push(src_cid.len() as u8);
	packet.extend_from_slice(src_cid);
	packet.extend_from_slice(&retry[src_cid_end..retry.len() - 16]);
	let tag = server_config().crypto.retry_tag(version, orig_dst_cid, &packet);
	packet.extend_from_slice(&tag);
	packet
}

/// Rewrites the server's Retry datagram, given the client's original DCID, into the datagrams
/// the client receives instead
type Mangle = fn(&[u8], ConnectionId) -> Vec<Vec<u8>>;

/// A Retry whose integrity tag fails
fn bad_tag(retry: &[u8], _: ConnectionId) -> Vec<Vec<u8>> {
	let mut retry = retry.to_vec();
	*retry.last_mut().unwrap() ^= 0xff;
	vec![retry]
}

/// A Retry naming the client's original DCID as the server's new CID
fn same_cid(retry: &[u8], orig_dst_cid: ConnectionId) -> Vec<Vec<u8>> {
	vec![forge_retry(retry, &orig_dst_cid, orig_dst_cid)]
}

/// The server's Retry, then a second, different one
fn second(retry: &[u8], orig_dst_cid: ConnectionId) -> Vec<Vec<u8>> {
	vec![retry.to_vec(), forge_retry(retry, &[0x22; 8], orig_dst_cid)]
}

/// Starts a connection the server Retries, queueing for the client what `mangle` makes of the
/// Retry, and returning the client's original DCID
fn retried(mangle: Mangle) -> (Pair, ConnectionHandle, ConnectionId) {
	let mut pair = pair_with(|_| {});
	pair.server.handle_incoming = Box::new(validate_incoming);
	let client_ch = pair.begin_connect(client_with(|_| {}));
	pair.drive_client();
	let initial = &pair.server.inbound[0].2;
	let orig_dst_cid = ConnectionId::new(&initial[6..6 + initial[5] as usize]);
	pair.drive_server();
	let (time, ecn, retry) = pair.client.inbound.pop_back().unwrap();
	assert!(pair.client.inbound.is_empty());
	assert_eq!(retry[0] & 0xf0, 0xf0, "a Retry");
	for datagram in mangle(&retry, orig_dst_cid) {
		pair.client.inbound.push_back((time, ecn, datagram.as_slice().into()));
	}
	(pair, client_ch, orig_dst_cid)
}

/// The client's next Initial keeps its original DCID unless it accepted a Retry
#[test]
fn invalid_retry_is_discarded() {
	let _guard = subscribe();
	for (mangle, accepted) in [(bad_tag as Mangle, false), (same_cid, false), (second, true)] {
		let (mut pair, _, orig_dst_cid) = retried(mangle);
		pair.drive_client();
		while pair.server.inbound.is_empty() {
			pair.time = pair.client.next_wakeup().unwrap();
			pair.drive_client();
		}
		let initial = &pair.server.inbound[0].2;
		let dst_cid = ConnectionId::new(&initial[6..6 + initial[5] as usize]);
		assert_eq!(dst_cid != orig_dst_cid, accepted);
	}
}

/// The client follows the first Retry and ignores the second
#[test]
fn second_retry_is_discarded() {
	let _guard = subscribe();
	let (mut pair, client_ch, _) = retried(second);
	pair.drive();
	assert_connected(pair.client_conn_mut(client_ch));
}

/// A discarded Retry does not stop the client from following the server's genuine Retry of its
/// retransmitted Initial
#[test]
fn retry_after_invalid_retry() {
	let _guard = subscribe();
	for mangle in [bad_tag as Mangle, same_cid] {
		let (mut pair, client_ch, _) = retried(mangle);
		pair.drive();
		assert_connected(pair.client_conn_mut(client_ch));
	}
}

/// A discarded Retry does not stop the client from processing a Version Negotiation packet
#[test]
fn version_negotiation_after_invalid_retry() {
	let _guard = subscribe();
	for mangle in [bad_tag as Mangle, same_cid] {
		let (mut pair, client_ch, orig_dst_cid) = retried(mangle);
		let (time, _, retry) = pair.client.inbound.back().unwrap();
		let time = *time;
		// Echo the client's CIDs, offering only a reserved version
		let dst_cid_end = 6 + retry[5] as usize;
		let mut vn = vec![0x80, 0, 0, 0, 0];
		vn.extend_from_slice(&retry[5..dst_cid_end]);
		vn.push(orig_dst_cid.len() as u8);
		vn.extend_from_slice(&orig_dst_cid);
		vn.extend_from_slice(&[0x0a, 0x1a, 0x2a, 0x3a]);
		pair.client.inbound.push_back((time, None, vn.as_slice().into()));
		pair.drive_client();
		assert_matches!(
			lost(pair.client_conn_mut(client_ch)),
			Some(ConnectionError::VersionMismatch)
		);
	}
}

/// A packet that arrives from a new address moves the server to it, and when the new address
/// never answers the challenge, the server falls back to the address it validated before
#[test]
fn path_validation_failure_reverts() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();
	pair.drive();
	let old = pair.client.addr;
	let new = SocketAddr::new(old.ip(), old.port() + 1);

	// One client packet arrives from the new address, as if forwarded by an attacker
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	let now = pair.time;
	pair.server.drive(now, new);
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), new);

	// Nothing the server sends arrives, and the client stays silent
	let mut updates = Vec::new();
	while pair.server_conn_mut(server_ch).remote_address() == new {
		pair.server.outbound.clear();
		pair.time = pair.server.next_wakeup().expect("validation times out");
		let now = pair.time;
		pair.server.drive(now, old);
		while let Some(event) = pair.server_conn_mut(server_ch).poll() {
			updates.push(event);
		}
	}
	pair.server.outbound.clear();
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), old);
	assert_matches!(updates.as_slice(), [.., Event::PathUpdated]);

	// The connection still works on the old path
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(MSG).unwrap();
	pair.drive();
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
}

/// The RTT the server measures after the client's port changes
fn rtt_after_rebinding(ip: IpAddr) -> (Duration, Duration) {
	let _guard = subscribe();
	let mut pair = pair_with(|_| {});
	pair.client.addr = SocketAddr::new(ip, pair.client.addr.port());
	pair.server.addr = SocketAddr::new(ip, pair.server.addr.port());
	let (client_ch, server_ch) = pair.connect_with(client_with(|_| {}));
	pair.drive();
	let before = pair.server_conn_mut(server_ch).rtt();

	let new = SocketAddr::new(ip, pair.client.addr.port() + 1);
	pair.client.addr = new;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	let now = pair.time;
	pair.server.drive(now, new);
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), new);
	(before, pair.server_conn_mut(server_ch).rtt())
}

/// A port change on the same IPv4 address looks like NAT rebinding and keeps the path's RTT and
/// congestion state
#[test]
fn ipv4_rebinding_keeps_path_state() {
	let (before, after) = rtt_after_rebinding(Ipv4Addr::LOCALHOST.into());
	assert_eq!(before, after);
}

/// The same port change over IPv6 starts the path over from the initial RTT
#[test]
fn ipv6_rebinding_resets_path_state() {
	let (before, after) = rtt_after_rebinding(Ipv6Addr::LOCALHOST.into());
	assert_ne!(before, after);
	assert_eq!(after, transport::Config::default().initial_rtt);
}
