//! One UDP port shared by QUIC and the WebRTC media path.
//!
//! A QUIC listener's socket classifies every datagram it reads with
//! [`moq_sock::demux`]: QUIC stays on the endpoint, and ICE, DTLS, and RTP
//! are diverted to a [`WebRtc`] feed that an embedder hands to its WebRTC stack.
//! Anything else is dropped and counted. The split runs inline in the endpoint's
//! receive, so a QUIC datagram is never copied or handed across tasks.

use std::{
	io::{self, IoSliceMut},
	net::{IpAddr, SocketAddr},
	sync::{Arc, Mutex},
	task::{Context, Poll},
};

use bytes::Bytes;
use moq_sock::{
	demux::{Flows, Queue, Route},
	udp::RecvMeta,
};

pub use moq_sock::{
	demux::{Counters, Discard, Pin},
	udp::Transmit,
};

use super::driver::{
	Slot,
	endpoint::{AsyncUdpSocket, Endpoint},
};

/// The bytes a [`WebRtc`] feed may hold before it drops, shared by every source.
const CAPACITY: usize = 4 << 20;

/// A QUIC listener's demux: what its socket diverts, pins, and drops.
///
/// Clones share one state, so every shard of a worker group feeds one [`WebRtc`]
/// and counts into one [`Counters`]. Set it as
/// [`server::Config::demux`](crate::server::Config::demux) and keep a clone.
#[derive(Clone, Debug, Default)]
pub struct Demux {
	/// Liveness: a [`WebRtc`] consumes it, and closes once every clone and every
	/// listening socket's [`Divert`] is gone.
	live: kio::Producer<()>,
	shared: Arc<Shared>,
	/// The listening sockets a [`WebRtc`] answers through.
	sockets: Arc<Mutex<Vec<Answer>>>,
}

#[derive(Debug)]
struct Shared {
	flows: Flows,
	counters: Counters,
	queue: kio::Shared<Queue<Datagram>>,
}

impl Default for Shared {
	fn default() -> Self {
		Self {
			flows: Flows::default(),
			counters: Counters::default(),
			queue: kio::Shared::new(Queue::new(CAPACITY)),
		}
	}
}

impl Demux {
	/// The datagrams this listener dropped, per reason.
	pub fn counters(&self) -> &Counters {
		&self.shared.counters
	}

	/// Start diverting the WebRTC media path to a feed.
	///
	/// Until a feed exists, and again once every feed is dropped, that traffic is
	/// dropped as [`Discard::Unclaimed`]. Feeds share one queue.
	pub fn webrtc(&self) -> WebRtc {
		// The first feed starts clean rather than reading what a previous one left.
		if !self.live.is_used() {
			self.shared.queue.lock().clear();
		}
		WebRtc {
			live: self.live.consume(),
			shared: self.shared.clone(),
			sockets: self.sockets.clone(),
		}
	}

	/// The receive half a listening socket runs, which keeps the feed open until the
	/// endpoint releases the socket, so the feed ends with the listener even when no
	/// clone of this is kept.
	pub(crate) fn divert(&self) -> Divert {
		Divert {
			live: self.live.clone(),
			shared: self.shared.clone(),
		}
	}

	/// Run the split on `socket`, leaving only QUIC for the endpoint that reads it.
	pub(crate) fn wrap(&self, socket: Box<dyn AsyncUdpSocket>) -> Box<dyn AsyncUdpSocket> {
		Box::new(Split {
			inner: socket,
			divert: self.divert(),
		})
	}

	/// Answer through `endpoint`'s socket, one this demux [`wrap`](Self::wrap)ped.
	///
	/// Only its slot is kept, which the endpoint empties when it releases the port.
	pub(crate) fn attach(&self, endpoint: &Endpoint) {
		self.sockets.lock().unwrap().push(Answer {
			slot: endpoint.inner.slot.clone(),
			ipv6: endpoint.local_addr().is_ipv6(),
		});
	}
}

/// A listening socket a [`WebRtc`] answers through.
#[derive(Debug)]
struct Answer {
	slot: Arc<Slot>,
	/// Whether the socket is AF_INET6, so an IPv4 destination needs its mapped form.
	ipv6: bool,
}

/// A datagram diverted to the WebRTC media path.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Datagram {
	/// The sender, with an IPv4-mapped address folded to IPv4.
	pub remote: SocketAddr,
	/// The local address it arrived on, where the platform reports it.
	pub local: Option<IpAddr>,
	/// The whole datagram.
	pub data: Bytes,
}

