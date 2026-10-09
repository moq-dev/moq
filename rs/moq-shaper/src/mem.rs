//! An in-memory stand-in for [`tokio::net::UdpSocket`], so the unit tests run
//! on a paused clock. A paused clock jumps to the next timer whenever the
//! runtime idles, which would fire timeouts and steps while a real datagram
//! sat in the kernel; a datagram here is delivered inside the runtime, so the
//! clock only moves once nothing is in flight. `tests/cli.rs` covers real sockets.

use std::{
	collections::HashMap,
	io,
	net::SocketAddr,
	sync::{
		LazyLock, Mutex, OnceLock,
		atomic::{AtomicU16, Ordering},
	},
};

use tokio::sync::mpsc;

type Datagram = (SocketAddr, Vec<u8>);

/// Every bound socket by port, which is the whole address on this network.
static PORTS: LazyLock<Mutex<HashMap<u16, mpsc::UnboundedSender<Datagram>>>> = LazyLock::new(Default::default);
static NEXT_PORT: AtomicU16 = AtomicU16::new(1);

pub struct UdpSocket {
	addr: SocketAddr,
	peer: OnceLock<SocketAddr>,
	queue: tokio::sync::Mutex<mpsc::UnboundedReceiver<Datagram>>,
}

impl UdpSocket {
	pub async fn bind(mut addr: SocketAddr) -> io::Result<Self> {
		if addr.port() == 0 {
			addr.set_port(NEXT_PORT.fetch_add(1, Ordering::Relaxed));
		}
		let (send, queue) = mpsc::unbounded_channel();
		let mut ports = PORTS.lock().unwrap();
		if ports.contains_key(&addr.port()) {
			return Err(io::ErrorKind::AddrInUse.into());
		}
		ports.insert(addr.port(), send);
		Ok(Self {
			addr,
			peer: OnceLock::new(),
			queue: tokio::sync::Mutex::new(queue),
		})
	}

	pub fn local_addr(&self) -> io::Result<SocketAddr> {
		Ok(self.addr)
	}

	pub async fn connect(&self, peer: SocketAddr) -> io::Result<()> {
		self.peer.set(peer).map_err(|_| io::ErrorKind::AlreadyExists.into())
	}

	/// Like UDP, a datagram to a port nobody bound is lost without an error.
	pub async fn send_to(&self, buf: &[u8], dest: SocketAddr) -> io::Result<usize> {
		if let Some(port) = PORTS.lock().unwrap().get(&dest.port()) {
			let _ = port.send((self.addr, buf.to_vec()));
		}
		Ok(buf.len())
	}

	pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
		// The port's sender lives until this socket drops, so the queue never ends.
		let (from, datagram) = self.queue.lock().await.recv().await.expect("bound port");
		let size = datagram.len().min(buf.len());
		buf[..size].copy_from_slice(&datagram[..size]);
		Ok((size, from))
	}

	pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
		self.send_to(buf, *self.peer.get().expect("not connected")).await
	}

	/// Only what the connected peer sends, as a connected UDP socket filters.
	pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
		let peer = *self.peer.get().expect("not connected");
		loop {
			let (size, from) = self.recv_from(buf).await?;
			if from == peer {
				return Ok(size);
			}
		}
	}
}

impl Drop for UdpSocket {
	fn drop(&mut self) {
		PORTS.lock().unwrap().remove(&self.addr.port());
	}
}
