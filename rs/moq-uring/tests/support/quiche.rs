//! Raw-quiche echo plumbing shared by the integration test and the ablation
//! benchmark (`#[path]`-included by both, so each sees the subset it calls).
//!
//! quiche is sans-IO, which is the point: these helpers wire its packet-in /
//! packet-out / timeout surface straight to a [`moq_uring::udp::Socket`] and a
//! [`moq_net::runtime::Deadline`], with no other runtime underneath.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context as _;
use moq_net::runtime::Deadline;
use moq_uring::{Handle, udp};

/// The QUIC UDP payload size both sides use: every full datagram in a GSO
/// train, and the GRO coalesce stride.
pub const SEGMENT: usize = 1350;
/// One GSO train is at most 64 segments; stay a hair under the kernel cap.
const TRAIN_SEGMENTS: usize = 63;

/// A self-signed localhost certificate on disk, for quiche's file-based API.
pub struct Certs {
	pub dir: tempfile::TempDir,
	pub cert: std::path::PathBuf,
	pub key: std::path::PathBuf,
}

pub fn certs() -> anyhow::Result<Certs> {
	let signed = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
	let dir = tempfile::tempdir()?;
	let cert = dir.path().join("cert.pem");
	let key = dir.path().join("key.pem");
	std::fs::write(&cert, signed.cert.pem())?;
	std::fs::write(&key, signed.signing_key.serialize_pem())?;
	Ok(Certs { dir, cert, key })
}