/// The WebRTC media path of a [`Demux`]: ICE, DTLS, and RTP from its
/// socket, and a way to answer through it.
///
/// Diverting runs while any feed is alive; drop it to stop.
#[derive(Debug)]
pub struct WebRtc {
	live: kio::Consumer<()>,
	shared: Arc<Shared>,
	sockets: Arc<Mutex<Vec<Answer>>>,
}

impl WebRtc {
	/// Poll for the next datagram; `None` once every [`Demux`] clone is gone and
	/// every listening socket it split is released.
	pub fn poll_recv(&self, waiter: &kio::Waiter) -> Poll<Option<Datagram>> {
		if let Poll::Ready(mut queue) = self.shared.queue.poll(waiter, |queue| match queue.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}) {
			return Poll::Ready(queue.pop());
		}
		// Nothing queued; nothing more arrives once the listener is gone.
		match self.live.poll_closed(waiter) {
			Poll::Ready(()) => Poll::Ready(self.shared.queue.lock().pop()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Receive the next datagram; `None` once the listener and every [`Demux`]
	/// clone are gone; see [`Self::poll_recv`].
	pub async fn recv(&self) -> Option<Datagram> {
		kio::wait(|waiter| self.poll_recv(waiter)).await
	}

	/// Poll to send `transmit` from the shared port, with GSO and ECN as it asks.
	///
	/// A datagram sent once the listener has released its socket is lost, as UDP
	/// allows. Errors are those of the endpoint's own sends: an unreachable destination
	/// comes back, as [`AsyncUdpSocket::try_send`] describes, and anything else is logged.
	pub fn poll_send(&self, waiter: &kio::Waiter, transmit: &Transmit<'_>) -> Poll<io::Result<()>> {
		// Any member of a reuseport group answers from the same port.
		let sockets = self.sockets.lock().unwrap();
		let Some(socket) = sockets.iter().find(|socket| !socket.slot.is_released()) else {
			return Poll::Ready(Ok(()));
		};
		let destination = match (transmit.destination, socket.ipv6) {
			(SocketAddr::V4(v4), true) => SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port()),
			(destination, _) => destination,
		};
		let transmit = Transmit {
			destination,
			..*transmit
		};
		socket.slot.poll_send(&transmit, &mut waiter.context())
	}

	/// Send `transmit` from the shared port; see [`Self::poll_send`].
	pub async fn send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
		kio::wait(|waiter| self.poll_send(waiter, transmit)).await
	}

	/// Route every datagram from `remote` to this feed, whatever its first byte,
	/// until the returned [`Pin`] drops.
	pub fn pin(&self, remote: SocketAddr) -> Pin {
		self.shared.flows.pin(remote)
	}
}

/// A listening socket that runs its [`Divert`] on every receive.
#[derive(Debug)]
struct Split {
	inner: Box<dyn AsyncUdpSocket>,
	divert: Divert,
}

impl AsyncUdpSocket for Split {
	fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [RecvMeta],
	) -> Poll<io::Result<usize>> {
		self.inner
			.poll_recv(cx, bufs, meta)
			.map_ok(|received| self.divert.split(bufs, meta, received))
	}

	fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
		self.inner.try_send(transmit)
	}

	fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		self.inner.poll_writable(cx)
	}

	fn local_addr(&self) -> io::Result<SocketAddr> {
		self.inner.local_addr()
	}

	fn max_receive_segments(&self) -> usize {
		self.inner.max_receive_segments()
	}

	fn max_transmit_segments(&self) -> usize {
		self.inner.max_transmit_segments()
	}

	fn may_fragment(&self) -> bool {
		self.inner.may_fragment()
	}
}

/// The receive half of a [`Demux`], run by each listening socket.
#[derive(Clone, Debug)]
pub(crate) struct Divert {
	live: kio::Producer<()>,
	shared: Arc<Shared>,
}

impl Divert {
	/// Route the `received` batches in `bufs` and `metas`, keeping only QUIC.
	///
	/// QUIC segments are compacted to the front of each batch and the batches
	/// emptied by the split are removed, so the endpoint reads the first
	/// returned-count entries as usual. That count may be zero.
	pub(crate) fn split(&self, bufs: &mut [IoSliceMut<'_>], metas: &mut [RecvMeta], received: usize) -> usize {
		let mut kept = 0;
		for i in 0..received {
			let mut meta = metas[i];
			meta.len = self.split_batch(&mut bufs[i][..meta.len], &meta);
			if meta.len == 0 {
				continue;
			}
			if kept != i {
				let (head, tail) = bufs.split_at_mut(i);
				head[kept][..meta.len].copy_from_slice(&tail[0][..meta.len]);
			}
			metas[kept] = meta;
			kept += 1;
		}
		kept
	}

	/// Route each `meta.stride` segment of one batch, returning the length of the
	/// QUIC segments left at its front.
	///
	/// Only the last segment of a batch may be short, and the QUIC ones keep their
	/// order, so the compacted batch keeps the same stride.
	fn split_batch(&self, buf: &mut [u8], meta: &RecvMeta) -> usize {
		let mut kept = 0;
		let mut start = 0;
		while start < buf.len() {
			let end = buf.len().min(start + meta.stride.max(1));
			match self.shared.flows.route(meta.addr, &buf[start..end]) {
				Route::Quic => {
					buf.copy_within(start..end, kept);
					kept += end - start;
				}
				Route::WebRtc => self.push(meta, &buf[start..end]),
				Route::Discard(reason) => self.shared.counters.discard(reason),
			}
			start = end;
		}
		kept
	}

	fn push(&self, meta: &RecvMeta, data: &[u8]) {
		if !self.live.is_used() {
			self.shared.counters.discard(Discard::Unclaimed);
			return;
		}
		let remote = SocketAddr::new(meta.addr.ip().to_canonical(), meta.addr.port());
		let datagram = Datagram {
			remote,
			local: meta.dst_ip.map(|ip| ip.to_canonical()),
			data: Bytes::copy_from_slice(data),
		};
		let cost = data.len() + size_of::<Datagram>();
		if self.shared.queue.lock().push(remote, cost, datagram).is_err() {
			self.shared.counters.discard(Discard::Overflow);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const CHECK: &[u8] = &[
		0x00, 0x01, 0x00, 0x08, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, // header
		0x00, 0x06, 0x00, 0x03, b'a', b':', b'b', 0, // USERNAME
	];
	const QUERY: &[u8] = &[
		0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
	];

	fn remote(port: u16) -> SocketAddr {
		SocketAddr::from(([192, 0, 2, 1], port))
	}

	/// Split `batches`, each one GRO batch of equal-stride segments from one source,
	/// and return what the endpoint would read.
	fn split(divert: &Divert, batches: &[(SocketAddr, &[&[u8]])]) -> Vec<(SocketAddr, usize, Vec<u8>)> {
		let mut storage = vec![[0u8; 1024]; batches.len()];
		let mut metas = vec![RecvMeta::default(); batches.len()];
		for ((buf, meta), (addr, segments)) in storage.iter_mut().zip(&mut metas).zip(batches) {
			let stride = segments[0].len();
			let mut len = 0;
			for segment in *segments {
				buf[len..len + segment.len()].copy_from_slice(segment);
				len += stride;
			}
			len -= stride - segments.last().unwrap().len();
			meta.addr = *addr;
			meta.len = len;
			meta.stride = stride;
		}
		let mut bufs: Vec<_> = storage.iter_mut().map(|buf| IoSliceMut::new(buf)).collect();
		let kept = divert.split(&mut bufs, &mut metas, batches.len());
		metas[..kept]
			.iter()
			.zip(&bufs)
			.map(|(meta, buf)| (meta.addr, meta.stride, buf[..meta.len].to_vec()))
			.collect()
	}

	fn drain(feed: &WebRtc) -> Vec<(SocketAddr, Vec<u8>)> {
		std::iter::from_fn(|| match feed.poll_recv(&kio::Waiter::noop()) {
			Poll::Ready(Some(datagram)) => Some((datagram.remote, datagram.data.to_vec())),
			_ => None,
		})
		.collect()
	}

	#[test]
	fn each_class_reaches_its_stack() {
		let demux = Demux::default();
		let feed = demux.webrtc();
		let kept = split(
			&demux.divert(),
			&[
				(remote(1), &[&[0x40, 1]]),
				(remote(2), &[CHECK]),
				(remote(3), &[&[0x16, 0xfe, 0xfd]]),
				(remote(4), &[&[0x80, 0x00]]),
				(remote(5), &[&[0xc0, 5]]),
				(remote(6), &[QUERY]),
				(remote(7), &[&[5]]),
			],
		);

		// QUIC compacts to the front; the diverted batches leave no gap.
		assert_eq!(kept, [(remote(1), 2, vec![0x40, 1]), (remote(5), 2, vec![0xc0, 5])]);
		assert_eq!(
			drain(&feed),
			[
				(remote(2), CHECK.to_vec()),
				(remote(3), vec![0x16, 0xfe, 0xfd]),
				(remote(4), vec![0x80, 0x00]),
			]
		);
		assert_eq!(demux.counters().get(Discard::Stun), 1);
		assert_eq!(demux.counters().get(Discard::Unknown), 1);
	}

	#[test]
	fn a_mixed_batch_keeps_its_stride() {
		let demux = Demux::default();
		let feed = demux.webrtc();
		let kept = split(
			&demux.divert(),
			&[(remote(1), &[&[0x40, 1, 1], &[0x80, 2, 2], &[0x41, 3, 3], &[0x42, 4]])],
		);

		assert_eq!(kept, [(remote(1), 3, vec![0x40, 1, 1, 0x41, 3, 3, 0x42, 4])]);
		assert_eq!(drain(&feed), [(remote(1), vec![0x80, 2, 2])]);
	}

	#[test]
	fn a_pinned_remote_reaches_webrtc_whatever_it_sends() {
		let demux = Demux::default();
		let feed = demux.webrtc();
		let pin = feed.pin(remote(1));
		let kept = split(
			&demux.divert(),
			&[(remote(1), &[&[0x40, 1]]), (remote(2), &[&[0x40, 2]])],
		);
		assert_eq!(kept, [(remote(2), 2, vec![0x40, 2])]);
		assert_eq!(drain(&feed), [(remote(1), vec![0x40, 1])]);

		drop(pin);
		let kept = split(&demux.divert(), &[(remote(1), &[&[0x40, 1]])]);
		assert_eq!(kept.len(), 1);
	}

	#[test]
	fn webrtc_traffic_waits_for_a_feed() {
		let demux = Demux::default();
		let divert = demux.divert();
		let rtp: &[(SocketAddr, &[&[u8]])] = &[(remote(1), &[&[0x80, 0]])];

		// Nobody reads it yet, so nothing is held for later.
		assert!(split(&divert, rtp).is_empty());
		assert_eq!(demux.counters().get(Discard::Unclaimed), 1);

		let feed = demux.webrtc();
		split(&divert, rtp);
		drop(feed);
		split(&divert, rtp);
		assert_eq!(demux.counters().get(Discard::Unclaimed), 2);

		// A new feed starts clean, without the datagram the dropped one left.
		let feed = demux.webrtc();
		assert!(drain(&feed).is_empty());
	}

	#[test]
	fn a_flooding_source_overflows_alone() {
		let demux = Demux::default();
		let feed = demux.webrtc();
		let divert = demux.divert();
		let big = [0x80; 1024];
		let share = CAPACITY / Queue::<Datagram>::SHARE / big.len();
		for _ in 0..=share {
			split(&divert, &[(remote(1), &[&big])]);
		}
		let overflow = demux.counters().get(Discard::Overflow);
		assert!(overflow > 0);

		split(&divert, &[(remote(2), &[&big])]);
		assert_eq!(demux.counters().get(Discard::Overflow), overflow);
		assert_eq!(drain(&feed).last().map(|(remote, _)| *remote), Some(remote(2)));
	}

	#[test]
	fn the_feed_ends_with_the_listener() {
		let demux = Demux::default();
		let feed = demux.webrtc();
		let divert = demux.divert();
		split(&divert, &[(remote(1), &[&[0x80, 0]])]);
		drop(demux);

		// The listening socket still diverts, so the feed stays open.
		let waiter = kio::Waiter::noop();
		assert!(matches!(feed.poll_recv(&waiter), Poll::Ready(Some(_))));
		assert!(feed.poll_recv(&waiter).is_pending());

		// What was queued still arrives, then the end.
		split(&divert, &[(remote(1), &[&[0x80, 0]])]);
		drop(divert);
		assert!(matches!(feed.poll_recv(&waiter), Poll::Ready(Some(_))));
		assert!(matches!(feed.poll_recv(&waiter), Poll::Ready(None)));
	}
}