fn config(certs: Option<&Certs>) -> anyhow::Result<quiche::Config> {
	let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;
	if let Some(certs) = certs {
		config.load_cert_chain_from_pem_file(certs.cert.to_str().context("cert path")?)?;
		config.load_priv_key_from_pem_file(certs.key.to_str().context("key path")?)?;
	} else {
		// The client trusts our self-signed server blindly; this is loopback.
		config.verify_peer(false);
	}
	config.set_application_protos(&[b"moq-uring-echo"])?;
	// Generous windows so the echo never stalls on flow control.
	config.set_initial_max_data(64 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_local(32 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_remote(32 * 1024 * 1024);
	config.set_initial_max_streams_bidi(16);
	config.set_max_recv_udp_payload_size(SEGMENT);
	config.set_max_send_udp_payload_size(SEGMENT);
	config.set_max_idle_timeout(10_000);
	Ok(config)
}

fn connection_id() -> quiche::ConnectionId<'static> {
	let scid: [u8; quiche::MAX_CONN_ID_LEN] = rand::random();
	quiche::ConnectionId::from_vec(scid.to_vec())
}

/// One QUIC endpoint: a worker socket plus a quiche connection and its timer.
pub struct Peer {
	pub sock: udp::Socket,
	pub conn: quiche::Connection,
	pub deadline: Deadline<Handle>,
	pub local: SocketAddr,
}

impl Peer {
	/// A client half-connected to `server`, first flight not yet sent.
	pub fn connect(handle: &Handle, sock: udp::Socket, server: SocketAddr) -> anyhow::Result<Self> {
		let local = sock.local_addr()?;
		let mut config = config(None)?;
		let conn = quiche::connect(Some("localhost"), &connection_id(), local, server, &mut config)?;
		Ok(Self {
			sock,
			conn,
			deadline: Deadline::new(handle),
			local,
		})
	}

	/// A server that will accept the first connection arriving on `sock`.
	pub fn accept(handle: &Handle, sock: udp::Socket, certs: &Certs, from: SocketAddr) -> anyhow::Result<Self> {
		let local = sock.local_addr()?;
		let mut config = config(Some(certs))?;
		let conn = quiche::accept(&connection_id(), None, local, from, &mut config)?;
		Ok(Self {
			sock,
			conn,
			deadline: Deadline::new(handle),
			local,
		})
	}

	/// Drain quiche's egress into GSO trains until it reports `Done`.
	pub async fn flush(&mut self) -> anyhow::Result<()> {
		loop {
			let mut tx = self.sock.acquire().await?;
			let mut filled = 0;
			let mut dest = None;
			// Pack uniform SEGMENT-sized packets back to back; a short packet
			// ends the train (it must ride last in a GSO send).
			while filled + SEGMENT <= tx.len() && filled / SEGMENT < TRAIN_SEGMENTS {
				match self.conn.send(&mut tx[filled..filled + SEGMENT]) {
					Ok((n, info)) => {
						dest = Some(info.to);
						filled += n;
						if n < SEGMENT {
							break;
						}
					}
					Err(quiche::Error::Done) => break,
					Err(err) => return Err(err.into()),
				}
			}
			let Some(to) = dest else {
				return Ok(());
			};
			tx.send(filled, to, SEGMENT)?;
		}
	}

	/// Feed one received packet's datagrams into quiche.
	pub fn ingest(&mut self, packet: &mut udp::Packet) -> anyhow::Result<()> {
		let from = packet.from();
		let to = self.local;
		for segment in packet.segments() {
			match self.conn.recv(segment, quiche::RecvInfo { from, to }) {
				Ok(_) => {}
				// A malformed/undecryptable datagram is UDP noise, not fatal.
				Err(err) => tracing::debug!(?err, "quiche dropped a datagram"),
			}
		}
		Ok(())
	}

	/// Wait for ingress or the connection's timeout, and process it. Returns
	/// once quiche state may have advanced.
	pub async fn step(&mut self) -> anyhow::Result<()> {
		self.deadline.set(self.conn.timeout_instant());
		enum Event {
			Packet(udp::Packet),
			Timeout,
		}
		let event = kio::wait(|waiter| {
			if let Poll::Ready(result) = self.sock.poll_recv(waiter) {
				return Poll::Ready(result.map(Event::Packet));
			}
			if self.deadline.poll(waiter).is_ready() {
				return Poll::Ready(Ok(Event::Timeout));
			}
			Poll::Pending
		})
		.await?;
		match event {
			Event::Packet(mut packet) => {
				self.ingest(&mut packet)?;
				// Drain whatever else already queued without parking again.
				while let Poll::Ready(result) = self.sock.poll_recv(&kio::Waiter::noop()) {
					self.ingest(&mut result?)?;
				}
			}
			Event::Timeout => self.conn.on_timeout(),
		}
		Ok(())
	}
}

/// Echo every readable byte back on its stream, mirroring fins. Reads are
/// bounded by the stream's send capacity so the echo write can never hit
/// `Done`; a capacity-blocked stream stays readable and is retried after the
/// next flush/receive round frees the congestion window.
pub fn echo_streams(conn: &mut quiche::Connection, scratch: &mut [u8]) -> anyhow::Result<()> {
	let readable: Vec<u64> = conn.readable().collect();
	for stream in readable {
		loop {
			let cap = match conn.stream_capacity(stream) {
				Ok(cap) => cap,
				Err(quiche::Error::InvalidStreamState(_)) => break,
				Err(err) => return Err(err.into()),
			};
			if cap == 0 {
				break;
			}
			let take = cap.min(scratch.len());
			match conn.stream_recv(stream, &mut scratch[..take]) {
				Ok((n, fin)) => {
					let mut offset = 0;
					while offset < n {
						offset += conn.stream_send(stream, &scratch[offset..n], false)?;
					}
					if fin {
						// An empty fin write succeeds even at zero capacity.
						conn.stream_send(stream, &[], true)?;
						break;
					}
				}
				Err(quiche::Error::Done) => break,
				Err(err) => return Err(err.into()),
			}
		}
	}
	Ok(())
}

/// Run a whole client session against an already-listening echo server task:
/// handshake, send `payload` with fin, collect the echo, close. Returns the
/// echoed bytes.
pub async fn echo_client(
	handle: &Handle,
	sock: udp::Socket,
	server: SocketAddr,
	payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
	let mut peer = Peer::connect(handle, sock, server)?;
	let mut echoed = Vec::with_capacity(payload.len());
	let mut sent = 0;
	let mut scratch = vec![0u8; 64 * 1024];
	let mut watchdog = Deadline::after(handle, Duration::from_secs(30));

	loop {
		if peer.conn.is_established() && sent < payload.len() {
			match peer.conn.stream_send(0, &payload[sent..], false) {
				Ok(n) => {
					sent += n;
					if sent == payload.len() {
						peer.conn.stream_send(0, &[], true)?;
					}
				}
				Err(quiche::Error::Done) => {}
				Err(err) => return Err(err.into()),
			}
		}
		loop {
			match peer.conn.stream_recv(0, &mut scratch) {
				Ok((n, fin)) => {
					echoed.extend_from_slice(&scratch[..n]);
					if fin {
						peer.conn.close(true, 0, b"done").ok();
					}
				}
				Err(quiche::Error::Done) => break,
				Err(quiche::Error::InvalidStreamState(_)) => break,
				Err(err) => return Err(err.into()),
			}
		}
		peer.flush().await?;
		if peer.conn.is_closed() {
			return Ok(echoed);
		}

		enum Event {
			Stepped,
			Watchdog,
		}
		let event = {
			let mut step = std::pin::pin!(peer.step());
			kio::wait(|waiter| {
				if watchdog.poll(waiter).is_ready() {
					return Poll::Ready(Ok(Event::Watchdog));
				}
				match waiter.poll_future(step.as_mut()) {
					Poll::Ready(result) => Poll::Ready(result.map(|()| Event::Stepped)),
					Poll::Pending => Poll::Pending,
				}
			})
			.await?
		};
		if matches!(event, Event::Watchdog) {
			anyhow::bail!("echo timed out after 30s (sent {sent}, echoed {})", echoed.len());
		}
	}
}

/// Echo `payload` over one fresh stream of an established connection and wait
/// for it all to come back. Returns the echoed byte count.
pub async fn stream_echo(peer: &mut Peer, stream: u64, payload: &[u8]) -> anyhow::Result<usize> {
	let mut scratch = vec![0u8; 64 * 1024];
	let mut sent = 0;
	let mut fin_sent = false;
	let mut received = 0;

	loop {
		while sent < payload.len() {
			match peer.conn.stream_send(stream, &payload[sent..], false) {
				Ok(0) => break,
				Ok(n) => sent += n,
				Err(quiche::Error::Done) => break,
				Err(err) => return Err(err.into()),
			}
		}
		if sent == payload.len() && !fin_sent {
			peer.conn.stream_send(stream, &[], true)?;
			fin_sent = true;
		}
		loop {
			match peer.conn.stream_recv(stream, &mut scratch) {
				Ok((n, fin)) => {
					received += n;
					if fin {
						return Ok(received);
					}
				}
				Err(quiche::Error::Done) | Err(quiche::Error::InvalidStreamState(_)) => break,
				Err(err) => return Err(err.into()),
			}
		}
		peer.flush().await?;
		peer.step().await?;
	}
}

/// The echo server body: accept whatever arrives on `sock` first, then echo
/// until the connection closes.
pub async fn echo_server(handle: Handle, sock: udp::Socket, certs: Certs) -> anyhow::Result<()> {
	// The first packet tells us who is connecting.
	let mut first = sock.recv().await?;
	let mut peer = Peer::accept(&handle, sock, &certs, first.from())?;
	peer.ingest(&mut first)?;
	drop(first);

	let mut scratch = vec![0u8; 64 * 1024];
	loop {
		echo_streams(&mut peer.conn, &mut scratch)?;
		peer.flush().await?;
		if peer.conn.is_closed() {
			return Ok(());
		}
		peer.step().await?;
	}
}
