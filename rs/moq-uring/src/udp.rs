//! UDP through the worker's ring: batched receive, GSO send.
//!
//! Receive is one multishot `recvmsg` per socket feeding from a registered
//! provided-buffer ring: each completion consumes one whole buffer, so every
//! buffer is sized for the worst case (a full `UDP_GRO` coalesce plus the
//! recvmsg header). Incremental consumption (`IOU_PBUF_RING_INC`) looked like
//! a better fit for GRO's 100-byte-to-64KB completion variance, but it cannot
//! back a multishot `recvmsg`: the kernel releases an incremental buffer only
//! at exactly zero bytes left, and `io_recvmsg_prep_multishot` fails with
//! `EFAULT` the moment a leftover tail is smaller than the recvmsg header.
//! A completed buffer is handed out as a [`Packet`] and returns to the kernel
//! once every packet borrowing it drops.
//!
//! Send stages datagrams in a pool of buffers owned by id and released
//! explicitly on completion, the shape `SENDMSG_ZC`'s deferred-reclaim NOTIF
//! model needs later. Every GSO `sendmsg` carries its `UDP_SEGMENT` control
//! message explicitly; the socket default is never relied on. The ECN mark
//! rides beside it as `IP_TOS` or `IPV6_TCLASS`, and receives read the mark
//! back through `IP_RECVTOS` and `IPV6_RECVTCLASS`, so a QUIC stack's ECN
//! validation sees what the network did to its packets. Packet info rides
//! the same way: each receive reads the local address it arrived on
//! (`IP_PKTINFO` or `IPV6_RECVPKTINFO`), and a send can name its source with
//! `IP_PKTINFO` or `IPV6_PKTINFO`, so a wildcard bind on a multihomed host
//! answers from the address the peer reached.
//!
//! Both pools are queues of concurrent operations, not byte budgets: one send
//! buffer holds one GSO train and one receive buffer holds one completion,
//! however little either carries. So the depth a socket needs is set by how
//! many of its connections want the socket at once, which no caller can
//! predict. Both start small and grow on demand when they starve, bounded by
//! the ceilings in [`Config`]; a pool never shrinks, so it settles at the
//! socket's high-water concurrency and the memory comes back when it drops.
//!
//! The `gro`/`gso`/`multishot` toggles in [`Config`] exist for the ablation
//! benchmarks; production callers keep the defaults (all on).

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, SocketAddr, SocketAddrV6, UdpSocket};
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::task::Poll;

use io_uring::{cqueue, opcode, types};

use crate::Error;
use crate::metrics::Counters;
use crate::shared::{Cqe, Op, Shared};
use crate::worker::Owner;

/// `CMSG_SPACE` of a `len`-byte payload.
const fn cmsg_space(len: usize) -> usize {
	// SAFETY: a pure size computation.
	unsafe { libc::CMSG_SPACE(len as _) as usize }
}

/// Space for the control messages of one receive or send: the segment size
/// (`UDP_GRO`, or a send's u16 `UDP_SEGMENT`), then the family's packet info
/// and ECN mark. A v4-mapped datagram on a v6 socket carries `IPV6_PKTINFO`
/// with `IP_TOS`, which the IPv6 set already covers.
const CONTROL_LEN: usize = {
	let int = std::mem::size_of::<libc::c_int>();
	let v4 = cmsg_space(std::mem::size_of::<libc::in_pktinfo>()) + cmsg_space(int);
	let v6 = cmsg_space(std::mem::size_of::<libc::in6_pktinfo>()) + cmsg_space(int);
	cmsg_space(int) + if v4 > v6 { v4 } else { v6 }
};
// Linux on 64-bit: the IPv6 set needs 88 bytes and the IPv4 one 80, and an
// undersized buffer would truncate every receive into a drop.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(CONTROL_LEN == 88);
/// Space reserved for the source address of each received datagram.
const NAME_LEN: usize = std::mem::size_of::<libc::sockaddr_storage>();
/// Fixed per-completion overhead of the multishot recvmsg layout.
const RECV_OVERHEAD: usize = 16 + NAME_LEN + CONTROL_LEN;
/// The largest payload one receive can produce: a full GRO coalesce.
const MAX_RECV: usize = 64 * 1024;
/// The kernel refuses GSO trains beyond this many segments.
const MAX_GSO_SEGMENTS: usize = 64;
/// The largest receive pool: the provided-buffer ring holds a power-of-two
/// number of entries and the kernel caps it here.
const MAX_RX_BUFFERS: u16 = 1 << 15;
/// Receive buffers allocated before any starvation. Enough for an idle socket;
/// the pool grows from here.
const INITIAL_RX_BUFFERS: u16 = 16;
/// Send buffers allocated before any starvation.
const INITIAL_TX_BUFFERS: u16 = 64;

/// Double a pool, bounded by its ceiling.
fn grown(len: usize, max: u16) -> Option<u16> {
	let max = usize::from(max);
	match len < max {
		true => Some(len.saturating_mul(2).clamp(1, max) as u16),
		false => None,
	}
}

/// How a socket uses the ring. The defaults are the production path; the
/// toggles exist so the benchmarks can ablate one mechanism at a time.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Coalesce received datagrams with `UDP_GRO`.
	pub gro: bool,
	/// Send with a `UDP_SEGMENT` control message instead of one `sendmsg` per
	/// datagram. A socket whose route rejects a train falls back to the latter.
	pub gso: bool,
	/// Receive through one persistent multishot `recvmsg` and the provided
	/// buffer ring, instead of re-armed oneshot receives.
	pub multishot: bool,
	/// Receive pool ceiling: at most this many buffers, and at most 32768.
	///
	/// Each receive completion consumes one buffer whatever its size, so the
	/// pool is a queue depth in packets rather than in bytes: GRO coalescing
	/// collapses as connections multiply, and the depth a socket needs follows
	/// that, not its bitrate. Buffers are allocated on demand, so this bounds
	/// the memory rather than reserving it.
	pub rx_buffers_max: u16,
	/// Receive pool: bytes per buffer. Must hold one worst-case receive.
	pub rx_buffer_len: usize,
	/// Send pool ceiling: at most this many buffers, allocated on demand.
	///
	/// One buffer stages one GSO train, so the pool is the socket's in-flight
	/// send concurrency. Set it to 1 to serialize sends.
	pub tx_buffers_max: u16,
	/// Send pool: bytes per buffer, the ceiling for one GSO train.
	pub tx_buffer_len: usize,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			gro: true,
			gso: true,
			multishot: true,
			// 16 MiB and 64 MiB of headroom at the default buffer lengths,
			// reached only by a socket that actually starves for them.
			rx_buffers_max: 256,
			rx_buffer_len: MAX_RECV + RECV_OVERHEAD,
			tx_buffers_max: 1024,
			tx_buffer_len: 64 * 1024,
		}
	}
}

/// One buffer of the receive pool.
struct RxBuf {
	/// Stable heap allocation; [`Packet`]s hold raw slices into it.
	data: Box<[u8]>,
	/// Live [`Packet`]s borrowing slices of this buffer.
	outstanding: usize,
	/// Multishot: a completion consumed this buffer, so it recycles back into
	/// the provided ring once `outstanding` drains.
	kernel_done: bool,
	/// Oneshot: an armed receive owns this buffer.
	claimed: bool,
}

/// A received-but-not-consumed packet; materialized into a [`Packet`] on pop.
struct Queued {
	bid: u16,
	start: usize,
	len: usize,
	from: SocketAddr,
	dst_ip: Option<IpAddr>,
	stride: usize,
	ecn: Option<Ecn>,
}

/// The ECN codepoint carried in the IP header's TOS or traffic class byte.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecn {
	/// ECN-capable transport, the codepoint classic ECN marks with.
	Ect0 = 0b10,
	/// ECN-capable transport, the codepoint L4S marks with.
	Ect1 = 0b01,
	/// Congestion experienced: a queue on the path marked the packet.
	Ce = 0b11,
}

impl Ecn {
	/// The codepoint in the low two bits of a TOS byte, if any.
	fn from_bits(bits: u8) -> Option<Self> {
		match bits & 0b11 {
			0b10 => Some(Self::Ect0),
			0b01 => Some(Self::Ect1),
			0b11 => Some(Self::Ce),
			_ => None,
		}
	}
}

/// What one [`TxBuf::send`] puts on the wire.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Transmit {
	/// The destination.
	pub to: SocketAddr,
	/// The datagram size; the buffer is sent in datagrams of this many bytes,
	/// the last possibly short.
	pub segment: usize,
	/// The ECN codepoint every datagram carries, if any.
	pub ecn: Option<Ecn>,
	/// The local address every datagram leaves from, or the kernel's choice.
	/// Pass a [`Packet::dst_ip`] to answer from the address a peer reached.
	pub src_ip: Option<IpAddr>,
}

impl Transmit {
	/// Datagrams of `segment` bytes to `to`, unmarked, from the kernel's choice of source.
	pub fn new(to: SocketAddr, segment: usize) -> Self {
		Self {
			to,
			segment,
			ecn: None,
			src_ip: None,
		}
	}
}

/// Free `bid` back to its pool if nothing borrows it any more. Returns whether
/// the buffer became available for a new receive.
fn recycle_if_idle(rx: &mut Rx, bid: u16) -> bool {
	let buf = &mut rx.bufs[bid as usize];
	if buf.outstanding > 0 {
		return false;
	}
	if buf.claimed {
		// Oneshot: the buffer frees wholesale.
		buf.claimed = false;
		return true;
	}
	if !buf.kernel_done {
		// Multishot: the kernel still owns it.
		return false;
	}
	// Multishot: hand the whole buffer back to the kernel.
	buf.kernel_done = false;
	let addr = buf.data.as_mut_ptr();
	let len = buf.data.len();
	if let Some(ring) = &mut rx.ring {
		ring.add(bid, addr, len);
		ring.publish();
	}
	true
}

/// Whether the receive pool has proven too shallow to arm against as it is.
///
/// A recorded starvation counts on its own: a buffer recycled between the
/// kernel's `ENOBUFS` and this re-arm masks the shortfall without answering
/// it, and arming into that one buffer just starves again.
fn should_grow(rx: &Rx, multishot: bool) -> bool {
	if rx.starved {
		return true;
	}
	match multishot {
		// Nothing left in the provided ring for the kernel to receive into.
		true => !rx.bufs.iter().any(|buf| !buf.kernel_done),
		// Every buffer is claimed by a receive or borrowed by a packet.
		false => !rx.bufs.iter().any(|buf| !buf.claimed && buf.outstanding == 0),
	}
}

/// Allocate more receive buffers and hand them straight to the kernel, up to
/// [`Config::rx_buffers_max`]. Returns whether the pool grew.
///
/// Only the `RxBuf` structs move; [`Packet`] and the provided ring both point
/// at the `Box<[u8]>` allocations, which stay put.
fn grow_rx(rx: &mut Rx, config: &Config) -> bool {
	let Some(target) = grown(rx.bufs.len(), config.rx_buffers_max) else {
		return false;
	};
	while rx.bufs.len() < usize::from(target) {
		let bid = rx.bufs.len() as u16;
		rx.bufs.push(RxBuf {
			data: vec![0u8; config.rx_buffer_len].into_boxed_slice(),
			outstanding: 0,
			kernel_done: false,
			claimed: false,
		});
		if let Some(ring) = &mut rx.ring {
			let buf = &mut rx.bufs[bid as usize];
			let addr = buf.data.as_mut_ptr();
			let len = buf.data.len();
			ring.add(bid, addr, len);
		}
	}
	if let Some(ring) = &mut rx.ring {
		ring.publish();
	}
	true
}

/// Allocate more send buffers, up to [`Config::tx_buffers_max`]. Returns
/// whether the pool grew.
///
/// The payload allocations are stable across the `Vec` growth, which is what
/// lets an in-flight send keep a raw pointer into one.
fn grow_tx(tx: &mut Tx, config: &Config) -> bool {
	let Some(target) = grown(tx.bufs.len(), config.tx_buffers_max) else {
		return false;
	};
	while tx.bufs.len() < usize::from(target) {
		tx.free.push(tx.bufs.len() as u16);
		tx.bufs.push(TxSlot::new(config.tx_buffer_len));
	}
	true
}

/// The registered provided-buffer ring: kernel-shared memory we own.
struct BufRing {
	/// The buffer group id the ring is registered under.
	bgid: u16,
	ptr: NonNull<types::BufRingEntry>,
	layout: Layout,
	mask: u16,
	tail: u16,
}

impl BufRing {
	fn new(entries: u16, bgid: u16) -> Self {
		let layout = Layout::from_size_align(entries as usize * std::mem::size_of::<types::BufRingEntry>(), 4096)
			.expect("buffer ring layout");
		// SAFETY: layout is non-zero.
		let ptr = unsafe { alloc_zeroed(layout) };
		let ptr = NonNull::new(ptr.cast::<types::BufRingEntry>()).unwrap_or_else(|| handle_alloc_error(layout));
		Self {
			bgid,
			ptr,
			layout,
			mask: entries - 1,
			tail: 0,
		}
	}

	/// Stage one buffer for the kernel; call [`publish`](Self::publish) after.
	fn add(&mut self, bid: u16, addr: *mut u8, len: usize) {
		let index = (self.tail & self.mask) as usize;
		// SAFETY: index is masked into the allocation.
		let entry = unsafe { &mut *self.ptr.as_ptr().add(index) };
		entry.set_addr(addr as u64);
		entry.set_len(len as u32);
		entry.set_bid(bid);
		self.tail = self.tail.wrapping_add(1);
	}

	/// Make staged buffers visible to the kernel.
	fn publish(&self) {
		// SAFETY: the tail pointer lives inside the registered allocation.
		let tail = unsafe { types::BufRingEntry::tail(self.ptr.as_ptr()) }.cast_mut();
		// SAFETY: the kernel reads this address atomically.
		unsafe { AtomicU16::from_ptr(tail) }.store(self.tail, Ordering::Release);
	}
}

impl Drop for BufRing {
	fn drop(&mut self) {
		// SAFETY: allocated in `new` with this layout; the caller unregisters
		// the ring (or has torn down the io_uring) before dropping.
		unsafe { dealloc(self.ptr.as_ptr().cast(), self.layout) };
	}
}

/// Receive-side state.
struct Rx {
	bufs: Vec<RxBuf>,
	ring: Option<BufRing>,
	/// Multishot: the recvmsg header template (name + control sizes).
	hdr: Box<libc::msghdr>,
	queue: VecDeque<Queued>,
	waiters: kio::WaiterList,
	/// The slab key of the armed receive, if one is in flight.
	armed: Option<u64>,
	/// The kernel ran the pool dry since the last arm. Held rather than acted
	/// on immediately because the re-arm is what can grow the pool.
	starved: bool,
	/// Terminal failure, surfaced by `poll_recv` once the queue drains.
	error: Option<i32>,
	/// Destinations a send found no route to, each with its errno, for the
	/// demux task to hand to the connections dialing them. Here rather than
	/// on the send side because the demux task is what parks on `waiters`.
	/// `None` until a consumer asks for them, so a raw socket queues nothing.
	unreachable: Option<Vec<(SocketAddr, i32)>>,
}

/// Send-side state.
struct Tx {
	bufs: Vec<TxSlot>,
	free: Vec<u16>,
	waiters: kio::WaiterList,
	/// Whether an acquisition is already waiting for the drained pool.
	stalled: bool,
	/// Terminal failure, surfaced by `poll_acquire`.
	error: Option<i32>,
}

/// Stable storage reused by every checkout of one transmit slot: the payload
/// the kernel reads and the `sendmsg` headers pointing into it.
struct TxSlot {
	/// Empty while a [`TxBuf`] has it checked out; sending or dropping that
	/// hands the payload back.
	data: Vec<u8>,
	headers: Vec<SendHdr>,
	/// Sends staged from this slot that the kernel has not completed. The slot
	/// returns to the free list when it hits zero.
	in_flight: usize,
}

impl TxSlot {
	fn new(len: usize) -> Self {
		Self {
			data: Vec::with_capacity(len),
			headers: Vec::new(),
			in_flight: 0,
		}
	}
}

/// A bound socket a worker takes over, with whatever identity it carries.
///
/// Both variants convert with `From`, so [`crate::Handle::udp`] takes either
/// as is. The member is the only way a socket gets a steering slot: the
/// group that completed it is what proved the slot is real.
pub enum Bound {
	/// A socket on its own, bound by the caller.
	Lone(UdpSocket),
	/// One member of a steered `SO_REUSEPORT` group. Every connection id an
	/// endpoint on it issues then names the member's
	/// [`Shard`](moq_sock::shard::Shard), so the group's filter keeps
	/// delivering a connection's packets to this socket.
	Member(moq_sock::shard::Socket),
}

impl From<UdpSocket> for Bound {
	fn from(socket: UdpSocket) -> Self {
		Self::Lone(socket)
	}
}

impl From<moq_sock::shard::Socket> for Bound {
	fn from(member: moq_sock::shard::Socket) -> Self {
		Self::Member(member)
	}
}

/// Everything both the [`Socket`] handle and in-flight ops keep alive.
pub(crate) struct SockShared {
	io: UdpSocket,
	/// The worker, as the I/O built on this socket carries it.
	owner: Owner,
	/// This socket's slot in a steered reuseport group, if it is in one.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	shard: Option<moq_sock::shard::Shard>,
	/// The worker's counters, held directly rather than reached through
	/// `owner`, so counting a datagram is not a `Weak::upgrade`.
	metrics: std::sync::Arc<Counters>,
	config: Config,
	/// Starts as `config.gso`, and clears for good once a rejected train's
	/// datagrams go out alone: the route or driver cannot segment.
	gso: Cell<bool>,
	/// When a lost send was last logged, so a flood of them logs once per interval.
	lost_logged: Cell<Option<moq_clock::Instant>>,
	closed: Cell<bool>,
	rx: RefCell<Rx>,
	tx: RefCell<Tx>,
}

impl SockShared {
	/// Whether the worker loop that would drive this socket is gone: dropped
	/// outright, or torn down while handles keep the shared state alive.
	fn worker_gone(&self) -> bool {
		self.owner.handle().is_none()
	}

	/// A packet released its buffer slice.
	fn release_rx(self: &Rc<Self>, bid: u16) {
		let mut rx = self.rx.borrow_mut();
		rx.bufs[bid as usize].outstanding -= 1;
		if !recycle_if_idle(&mut rx, bid) {
			return;
		}
		// A receive that died on ENOBUFS can start again now.
		if rx.armed.is_none() && rx.error.is_none() {
			drop(rx);
			if let Some(shared) = self.owner.upgrade() {
				arm_recv(&shared, self);
			}
		}
	}

	fn release_tx(&self, id: u16, data: Vec<u8>) {
		let mut tx = self.tx.borrow_mut();
		let slot = &mut tx.bufs[id as usize];
		debug_assert_eq!(slot.in_flight, 0);
		slot.data = data;
		tx.free.push(id);
		tx.stalled = false;
		tx.waiters.wake();
	}

	fn stage_tx(&self, id: u16) {
		self.tx.borrow_mut().bufs[id as usize].in_flight += 1;
	}

	fn complete_tx(&self, id: u16) {
		let mut tx = self.tx.borrow_mut();
		let slot = &mut tx.bufs[id as usize];
		debug_assert!(slot.in_flight > 0);
		slot.in_flight -= 1;
		if slot.in_flight == 0 {
			tx.free.push(id);
			tx.stalled = false;
			tx.waiters.wake();
		}
	}

	fn fail_rx(&self, code: i32) {
		let mut rx = self.rx.borrow_mut();
		rx.error.get_or_insert(code);
		rx.waiters.wake();
	}

	/// A send to `to` found no route, which says nothing about the socket, so
	/// with a consumer only the connections dialing `to` hear it. Without one
	/// there is no destination to route it to, so it fails the socket: a raw
	/// socket has no other way to report it.
	fn unreachable(&self, to: SocketAddr, code: i32) {
		let mut rx = self.rx.borrow_mut();
		let Some(reports) = rx.unreachable.as_mut() else {
			drop(rx);
			self.fail_tx(code);
			return;
		};
		if !reports.iter().any(|(addr, _)| *addr == to) {
			reports.push((to, code));
		}
		rx.waiters.wake();
	}

	fn fail_tx(&self, code: i32) {
		let mut tx = self.tx.borrow_mut();
		tx.error.get_or_insert(code);
		tx.waiters.wake();
	}

	/// `op` is lost, which QUIC recovers like any other loss: log it, at most once an interval,
	/// except the losses a peer causes as a matter of course.
	fn lost(&self, op: &SendOp, err: io::Error) {
		if matches!(err.raw_os_error(), Some(libc::ECONNREFUSED | libc::EMSGSIZE)) {
			tracing::debug!(%err, to = %op.to, "send lost");
			return;
		}
		let now = moq_clock::now();
		if self
			.lost_logged
			.get()
			.is_some_and(|last| now.saturating_duration_since(last) < LOST_LOG_INTERVAL)
		{
			return;
		}
		self.lost_logged.set(Some(now));
		tracing::warn!(%err, to = %op.to, len = op.expect, "send lost");
	}
}

impl Drop for SockShared {
	fn drop(&mut self) {
		// Every op referencing our buffers has completed (ops own an `Rc` of
		// us), so the kernel is done; give the buffer group id back once it
		// has let go of the ring.
		if let Some(ring) = &self.rx.get_mut().ring
			&& let Some(shared) = self.owner.upgrade()
		{
			let unregistered = shared.ring.borrow().submitter().unregister_buf_ring(ring.bgid);
			match unregistered {
				Ok(()) => shared.bgids.borrow_mut().give(ring.bgid),
				// Still registered, so the id stays out of circulation.
				Err(err) => tracing::warn!(%err, bgid = ring.bgid, "failed to unregister a buffer ring"),
			}
		}
	}
}

/// A UDP socket driven by a [`crate::Worker`].
///
/// Created by [`crate::Handle::udp`]. Dropping it cancels the armed receive
/// and releases the socket once the kernel confirms.
pub struct Socket {
	shared: Rc<SockShared>,
}

impl Socket {
	/// A test-only observer for whether every kernel operation released this socket.
	#[cfg(test)]
	pub(crate) fn downgrade(&self) -> std::rc::Weak<SockShared> {
		Rc::downgrade(&self.shared)
	}

	pub(crate) fn bind(shared: &Rc<Shared>, bound: Bound, config: Config) -> Result<Self, Error> {
		let (io, shard) = match bound {
			Bound::Lone(io) => (io, None),
			Bound::Member(member) => {
				let shard = member.shard();
				(member.into_inner(), Some(shard))
			}
		};
		let floor = if config.gro { MAX_RECV + RECV_OVERHEAD } else { 2048 };
		if config.rx_buffer_len < floor || config.rx_buffers_max == 0 || config.tx_buffers_max == 0 {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"receive buffers must hold one worst-case receive ({floor} bytes) and both pools need at least one buffer"
				),
			)
			.into());
		}
		// Rounding up past `u16::MAX` would wrap to a zero-entry ring.
		if config.rx_buffers_max > MAX_RX_BUFFERS {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"receive pool holds at most {MAX_RX_BUFFERS} buffers, got {}",
					config.rx_buffers_max
				),
			)
			.into());
		}

		if config.gro {
			set_option(&io, libc::SOL_UDP, libc::UDP_GRO)?;
		}
		// Receive the ECN codepoint with each datagram. A v6 socket takes
		// both: Linux reports a v4-mapped datagram's mark as `IP_TOS`.
		// The local address a datagram arrived on comes as `IPV6_PKTINFO` on a
		// v6 socket, v4-mapped for an IPv4 datagram, and `IP_PKTINFO` on a v4 one.
		if io.local_addr()?.is_ipv6() {
			set_option(&io, libc::IPPROTO_IPV6, libc::IPV6_RECVTCLASS)?;
			set_option(&io, libc::IPPROTO_IPV6, libc::IPV6_RECVPKTINFO)?;
		} else {
			set_option(&io, libc::IPPROTO_IP, libc::IP_PKTINFO)?;
		}
		set_option(&io, libc::IPPROTO_IP, libc::IP_RECVTOS)?;
		// Send at the interface MTU and never fragment, as moq-sock does, so a forged ICMP
		// fragmentation-needed cannot shrink a path below a GSO train and turn its sends into
		// EMSGSIZE or a worker-wide GSO fallback. The v4 option covers v4-mapped peers on a v6
		// socket too. IPv6 also needs DONTFRAG, since its output path can still fragment under
		// PMTUDISC_PROBE alone.
		set_value(&io, libc::IPPROTO_IP, libc::IP_MTU_DISCOVER, libc::IP_PMTUDISC_PROBE)?;
		if io.local_addr()?.is_ipv6() {
			set_value(
				&io,
				libc::IPPROTO_IPV6,
				libc::IPV6_MTU_DISCOVER,
				libc::IPV6_PMTUDISC_PROBE,
			)?;
			set_option(&io, libc::IPPROTO_IPV6, libc::IPV6_DONTFRAG)?;
		}

		// The ring's entry count is fixed at registration, so it is sized for
		// the ceiling; the buffers behind it are allocated as the pool grows.
		let rx_cap = config.rx_buffers_max.next_power_of_two();
		let rx_count = INITIAL_RX_BUFFERS.min(config.rx_buffers_max);
		let mut bufs = Vec::with_capacity(rx_count as usize);
		for _ in 0..rx_count {
			bufs.push(RxBuf {
				data: vec![0u8; config.rx_buffer_len].into_boxed_slice(),
				outstanding: 0,
				kernel_done: false,
				claimed: false,
			});
		}

		let ring = if config.multishot {
			let bgid = shared.bgids.borrow_mut().take().ok_or_else(|| {
				io::Error::new(
					io::ErrorKind::OutOfMemory,
					"every buffer group id on this worker names a live socket",
				)
			})?;
			let mut ring = BufRing::new(rx_cap, bgid);
			// SAFETY: the ring allocation lives in `SockShared`, which the
			// armed receive's `Op` keeps alive until its terminal CQE, and is
			// unregistered before it drops.
			let registered = unsafe {
				shared
					.ring
					.borrow()
					.submitter()
					.register_buf_ring_with_flags(ring.ptr.as_ptr() as u64, rx_cap, bgid, 0)
			};
			if let Err(err) = registered {
				// Nothing was registered, so the id is free again.
				shared.bgids.borrow_mut().give(bgid);
				return Err(Error::ring(err));
			}
			for (bid, buf) in bufs.iter_mut().enumerate() {
				let addr = buf.data.as_mut_ptr();
				let len = buf.data.len();
				ring.add(bid as u16, addr, len);
			}
			ring.publish();
			Some(ring)
		} else {
			None
		};

		let mut hdr: Box<libc::msghdr> = Box::new(unsafe { std::mem::zeroed() });
		hdr.msg_namelen = NAME_LEN as libc::socklen_t;
		hdr.msg_controllen = CONTROL_LEN;

		let tx_count = INITIAL_TX_BUFFERS.min(config.tx_buffers_max);
		let tx = Tx {
			bufs: (0..tx_count).map(|_| TxSlot::new(config.tx_buffer_len)).collect(),
			free: (0..tx_count).collect(),
			waiters: kio::WaiterList::new(),
			stalled: false,
			error: None,
		};

		let sock = Rc::new(SockShared {
			io,
			owner: Owner::new(shared),
			shard,
			metrics: shared.metrics.clone(),
			gso: Cell::new(config.gso),
			lost_logged: Cell::new(None),
			config,
			closed: Cell::new(false),
			rx: RefCell::new(Rx {
				bufs,
				ring,
				hdr,
				queue: VecDeque::new(),
				waiters: kio::WaiterList::new(),
				armed: None,
				starved: false,
				error: None,
				unreachable: None,
			}),
			tx: RefCell::new(tx),
		});

		arm_recv(shared, &sock);
		if let Some(code) = sock.rx.borrow().error {
			return Err(io::Error::from_raw_os_error(code).into());
		}
		Ok(Self { shared: sock })
	}

	/// The bound local address.
	pub fn local_addr(&self) -> io::Result<SocketAddr> {
		self.shared.io.local_addr()
	}

	/// The worker driving this socket, which everything built on it runs on.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub(crate) fn owner(&self) -> Owner {
		self.shared.owner.clone()
	}

	/// This socket's slot in a steered reuseport group, if it is in one.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub(crate) fn shard(&self) -> Option<moq_sock::shard::Shard> {
		self.shared.shard
	}

	/// A received packet, or the socket's terminal error, registering `waiter`
	/// while neither is available. Queued packets drain before an error
	/// surfaces.
	pub fn poll_recv(&self, waiter: &kio::Waiter) -> Poll<io::Result<Packet>> {
		let mut rx = self.shared.rx.borrow_mut();
		if let Some(queued) = rx.queue.pop_front() {
			let buf = &rx.bufs[queued.bid as usize];
			// SAFETY: `start..start + len` is in bounds; the allocation is
			// stable and the range is exclusively this packet's (see Packet).
			let ptr = unsafe { NonNull::new_unchecked(buf.data.as_ptr().cast_mut().add(queued.start)) };
			return Poll::Ready(Ok(Packet {
				sock: self.shared.clone(),
				bid: queued.bid,
				ptr,
				len: queued.len,
				stride: queued.stride,
				from: queued.from,
				dst_ip: queued.dst_ip,
				ecn: queued.ecn,
			}));
		}
		if let Some(code) = rx.error {
			return Poll::Ready(Err(io::Error::from_raw_os_error(code)));
		}
		if self.shared.worker_gone() {
			return Poll::Ready(Err(Shared::gone_error()));
		}
		waiter.register(&mut rx.waiters);
		Poll::Pending
	}

	/// Report sends that find no route through
	/// [`take_unreachable`](Self::take_unreachable) instead of failing every
	/// later send, for a consumer that routes them to the right connection.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub(crate) fn route_unreachable(&self) {
		self.shared.rx.borrow_mut().unreachable.get_or_insert_default();
	}

	/// A destination a send found no route to, and why, once per report.
	///
	/// Reported through the receive waiters, so whoever parks in
	/// [`poll_recv`](Self::poll_recv) is woken to collect it.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub(crate) fn take_unreachable(&self) -> Option<(SocketAddr, io::Error)> {
		let (to, code) = self.shared.rx.borrow_mut().unreachable.as_mut()?.pop()?;
		Some((to, io::Error::from_raw_os_error(code)))
	}

	/// Await [`poll_recv`](Self::poll_recv).
	pub async fn recv(&self) -> io::Result<Packet> {
		kio::wait(|waiter| self.poll_recv(waiter)).await
	}

	/// A free send-staging buffer, registering `waiter` while the pool is
	/// drained. Backpressure lives here: the pool caps in-flight sends.
	pub fn poll_acquire(&self, waiter: &kio::Waiter) -> Poll<io::Result<TxBuf>> {
		let mut tx = self.shared.tx.borrow_mut();
		if let Some(code) = tx.error {
			return Poll::Ready(Err(io::Error::from_raw_os_error(code)));
		}
		if self.shared.worker_gone() {
			return Poll::Ready(Err(Shared::gone_error()));
		}
		if tx.free.is_empty() {
			// Starved: every buffer is in flight, so the socket needs a deeper
			// send window than it has. Grow rather than serialize behind it.
			grow_tx(&mut tx, &self.shared.config);
		}
		if let Some(id) = tx.free.pop() {
			let mut data = std::mem::take(&mut tx.bufs[id as usize].data);
			data.clear();
			// A checkout is a `Vec` its holder may have shrunk or regrown before
			// dropping it; every checkout holds exactly one pool-sized train.
			let cap = self.shared.config.tx_buffer_len;
			if data.capacity() != cap {
				data = Vec::with_capacity(cap);
			}
			return Poll::Ready(Ok(TxBuf {
				sock: self.shared.clone(),
				id,
				data,
				armed: false,
			}));
		}
		if !tx.stalled {
			tx.stalled = true;
			self.shared.metrics.tx_stalls.add(1);
		}
		waiter.register(&mut tx.waiters);
		Poll::Pending
	}

	/// Await [`poll_acquire`](Self::poll_acquire).
	pub async fn acquire(&self) -> io::Result<TxBuf> {
		kio::wait(|waiter| self.poll_acquire(waiter)).await
	}
}

impl Drop for Socket {
	fn drop(&mut self) {
		self.shared.closed.set(true);
		let rx = self.shared.rx.borrow();
		if let (Some(key), Some(shared)) = (rx.armed, self.shared.owner.upgrade()) {
			drop(rx);
			// Fire-and-forget: the cancel's own CQE is consumed by the worker,
			// and the receive's terminal CQE releases the socket state. Failing
			// to stage it means the ring itself is broken, which only the
			// worker can report.
			if let Err(err) = shared.cancel(key) {
				shared.fail(err);
			}
		}
	}
}

impl std::fmt::Debug for Socket {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Socket")
			.field("addr", &self.shared.io.local_addr())
			.finish()
	}
}

/// One receive: a possibly GRO-coalesced run of datagrams from one source.
///
/// Borrows its worker's receive pool; drop it to hand the space back. The
/// payload is `stride`-sized datagrams, the last possibly short.
pub struct Packet {
	sock: Rc<SockShared>,
	bid: u16,
	ptr: NonNull<u8>,
	len: usize,
	stride: usize,
	from: SocketAddr,
	dst_ip: Option<IpAddr>,
	ecn: Option<Ecn>,
}

impl Packet {
	/// The datagrams' source address.
	pub fn from(&self) -> SocketAddr {
		self.from
	}

	/// The local address the datagrams arrived on, v4-mapped on a v6 socket.
	pub fn dst_ip(&self) -> Option<IpAddr> {
		self.dst_ip
	}

	/// The ECN codepoint the datagrams arrived with; GRO only coalesces
	/// datagrams that share one.
	pub fn ecn(&self) -> Option<Ecn> {
		self.ecn
	}

	/// The datagram size GRO coalesced with; the final datagram may be short.
	pub fn stride(&self) -> usize {
		self.stride
	}

	/// The whole coalesced payload.
	pub fn payload(&self) -> &[u8] {
		// SAFETY: exclusive, in-bounds range of a stable allocation that the
		// `sock` Rc keeps alive; the pool never touches it while outstanding.
		unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
	}
}

impl Drop for Packet {
	fn drop(&mut self) {
		self.sock.release_rx(self.bid);
	}
}

impl std::fmt::Debug for Packet {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Packet")
			.field("from", &self.from)
			.field("dst_ip", &self.dst_ip)
			.field("len", &self.len)
			.field("stride", &self.stride)
			.field("ecn", &self.ecn)
			.finish()
	}
}

/// A checked-out send-staging buffer, empty with room for
/// [`Config::tx_buffer_len`] bytes: fill it, then [`send`](Self::send) it.
///
/// The buffer belongs to the socket it was acquired from, and sending goes
/// back through that socket. Dropping it unsent returns it to the pool.
pub struct TxBuf {
	sock: Rc<SockShared>,
	id: u16,
	data: Vec<u8>,
	armed: bool,
}

impl TxBuf {
	/// Send the buffer on the owning socket as datagrams of `segment` bytes
	/// (the last may be short), marked with `ecn`. Fire-and-forget:
	/// the buffer returns to the pool when the kernel completes. A send the
	/// kernel fails or cuts short is lost, as UDP allows, and only logged.
	/// The next pool acquire fails only once the socket itself is unusable
	/// or a send finds no route to its destination.
	///
	/// This only stages an SQE, so the datagram reaches the kernel when the
	/// worker next enters the ring. Dropping the worker makes a bounded attempt
	/// to submit staged datagrams and drain their completions, but does not
	/// guarantee kernel completion or delivery.
	pub fn send(mut self, transmit: Transmit) -> io::Result<()> {
		let Transmit {
			to,
			segment,
			ecn,
			src_ip,
		} = transmit;
		let len = self.data.len();
		let cap = self.sock.config.tx_buffer_len;
		// `UDP_SEGMENT` is a u16, so an oversized segment would silently
		// truncate into a tiny stride and explode the implied segment count.
		// A buffer grown past the pool's size is refused rather than sent, so
		// the configured ceiling holds for every train.
		if len == 0 || len > cap || segment == 0 || segment > usize::from(u16::MAX) {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!("invalid send: {len} bytes in {segment} byte segments from a {cap} byte buffer"),
			));
		}
		let shared = match self.sock.owner.upgrade() {
			Some(shared) if !shared.stopped.get() => shared,
			_ => return Err(Shared::gone_error()),
		};

		// A GSO train is one `sendmsg` the kernel caps at 64 segments. Without
		// GSO every segment is its own `sendmsg`, so the ring is the limit
		// instead: staging more than the submission queue holds makes `push`
		// submit inline and go round again without reaping a single
		// completion, which starves the worker and overflows the queue.
		let segments = len.div_ceil(segment);
		let limit = match self.sock.gso.get() {
			true => MAX_GSO_SEGMENTS,
			false => shared.ring.borrow().params().sq_entries() as usize,
		};
		if segments > limit {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!("send of {segments} datagrams exceeds the {limit} one call may stage"),
			));
		}

		self.armed = true;
		let sock = self.sock.clone();
		let (base, headers) = {
			let mut tx = sock.tx.borrow_mut();
			let slot = &mut tx.bufs[self.id as usize];
			// The slot keeps the payload while the kernel reads it. Moving the
			// `Vec` leaves its heap allocation where it is.
			slot.data = std::mem::take(&mut self.data);
			if slot.headers.len() < segments {
				slot.headers.resize_with(segments, SendHdr::zeroed);
			}
			// SAFETY: the slot is checked out, so it cannot be sent from again
			// (and its payload and headers cannot change) until every send
			// below completes and returns it to the free list.
			(slot.data.as_mut_ptr(), unsafe {
				NonNull::new_unchecked(slot.headers.as_mut_ptr())
			})
		};
		let staging = Staging {
			sock: sock.clone(),
			id: self.id,
			headers,
		};

		let one = SendOne {
			to,
			ecn,
			src_ip,
			segment: sock.gso.get().then_some(segment as u16),
			resent: false,
		};
		if sock.gso.get() {
			send_one(&shared, &staging, 0, base, len, &one)?;
		} else {
			for index in 0..segments {
				let offset = index * segment;
				let chunk = segment.min(len - offset);
				// SAFETY: offset stays within the leased buffer.
				send_one(&shared, &staging, index, unsafe { base.add(offset) }, chunk, &one)?;
			}
		}
		sock.metrics.tx_datagrams.add(segments as u64);
		Ok(())
	}
}

impl TxBuf {
	/// Stage this lease as a send to `to` that never reaches the kernel, for a test to complete
	/// with whatever result it injects.
	#[cfg(test)]
	fn fake_send(mut self, to: SocketAddr) -> SendOp {
		self.armed = true;
		self.sock.stage_tx(self.id);
		let expect = self.data.len();
		self.sock.tx.borrow_mut().bufs[self.id as usize].data = std::mem::take(&mut self.data);
		SendOp {
			sock: self.sock.clone(),
			id: self.id,
			expect,
			to,
			train: None,
			resent: false,
		}
	}
}

impl std::ops::Deref for TxBuf {
	type Target = Vec<u8>;

	fn deref(&self) -> &Vec<u8> {
		&self.data
	}
}

impl std::ops::DerefMut for TxBuf {
	fn deref_mut(&mut self) -> &mut Vec<u8> {
		&mut self.data
	}
}

impl Drop for TxBuf {
	fn drop(&mut self) {
		if !self.armed {
			self.sock.release_tx(self.id, std::mem::take(&mut self.data));
		}
	}
}

impl std::fmt::Debug for TxBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("TxBuf").field("len", &self.data.len()).finish()
	}
}

/// Control-message space, aligned like `cmsghdr` demands.
#[repr(C, align(8))]
struct Control([u8; CONTROL_LEN]);

/// The stable storage one in-flight `sendmsg` points the kernel at.
struct SendHdr {
	hdr: libc::msghdr,
	iov: libc::iovec,
	name: libc::sockaddr_storage,
	control: Control,
}

impl SendHdr {
	fn zeroed() -> Self {
		// SAFETY: all-zero is valid for these C structs.
		unsafe { std::mem::zeroed() }
	}
}

/// What every send from one [`TxBuf::send`] call stages against.
struct Staging {
	sock: Rc<SockShared>,
	id: u16,
	/// The slot's headers, one per datagram this call stages.
	headers: NonNull<SendHdr>,
}

/// One in-flight `sendmsg`. The socket owns the header and payload it points
/// the kernel at; dropping this releases the claim on that transmit slot.
pub(crate) struct SendOp {
	sock: Rc<SockShared>,
	id: u16,
	expect: usize,
	to: SocketAddr,
	/// A GSO send's payload and destination, to resend it unsegmented if the
	/// kernel rejects the train.
	train: Option<(NonNull<u8>, SendOne)>,
	/// One datagram of a rejected train, sent again alone. Its success proves
	/// the route cannot segment.
	resent: bool,
}

impl Drop for SendOp {
	fn drop(&mut self) {
		self.sock.complete_tx(self.id);
	}
}

/// What every datagram of one [`TxBuf::send`] shares.
#[derive(Clone, Copy)]
struct SendOne {
	to: SocketAddr,
	ecn: Option<Ecn>,
	src_ip: Option<IpAddr>,
	/// The `UDP_SEGMENT` size, when the call is one GSO train.
	segment: Option<u16>,
	/// A datagram of a train the kernel rejected, sent again alone.
	resent: bool,
}

fn send_one(
	shared: &Rc<Shared>,
	staging: &Staging,
	index: usize,
	base: *mut u8,
	len: usize,
	one: &SendOne,
) -> io::Result<()> {
	let SendOne {
		to,
		ecn,
		src_ip,
		segment,
		resent,
	} = *one;
	// SAFETY: `index` is within the headers `TxBuf::send` reserved, and every
	// operation gets its own.
	let hdr = unsafe { &mut *staging.headers.as_ptr().add(index) };
	*hdr = SendHdr::zeroed();
	hdr.iov = libc::iovec {
		iov_base: base.cast(),
		iov_len: len,
	};
	let name_len = encode_addr(to, &mut hdr.name);
	hdr.hdr.msg_name = (&raw mut hdr.name).cast();
	hdr.hdr.msg_namelen = name_len;
	hdr.hdr.msg_iov = &raw mut hdr.iov;
	hdr.hdr.msg_iovlen = 1;

	// SAFETY: the control buffer is zeroed, aligned, and `CONTROL_LEN` holds
	// every message at once (a u16 fits where `CONTROL_LEN` budgets an int);
	// `msg_controllen` is set to the total first so `CMSG_NXTHDR` walks it.
	unsafe {
		let mut space = 0;
		if segment.is_some() {
			space += cmsg_space(std::mem::size_of::<u16>());
		}
		if ecn.is_some() {
			space += cmsg_space(std::mem::size_of::<libc::c_int>());
		}
		space += match src_ip {
			Some(IpAddr::V4(_)) => cmsg_space(std::mem::size_of::<libc::in_pktinfo>()),
			Some(IpAddr::V6(_)) => cmsg_space(std::mem::size_of::<libc::in6_pktinfo>()),
			None => 0,
		};
		debug_assert!(space <= CONTROL_LEN);
		if space > 0 {
			hdr.hdr.msg_control = hdr.control.0.as_mut_ptr().cast();
			hdr.hdr.msg_controllen = space;
		}
		let mut cmsg = libc::CMSG_FIRSTHDR(&hdr.hdr);
		if let Some(segment) = segment {
			(*cmsg).cmsg_level = libc::SOL_UDP;
			(*cmsg).cmsg_type = libc::UDP_SEGMENT;
			(*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<u16>() as _) as usize;
			std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<u16>(), segment);
			cmsg = libc::CMSG_NXTHDR(&hdr.hdr, cmsg);
		}
		if let Some(ecn) = ecn {
			// A v4-mapped destination on a v6 socket leaves as IPv4, so the
			// mark rides `IP_TOS`; a native v6 destination takes `IPV6_TCLASS`.
			let is_ipv4 = match to.ip() {
				IpAddr::V4(_) => true,
				IpAddr::V6(v6) => v6.to_ipv4_mapped().is_some(),
			};
			let (level, kind) = match is_ipv4 {
				true => (libc::IPPROTO_IP, libc::IP_TOS),
				false => (libc::IPPROTO_IPV6, libc::IPV6_TCLASS),
			};
			(*cmsg).cmsg_level = level;
			(*cmsg).cmsg_type = kind;
			(*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as _) as usize;
			std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<libc::c_int>(), ecn as libc::c_int);
			cmsg = libc::CMSG_NXTHDR(&hdr.hdr, cmsg);
		}
		// The variant follows the socket's family: a v6 socket answers a
		// v4-mapped peer from a v4-mapped source, which Linux accepts as
		// `IPV6_PKTINFO` and sends as IPv4.
		match src_ip {
			Some(IpAddr::V4(ip)) => {
				let info = libc::in_pktinfo {
					ipi_ifindex: 0,
					ipi_spec_dst: libc::in_addr {
						s_addr: u32::from_ne_bytes(ip.octets()),
					},
					ipi_addr: libc::in_addr { s_addr: 0 },
				};
				(*cmsg).cmsg_level = libc::IPPROTO_IP;
				(*cmsg).cmsg_type = libc::IP_PKTINFO;
				(*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(&info) as _) as usize;
				std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast(), info);
			}
			Some(IpAddr::V6(ip)) => {
				let info = libc::in6_pktinfo {
					ipi6_addr: libc::in6_addr { s6_addr: ip.octets() },
					ipi6_ifindex: 0,
				};
				(*cmsg).cmsg_level = libc::IPPROTO_IPV6;
				(*cmsg).cmsg_type = libc::IPV6_PKTINFO;
				(*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(&info) as _) as usize;
				std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast(), info);
			}
			None => {}
		}
	}
	let hdr_ptr = &raw const hdr.hdr;

	// Count the send before the slab owns it, so the `SendOp` below is the only
	// thing that can release the slot. Completions only run from the worker's
	// pump, so the count cannot reach zero while this call is still staging.
	staging.sock.stage_tx(staging.id);
	let key = shared.insert(Op::Send(SendOp {
		sock: staging.sock.clone(),
		id: staging.id,
		expect: len,
		to,
		train: segment.and(NonNull::new(base)).map(|base| (base, *one)),
		resent,
	}));
	let entry = opcode::SendMsg::new(types::Fd(staging.sock.io.as_raw_fd()), hdr_ptr)
		.build()
		.user_data(key);
	if let Err(err) = shared.push(&entry) {
		shared.ops.borrow_mut().remove(key as usize);
		return Err(err);
	}
	staging.sock.metrics.tx_sends.add(1);
	Ok(())
}

/// Arm (or re-arm) the socket's receive. Failure is recorded on the socket.
pub(crate) fn arm_recv(shared: &Rc<Shared>, sock: &Rc<SockShared>) {
	if sock.closed.get() || shared.stopped.get() {
		return;
	}
	let mut rx = sock.rx.borrow_mut();
	if rx.armed.is_some() || rx.error.is_some() {
		return;
	}

	// Grow before arming when the pool has proven too shallow, so the next
	// receive has somewhere to land instead of waiting on a live packet to be
	// released and dropping every datagram until then.
	if should_grow(&rx, sock.config.multishot) {
		rx.starved = false;
		grow_rx(&mut rx, &sock.config);
	}

	let entry = if sock.config.multishot {
		// Only arm with buffers in the provided ring (`!kernel_done`), or the
		// receive would die on ENOBUFS immediately and re-arming here would
		// spin.
		if !rx.bufs.iter().any(|buf| !buf.kernel_done) {
			sock.metrics.rx_exhausted.add(1);
			return;
		}
		let key = shared.insert(Op::Recv {
			sock: sock.clone(),
			one: None,
		});
		rx.armed = Some(key);
		let bgid = rx.ring.as_ref().expect("a multishot socket has a buffer ring").bgid;
		opcode::RecvMsgMulti::new(types::Fd(sock.io.as_raw_fd()), &*rx.hdr, bgid)
			.build()
			.user_data(key)
	} else {
		// Claim a whole free buffer for this one receive.
		let Some(bid) = rx
			.bufs
			.iter()
			.position(|buf| !buf.claimed && buf.outstanding == 0)
			.map(|bid| bid as u16)
		else {
			// Every buffer is borrowed and the pool is at its ceiling; a
			// release re-arms us.
			sock.metrics.rx_exhausted.add(1);
			return;
		};
		rx.bufs[bid as usize].claimed = true;

		// SAFETY: all-zero is valid for these C structs.
		let mut one: Box<OneshotRecv> = Box::new(unsafe { std::mem::zeroed() });
		one.bid = bid;
		one.iov = libc::iovec {
			iov_base: rx.bufs[bid as usize].data.as_mut_ptr().cast(),
			iov_len: rx.bufs[bid as usize].data.len(),
		};
		one.hdr.msg_name = (&raw mut one.name).cast();
		one.hdr.msg_namelen = NAME_LEN as libc::socklen_t;
		one.hdr.msg_iov = &raw mut one.iov;
		one.hdr.msg_iovlen = 1;
		one.hdr.msg_control = one.control.0.as_mut_ptr().cast();
		one.hdr.msg_controllen = CONTROL_LEN;

		let hdr_ptr = &raw mut one.hdr;
		let key = shared.insert(Op::Recv {
			sock: sock.clone(),
			one: Some(one),
		});
		rx.armed = Some(key);
		opcode::RecvMsg::new(types::Fd(sock.io.as_raw_fd()), hdr_ptr)
			.build()
			.user_data(key)
	};

	drop(rx);
	if let Err(err) = shared.push(&entry) {
		let key = sock.rx.borrow_mut().armed.take().expect("just armed");
		shared.ops.borrow_mut().remove(key as usize);
		sock.fail_rx(err.raw_os_error().unwrap_or(libc::EIO));
	}
}

/// The oneshot receive's stable kernel-visible storage and buffer claim.
pub(crate) struct OneshotRecv {
	hdr: libc::msghdr,
	iov: libc::iovec,
	name: libc::sockaddr_storage,
	control: Control,
	bid: u16,
}

/// Handle one receive completion. `terminal` means the op left the slab (the
/// multishot ended or this was a oneshot), so a re-arm may be needed.
pub(crate) fn on_recv(
	shared: &Rc<Shared>,
	sock: &Rc<SockShared>,
	one: Option<Box<OneshotRecv>>,
	cqe: Cqe,
	terminal: bool,
) {
	if terminal {
		sock.rx.borrow_mut().armed = None;
	}

	if cqe.result < 0 {
		let code = -cqe.result;
		if let Some(one) = &one {
			let mut rx = sock.rx.borrow_mut();
			rx.bufs[one.bid as usize].claimed = false;
		}
		match code {
			// The receive pool is exhausted. Record it: by the time the re-arm
			// looks, a recycled buffer may hide that the kernel ran dry.
			libc::ENOBUFS => {
				sock.metrics.rx_enobufs.add(1);
				sock.rx.borrow_mut().starved = true;
			}
			// Socket teardown; nothing to surface.
			libc::ECANCELED => return,
			// What tokio tolerates too: ICMP surfacing on the socket (ECONNRESET), and a
			// datagram too large for the buffer (EMSGSIZE). Each costs at most one receive.
			libc::ECONNRESET | libc::EMSGSIZE => {}
			_ => {
				sock.fail_rx(code);
				return;
			}
		}
		arm_recv(shared, sock);
		return;
	}

	let received = match one {
		None => on_recv_multi(sock, cqe),
		Some(one) => on_recv_oneshot(*one, cqe),
	};
	match received {
		Ok((_, Some(queued))) => {
			sock.metrics.rx_receives.add(1);
			// Only an empty datagram has a zero stride; it is still one datagram.
			sock.metrics
				.rx_datagrams
				.add(queued.len.div_ceil(queued.stride.max(1)) as u64);
			let mut rx = sock.rx.borrow_mut();
			rx.bufs[queued.bid as usize].outstanding += 1;
			rx.queue.push_back(queued);
			rx.waiters.wake();
		}
		// A dropped (truncated/malformed) receive: UDP loss semantics. The
		// buffer space it consumed still has to recycle.
		Ok((bid, None)) => {
			recycle_if_idle(&mut sock.rx.borrow_mut(), bid);
		}
		Err(code) => {
			sock.fail_rx(code);
			return;
		}
	}
	if terminal {
		arm_recv(shared, sock);
	}
}

/// Bookkeeping for one multishot completion: the provided buffer it names,
/// consumed whole. Returns the buffer id and the packet, if any.
fn on_recv_multi(sock: &Rc<SockShared>, cqe: Cqe) -> Result<(u16, Option<Queued>), i32> {
	let mut rx = sock.rx.borrow_mut();
	let rx = &mut *rx;
	let Some(bid) = cqueue::buffer_select(cqe.flags) else {
		return Err(libc::EPROTO);
	};
	let len = cqe.result as usize;
	let buf = &mut rx.bufs[bid as usize];
	if len > buf.data.len() {
		return Err(libc::EPROTO);
	}
	// The completion consumed the buffer; it returns to the ring on recycle.
	buf.kernel_done = true;

	let slice = &buf.data[..len];
	let Ok(out) = types::RecvMsgOut::parse(slice, &rx.hdr) else {
		tracing::warn!("dropping malformed multishot recvmsg completion");
		return Ok((bid, None));
	};
	if out.is_payload_truncated() || out.is_control_data_truncated() {
		tracing::warn!("dropping truncated receive (buffer tail too small for a full coalesce)");
		return Ok((bid, None));
	}
	let Some(from) = decode_addr(out.name_data()) else {
		tracing::warn!("dropping receive with an unparseable source address");
		return Ok((bid, None));
	};
	let payload = out.payload_data();
	if payload.is_empty() {
		return Ok((bid, None));
	}
	let meta = RecvMeta::parse(out.control_data());
	let payload_start = payload.as_ptr() as usize - buf.data.as_ptr() as usize;
	Ok((
		bid,
		Some(Queued {
			bid,
			start: payload_start,
			len: payload.len(),
			from,
			dst_ip: meta.dst_ip,
			stride: meta.stride.unwrap_or(payload.len()),
			ecn: meta.ecn,
		}),
	))
}

/// Bookkeeping for one oneshot completion: the claimed buffer holds only the
/// payload; address and control came back through our own msghdr. A dropped
/// packet leaves `claimed` for the caller's recycle to clear.
fn on_recv_oneshot(one: OneshotRecv, cqe: Cqe) -> Result<(u16, Option<Queued>), i32> {
	let bid = one.bid;
	let len = cqe.result as usize;

	if one.hdr.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
		tracing::warn!("dropping truncated oneshot receive");
		return Ok((bid, None));
	}
	let name = {
		// SAFETY: the kernel wrote `msg_namelen` bytes of address.
		let ptr = (&raw const one.name).cast::<u8>();
		unsafe { std::slice::from_raw_parts(ptr, (one.hdr.msg_namelen as usize).min(NAME_LEN)) }
	};
	let Some(from) = decode_addr(name) else {
		tracing::warn!("dropping receive with an unparseable source address");
		return Ok((bid, None));
	};
	if len == 0 {
		return Ok((bid, None));
	}
	let control = &one.control.0[..one.hdr.msg_controllen.min(CONTROL_LEN)];
	let meta = RecvMeta::parse(control);
	// `claimed` stays set: the packet owns the buffer until released.
	Ok((
		bid,
		Some(Queued {
			bid,
			start: 0,
			len,
			from,
			dst_ip: meta.dst_ip,
			stride: meta.stride.unwrap_or(len),
			ecn: meta.ecn,
		}),
	))
}

/// Handle one send completion; the buffer lease releases when the last
/// completion drops its `SendOp`.
pub(crate) fn on_send(op: SendOp, cqe: Cqe) {
	if cqe.result < 0 {
		let code = -cqe.result;
		// Linux has no way to ask whether a route or driver can segment; it
		// only fails the train with EIO or EINVAL. Dropping the train would
		// cost the connection a full RTO, so send it again unsegmented. Other
		// trains already in flight fail and resend the same way.
		if let (libc::EIO | libc::EINVAL, Some((base, one))) = (code, op.train) {
			if let Err(err) = resend_unsegmented(&op, base, &one) {
				op.sock.fail_tx(err.raw_os_error().unwrap_or(libc::EIO));
			}
			return;
		}
		match SendFailure::of(code) {
			SendFailure::Cancelled => {}
			SendFailure::Unreachable => op.sock.unreachable(op.to, code),
			SendFailure::Lost => op.sock.lost(&op, io::Error::from_raw_os_error(code)),
			SendFailure::Socket => op.sock.fail_tx(code),
		}
		return;
	}
	// Segmentation is what failed only if the datagrams go out alone. A source
	// address that left the host fails the same way with or without it, and
	// must not halt segmentation for every connection sharing the socket.
	if op.resent && op.sock.gso.replace(false) {
		tracing::info!("GSO send rejected; halting segmentation offload");
	}
	if cqe.result as usize != op.expect {
		let short = format!("short send: {} of {} bytes", cqe.result, op.expect);
		op.sock.lost(&op, io::Error::other(short));
	}
}

/// What a failed send costs, decided by its errno.
#[derive(Debug, PartialEq, Eq)]
enum SendFailure {
	/// The socket is closing.
	Cancelled,
	/// No route to the destination: its connection decides whether to give up on the peer.
	Unreachable,
	/// This packet, which UDP always allows. A peer can provoke most of these, through a
	/// netfilter drop (EPERM), a full conntrack table, memory pressure, or ICMP (ECONNREFUSED,
	/// EMSGSIZE from an MTU probe), so none of them may cost the other connections sharing the
	/// socket.
	Lost,
	/// The socket itself is unusable, so every later send would fail the same way.
	Socket,
}

impl SendFailure {
	fn of(code: i32) -> Self {
		match code {
			libc::ECANCELED => Self::Cancelled,
			code if moq_sock::udp::is_unreachable_err(&io::Error::from_raw_os_error(code)) => Self::Unreachable,
			// Not a socket, closed under us, or a buffer the kernel cannot read.
			libc::EBADF | libc::ENOTSOCK | libc::EFAULT => Self::Socket,
			_ => Self::Lost,
		}
	}
}

/// How often a socket logs a lost send.
const LOST_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Stage a rejected GSO train again, one `sendmsg` per segment, from the
/// buffer `op` still holds. Runs before `op` drops, so the slot stays leased.
fn resend_unsegmented(op: &SendOp, base: NonNull<u8>, one: &SendOne) -> io::Result<()> {
	let shared = match op.sock.owner.upgrade() {
		Some(shared) if !shared.stopped.get() => shared,
		_ => return Err(Shared::gone_error()),
	};
	let segment = usize::from(one.segment.expect("only a GSO send is resent"));
	let segments = op.expect.div_ceil(segment);
	let headers = {
		let mut tx = op.sock.tx.borrow_mut();
		let headers = &mut tx.bufs[op.id as usize].headers;
		if headers.len() < segments {
			headers.resize_with(segments, SendHdr::zeroed);
		}
		// SAFETY: the kernel completed the train's only `sendmsg`, so nothing
		// points at these headers until the sends below stage them.
		unsafe { NonNull::new_unchecked(headers.as_mut_ptr()) }
	};
	let staging = Staging {
		sock: op.sock.clone(),
		id: op.id,
		headers,
	};
	let one = SendOne {
		segment: None,
		resent: true,
		..*one
	};
	for index in 0..segments {
		let offset = index * segment;
		let chunk = segment.min(op.expect - offset);
		// SAFETY: offset stays within the train, inside the leased buffer.
		send_one(
			&shared,
			&staging,
			index,
			unsafe { base.as_ptr().add(offset) },
			chunk,
			&one,
		)?;
	}
	Ok(())
}

/// What the kernel said about one receive, from its control buffer.
#[derive(Default)]
struct RecvMeta {
	/// The `UDP_GRO` segment size, if the receive was coalesced. Never zero:
	/// splitting a payload into zero-byte datagrams would never finish.
	stride: Option<usize>,
	/// The ECN codepoint from `IP_TOS` or `IPV6_TCLASS`, if marked.
	ecn: Option<Ecn>,
	/// The local address from `IP_PKTINFO` or `IPV6_PKTINFO`.
	dst_ip: Option<IpAddr>,
}

impl RecvMeta {
	/// Walk the control messages; a malformed buffer ends the walk with what
	/// was read so far.
	fn parse(control: &[u8]) -> Self {
		let mut meta = Self::default();
		let header_len = unsafe { libc::CMSG_LEN(0) as usize };
		let mut offset = 0;

		while offset + header_len <= control.len() {
			// SAFETY: bounds-checked read of a cmsghdr-sized prefix.
			let header = unsafe { control.as_ptr().add(offset).cast::<libc::cmsghdr>().read_unaligned() };
			let message_len = header.cmsg_len;
			// Compared against what remains rather than added to `offset`, which a
			// length near usize::MAX would overflow.
			if message_len < header_len || message_len > control.len() - offset {
				return meta;
			}
			let data = &control[offset + header_len..offset + message_len];
			match (header.cmsg_level, header.cmsg_type) {
				(libc::SOL_UDP, libc::UDP_GRO) => {
					meta.stride = read_int(data)
						.and_then(|value| usize::try_from(value).ok())
						.filter(|&stride| stride > 0);
				}
				// Linux reports the TOS byte itself, but the traffic class as an int.
				(libc::IPPROTO_IP, libc::IP_TOS) => {
					meta.ecn = data.first().and_then(|bits| Ecn::from_bits(*bits));
				}
				(libc::IPPROTO_IPV6, libc::IPV6_TCLASS) => {
					meta.ecn = read_int(data).and_then(|value| Ecn::from_bits(value as u8));
				}
				// `ipi_addr` is the header's destination; `ipi_spec_dst` is the
				// route's preferred source, which may differ.
				(libc::IPPROTO_IP, libc::IP_PKTINFO) => {
					meta.dst_ip =
						read::<libc::in_pktinfo>(data).map(|info| IpAddr::from(info.ipi_addr.s_addr.to_ne_bytes()));
				}
				(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO) => {
					meta.dst_ip = read::<libc::in6_pktinfo>(data).map(|info| IpAddr::from(info.ipi6_addr.s6_addr));
				}
				_ => {}
			}
			// SAFETY: CMSG_SPACE is a pure size computation.
			let aligned = unsafe { libc::CMSG_SPACE((message_len - header_len) as _) as usize };
			offset = offset.saturating_add(aligned.max(header_len));
		}

		meta
	}
}

/// A control message's payload as the int the kernel wrote, if it is one.
fn read_int(data: &[u8]) -> Option<libc::c_int> {
	read(data)
}

/// A control message's payload as the plain C struct `T`, if it holds one.
fn read<T: Copy>(data: &[u8]) -> Option<T> {
	if data.len() < std::mem::size_of::<T>() {
		return None;
	}
	// SAFETY: length-checked unaligned read of a plain C type the kernel wrote.
	Some(unsafe { data.as_ptr().cast::<T>().read_unaligned() })
}

/// Turn a boolean socket option on.
fn set_option(io: &UdpSocket, level: libc::c_int, name: libc::c_int) -> io::Result<()> {
	set_value(io, level, name, 1)
}

/// Set an integer socket option.
fn set_value(io: &UdpSocket, level: libc::c_int, name: libc::c_int, value: libc::c_int) -> io::Result<()> {
	// SAFETY: valid fd, valid option buffer.
	let ret = unsafe {
		libc::setsockopt(
			io.as_raw_fd(),
			level,
			name,
			(&raw const value).cast(),
			std::mem::size_of::<libc::c_int>() as libc::socklen_t,
		)
	};
	match ret {
		0 => Ok(()),
		_ => Err(io::Error::last_os_error()),
	}
}

/// Write `addr` into `out`, returning the length the kernel wants.
fn encode_addr(addr: SocketAddr, out: &mut libc::sockaddr_storage) -> libc::socklen_t {
	match addr {
		SocketAddr::V4(v4) => {
			let sin = libc::sockaddr_in {
				sin_family: libc::AF_INET as libc::sa_family_t,
				sin_port: v4.port().to_be(),
				sin_addr: libc::in_addr {
					s_addr: u32::from_ne_bytes(v4.ip().octets()),
				},
				sin_zero: [0; 8],
			};
			// SAFETY: sockaddr_in fits in sockaddr_storage.
			unsafe { (&raw mut *out).cast::<libc::sockaddr_in>().write(sin) };
			std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t
		}
		SocketAddr::V6(v6) => {
			let sin6 = libc::sockaddr_in6 {
				sin6_family: libc::AF_INET6 as libc::sa_family_t,
				sin6_port: v6.port().to_be(),
				sin6_flowinfo: v6.flowinfo(),
				sin6_addr: libc::in6_addr {
					s6_addr: v6.ip().octets(),
				},
				sin6_scope_id: v6.scope_id(),
			};
			// SAFETY: sockaddr_in6 fits in sockaddr_storage.
			unsafe { (&raw mut *out).cast::<libc::sockaddr_in6>().write(sin6) };
			std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t
		}
	}
}

/// Parse a kernel-written socket address.
fn decode_addr(name: &[u8]) -> Option<SocketAddr> {
	if name.len() < std::mem::size_of::<libc::sa_family_t>() {
		return None;
	}
	const FAMILY_LEN: usize = std::mem::size_of::<libc::sa_family_t>();
	let mut family = [0u8; FAMILY_LEN];
	family.copy_from_slice(&name[..FAMILY_LEN]);
	match libc::sa_family_t::from_ne_bytes(family) as libc::c_int {
		libc::AF_INET if name.len() >= std::mem::size_of::<libc::sockaddr_in>() => {
			// SAFETY: length-checked unaligned read.
			let sin = unsafe { name.as_ptr().cast::<libc::sockaddr_in>().read_unaligned() };
			Some(SocketAddr::from((
				sin.sin_addr.s_addr.to_ne_bytes(),
				u16::from_be(sin.sin_port),
			)))
		}
		libc::AF_INET6 if name.len() >= std::mem::size_of::<libc::sockaddr_in6>() => {
			// SAFETY: length-checked unaligned read.
			let sin6 = unsafe { name.as_ptr().cast::<libc::sockaddr_in6>().read_unaligned() };
			// Keep the scope id: link-local replies are unroutable without it.
			Some(SocketAddr::V6(SocketAddrV6::new(
				sin6.sin6_addr.s6_addr.into(),
				u16::from_be(sin6.sin6_port),
				sin6.sin6_flowinfo,
				sin6.sin6_scope_id,
			)))
		}
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4};

	/// One `UDP_GRO` control message carrying `size`.
	fn gro(size: libc::c_int) -> Vec<u8> {
		let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as _) } as usize;
		let mut control = vec![0u8; space];
		let header = libc::cmsghdr {
			cmsg_len: unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as _) } as _,
			cmsg_level: libc::SOL_UDP,
			cmsg_type: libc::UDP_GRO,
		};
		let header_len = unsafe { libc::CMSG_LEN(0) } as usize;
		// SAFETY: `control` holds a header plus an int.
		unsafe {
			control.as_mut_ptr().cast::<libc::cmsghdr>().write_unaligned(header);
			control
				.as_mut_ptr()
				.add(header_len)
				.cast::<libc::c_int>()
				.write_unaligned(size);
		}
		control
	}

	/// One control message of `level`/`kind` carrying `value`.
	fn cmsg<T>(level: libc::c_int, kind: libc::c_int, value: T) -> Vec<u8> {
		let mut control = vec![0u8; cmsg_space(std::mem::size_of::<T>())];
		let header = libc::cmsghdr {
			cmsg_len: unsafe { libc::CMSG_LEN(std::mem::size_of::<T>() as _) } as _,
			cmsg_level: level,
			cmsg_type: kind,
		};
		let header_len = unsafe { libc::CMSG_LEN(0) } as usize;
		// SAFETY: `control` holds a header plus a `T`.
		unsafe {
			control.as_mut_ptr().cast::<libc::cmsghdr>().write_unaligned(header);
			control.as_mut_ptr().add(header_len).cast::<T>().write_unaligned(value);
		}
		control
	}

	/// The most a receive carries for each family, including a v4-mapped
	/// datagram on a v6 socket, fits the buffer and parses whole.
	#[test]
	fn the_full_receive_control_set_fits_and_parses() {
		let v4 = Ipv4Addr::new(127, 0, 0, 2);
		let v6 = Ipv6Addr::new(0xfd6d, 0x6f71, 0, 0, 0, 0, 0, 2);
		let in_pktinfo = libc::in_pktinfo {
			ipi_ifindex: 1,
			// The route's preferred source, which must not be read as the destination.
			ipi_spec_dst: libc::in_addr {
				s_addr: u32::from_ne_bytes([127, 0, 0, 1]),
			},
			ipi_addr: libc::in_addr {
				s_addr: u32::from_ne_bytes(v4.octets()),
			},
		};
		let in6_pktinfo = |ip: Ipv6Addr| libc::in6_pktinfo {
			ipi6_addr: libc::in6_addr { s6_addr: ip.octets() },
			ipi6_ifindex: 1,
		};
		let ect0 = Ecn::Ect0 as u8;
		let cases: [(Vec<Vec<u8>>, IpAddr); 3] = [
			(
				vec![
					gro(1200),
					cmsg(libc::IPPROTO_IP, libc::IP_PKTINFO, in_pktinfo),
					cmsg(libc::IPPROTO_IP, libc::IP_TOS, ect0),
				],
				v4.into(),
			),
			(
				vec![
					gro(1200),
					cmsg(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO, in6_pktinfo(v6)),
					cmsg(libc::IPPROTO_IPV6, libc::IPV6_TCLASS, libc::c_int::from(ect0)),
				],
				v6.into(),
			),
			(
				vec![
					gro(1200),
					cmsg(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO, in6_pktinfo(v4.to_ipv6_mapped())),
					cmsg(libc::IPPROTO_IP, libc::IP_TOS, ect0),
				],
				v4.to_ipv6_mapped().into(),
			),
		];
		for (messages, dst_ip) in cases {
			let control = messages.concat();
			assert!(control.len() <= CONTROL_LEN, "{} > {CONTROL_LEN}", control.len());
			let meta = RecvMeta::parse(&control);
			assert_eq!(meta.stride, Some(1200));
			assert_eq!(meta.ecn, Some(Ecn::Ect0));
			assert_eq!(meta.dst_ip, Some(dst_ip));
		}
	}

	#[test]
	fn a_zero_gro_size_is_no_stride() {
		assert_eq!(RecvMeta::parse(&gro(1200)).stride, Some(1200));
		assert_eq!(RecvMeta::parse(&gro(0)).stride, None);
	}

	/// A malformed length after a valid message ends the walk with that message.
	#[test]
	fn malformed_control_lengths_preserve_valid_prefix() {
		let prefix = gro(1200);
		let suffix = gro(900);
		let header_len = unsafe { libc::CMSG_LEN(0) } as usize;
		for length in [0, header_len - 1, suffix.len() + 1, usize::MAX] {
			let mut control = [prefix.as_slice(), suffix.as_slice()].concat();
			// SAFETY: the suffix contains a complete header, written without assuming alignment.
			unsafe {
				let pointer = control.as_mut_ptr().add(prefix.len()).cast::<libc::cmsghdr>();
				let mut header = pointer.read_unaligned();
				header.cmsg_len = length;
				pointer.write_unaligned(header);
			}
			assert_eq!(RecvMeta::parse(&control).stride, Some(1200));
		}
	}

	/// Round-trip an address through the kernel wire encoding.
	fn roundtrip(addr: SocketAddr) -> Option<SocketAddr> {
		// SAFETY: all-zero is a valid sockaddr_storage.
		let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
		let len = encode_addr(addr, &mut storage) as usize;
		// SAFETY: encode_addr wrote `len` bytes into `storage`.
		let name = unsafe { std::slice::from_raw_parts((&raw const storage).cast::<u8>(), len) };
		decode_addr(name)
	}

	#[test]
	fn addr_roundtrip_v4() {
		let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 7), 4443));
		assert_eq!(roundtrip(addr), Some(addr));
	}

	#[test]
	fn addr_roundtrip_v6_keeps_scope_and_flow() {
		let ip = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);
		let addr = SocketAddr::V6(SocketAddrV6::new(ip, 4443, 0x12345, 3));
		assert_eq!(roundtrip(addr), Some(addr));
	}

	/// A receive pool with nothing allocated yet and a ring of its own.
	fn empty_rx() -> Rx {
		Rx {
			bufs: Vec::new(),
			// Never registered, so this one is ours alone to publish into.
			ring: Some(BufRing::new(64, 0)),
			// SAFETY: all-zero is valid for `msghdr`, and nothing reads it here.
			hdr: Box::new(unsafe { std::mem::zeroed() }),
			queue: VecDeque::new(),
			waiters: kio::WaiterList::new(),
			armed: None,
			starved: false,
			error: None,
			unreachable: None,
		}
	}

	/// A recorded `ENOBUFS` outlives the buffer that recycled after it: the
	/// kernel ran the pool dry, so the pool is too shallow however full the
	/// ring looks by the time the re-arm gets to it. Growing off the ring's
	/// state alone leaves a bursting socket re-arming at its floor forever.
	#[test]
	fn a_recycled_buffer_does_not_mask_a_recorded_starvation() {
		let config = Config::default();
		let mut rx = empty_rx();
		grow_rx(&mut rx, &config);
		assert!(!should_grow(&rx, true), "a buffer is in the ring");

		rx.starved = true;
		assert!(should_grow(&rx, true), "the kernel ran dry, recycle or not");
		assert!(should_grow(&rx, false), "and the oneshot path reads it too");
	}

	/// A starved receive pool doubles into its ceiling, offering every new
	/// buffer to the kernel as it goes.
	#[test]
	fn the_receive_pool_doubles_to_its_ceiling() {
		let config = Config {
			rx_buffers_max: 40,
			..Default::default()
		};
		let mut rx = empty_rx();

		for expected in [1u16, 2, 4, 8, 16, 32, 40] {
			assert!(grow_rx(&mut rx, &config), "growth stopped short of {expected}");
			assert_eq!(rx.bufs.len(), usize::from(expected));
			// Every buffer reaches the kernel exactly once.
			assert_eq!(rx.ring.as_ref().expect("ring").tail, expected);
		}
		assert!(!grow_rx(&mut rx, &config), "grew past the ceiling");
	}

	/// Every errno is either loss, a destination, the socket, or teardown, and only the errors
	/// meaning the socket itself is gone fail it.
	#[test]
	fn send_errors_fail_the_socket_only_when_it_is_gone() {
		for code in [libc::EBADF, libc::ENOTSOCK, libc::EFAULT] {
			assert_eq!(SendFailure::of(code), SendFailure::Socket, "{code}");
		}
		for code in [libc::ENETUNREACH, libc::EHOSTUNREACH, libc::ENETDOWN] {
			assert_eq!(SendFailure::of(code), SendFailure::Unreachable, "{code}");
		}
		for code in [
			libc::EPERM,
			libc::EACCES,
			libc::ENOMEM,
			libc::ENOBUFS,
			libc::EMSGSIZE,
			libc::ECONNREFUSED,
			libc::EAGAIN,
			libc::EINVAL,
			libc::EIO,
		] {
			assert_eq!(SendFailure::of(code), SendFailure::Lost, "{code}");
		}
		assert_eq!(SendFailure::of(libc::ECANCELED), SendFailure::Cancelled);
	}

	/// A send that completes with a recoverable error, or short, costs that packet: the socket
	/// it shares with every other connection on the worker keeps sending. An error that means
	/// the socket is gone still fails it.
	#[test]
	fn a_failed_send_costs_only_its_packet() {
		let mut worker = match crate::Worker::new(crate::Config::default()) {
			Ok(worker) => worker,
			Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
				eprintln!("skipping io_uring socket test: {reason}");
				return;
			}
			Err(err) => panic!("worker setup failed: {err}"),
		};
		let handle = worker.handle();
		let peer = std::net::UdpSocket::bind("127.0.0.1:0").expect("peer");
		peer.set_read_timeout(Some(std::time::Duration::from_secs(10)))
			.expect("timeout");
		let to = peer.local_addr().expect("peer addr");
		let io = UdpSocket::bind("127.0.0.1:0").expect("bind");
		let sock = handle.udp(io, Config::default()).expect("socket");

		let lost = [
			libc::EPERM,
			libc::EACCES,
			libc::ENOMEM,
			libc::ENOBUFS,
			libc::EMSGSIZE,
			libc::EINVAL,
		];
		let err = worker
			.block_on(async {
				for code in lost {
					let mut tx = sock.acquire().await.expect("a lost send keeps the socket");
					tx.extend_from_slice(b"lost");
					on_send(
						tx.fake_send(to),
						Cqe {
							user_data: 0,
							result: -code,
							flags: 0,
						},
					);
				}
				let mut tx = sock.acquire().await.expect("a short send keeps the socket");
				tx.extend_from_slice(b"short");
				on_send(
					tx.fake_send(to),
					Cqe {
						user_data: 0,
						result: 2,
						flags: 0,
					},
				);

				// Still sending: a real datagram reaches the peer.
				let mut tx = sock.acquire().await.expect("send buffer");
				tx.extend_from_slice(b"alive");
				tx.send(Transmit::new(to, 5)).expect("staged");
				// Let the worker submit it and reap its completion.
				for _ in 0..1000 {
					if sock.shared.tx.borrow().bufs.iter().all(|buf| buf.in_flight == 0) {
						break;
					}
					crate::Timer::after(&handle, std::time::Duration::from_millis(1))
						.wait()
						.await;
				}

				let mut tx = sock.acquire().await.expect("send buffer");
				tx.extend_from_slice(b"gone");
				on_send(
					tx.fake_send(to),
					Cqe {
						user_data: 0,
						result: -libc::EBADF,
						flags: 0,
					},
				);
				sock.acquire().await.expect_err("a socket-gone error fails the socket")
			})
			.expect("worker");
		assert_eq!(err.raw_os_error(), Some(libc::EBADF), "{err}");

		let mut buf = [0u8; 16];
		let (len, _) = peer.recv_from(&mut buf).expect("the healthy send arrived");
		assert_eq!(&buf[..len], b"alive");
	}

	/// A source address that is no longer local fails a GSO train with `EINVAL`, like a route
	/// that cannot segment, but its unsegmented resend fails too. That costs only its packets:
	/// segmentation stays on for every other connection sharing the socket.
	#[test]
	fn a_stale_source_keeps_segmentation() {
		let mut worker = match crate::Worker::new(crate::Config::default()) {
			Ok(worker) => worker,
			Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
				eprintln!("skipping io_uring socket test: {reason}");
				return;
			}
			Err(err) => panic!("worker setup failed: {err}"),
		};
		let handle = worker.handle();
		let peer = std::net::UdpSocket::bind("[::1]:0").expect("peer");
		peer.set_read_timeout(Some(std::time::Duration::from_secs(10)))
			.expect("timeout");
		let to = peer.local_addr().expect("peer addr");
		let io = UdpSocket::bind("[::]:0").expect("bind");
		let sock = handle.udp(io, Config::default()).expect("socket");
		assert!(sock.shared.gso.get(), "the test needs GSO to start on");

		worker
			.block_on(async {
				// Wait for every staged send to complete.
				let settle = || async {
					for _ in 0..1000 {
						if sock.shared.tx.borrow().bufs.iter().all(|buf| buf.in_flight == 0) {
							break;
						}
						crate::Timer::after(&handle, std::time::Duration::from_millis(1))
							.wait()
							.await;
					}
				};

				let mut tx = sock.acquire().await.expect("send buffer");
				tx.resize(2 * 1200, 1);
				let mut stale = Transmit::new(to, 1200);
				// A documentation address, never assigned to this host.
				stale.src_ip = Some(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1).into());
				tx.send(stale).expect("staged");
				settle().await;
				assert!(sock.shared.gso.get(), "a stale source halted segmentation");

				let mut tx = sock.acquire().await.expect("a stale source keeps the socket");
				tx.resize(2 * 1200, 2);
				tx.send(Transmit::new(to, 1200)).expect("staged");
				settle().await;
			})
			.expect("worker");
		let mut buf = [0u8; 2048];
		for _ in 0..2 {
			let (len, _) = peer.recv_from(&mut buf).expect("the healthy train arrived");
			assert_eq!(&buf[..len], &[2; 1200][..]);
		}
		// The stale train, its two resends, then the healthy train as one train.
		assert_eq!(handle.metrics().snapshot().tx_sends, 4);
	}

	/// With nothing routing unreachable reports, a send that finds no route
	/// fails later sends instead of queueing a report nobody will collect.
	/// Linux refuses a v4-mapped destination on an IPv6-only socket with
	/// `ENETUNREACH`.
	#[test]
	fn a_raw_socket_sees_an_unreachable_send_fail() {
		use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrIn6, bind, setsockopt, socket, sockopt};

		let mut worker = match crate::Worker::new(crate::Config::default()) {
			Ok(worker) => worker,
			Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
				eprintln!("skipping io_uring socket test: {reason}");
				return;
			}
			Err(err) => panic!("worker setup failed: {err}"),
		};
		let handle = worker.handle();
		let fd = socket(AddressFamily::Inet6, SockType::Datagram, SockFlag::empty(), None).expect("socket");
		setsockopt(&fd, sockopt::Ipv6V6Only, &true).expect("IPV6_V6ONLY");
		let local: SocketAddrV6 = "[::1]:0".parse().expect("addr");
		bind(fd.as_raw_fd(), &SockaddrIn6::from(local)).expect("bind");
		let sock = handle.udp(UdpSocket::from(fd), Config::default()).expect("socket");

		let err = worker
			.block_on(async {
				let mut tx = sock.acquire().await.expect("send buffer");
				tx.extend_from_slice(b"lost");
				tx.send(Transmit::new(
					SocketAddr::new(Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), 1),
					4,
				))
				.expect("staged");
				// The failure lands with the send's completion, a worker turn later.
				for _ in 0..1000 {
					match sock.acquire().await {
						Ok(tx) => drop(tx),
						Err(err) => return err,
					}
					crate::Timer::after(&handle, std::time::Duration::from_millis(1))
						.wait()
						.await;
				}
				panic!("the unreachable send never failed the socket");
			})
			.expect("worker");
		assert_eq!(err.kind(), io::ErrorKind::NetworkUnreachable, "{err}");
		assert!(sock.shared.rx.borrow().unreachable.is_none(), "nothing queued");
	}

	/// A worker, or `None` (outside strict CI) on a kernel that cannot run one.
	fn worker() -> Option<crate::Worker> {
		match crate::Worker::new(crate::Config::default()) {
			Ok(worker) => Some(worker),
			Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
				eprintln!("skipping io_uring socket test: {reason}");
				None
			}
			Err(err) => panic!("worker setup failed: {err}"),
		}
	}

	fn bind() -> UdpSocket {
		UdpSocket::bind("127.0.0.1:0").expect("bind")
	}

	fn oneshot() -> Config {
		Config {
			multishot: false,
			..Default::default()
		}
	}

	/// The worker's shared core, reached through a socket that takes no
	/// buffer group id.
	fn core(handle: &crate::Handle) -> (Socket, Rc<Shared>) {
		let sock = handle.udp(bind(), oneshot()).expect("oneshot socket");
		let shared = sock.owner().upgrade().expect("worker");
		(sock, shared)
	}

	/// Drive `worker` until every kernel operation has let go of `sock`.
	fn release(worker: &mut crate::Worker, sock: std::rc::Weak<SockShared>) {
		worker
			.block_on(std::future::poll_fn(|cx| {
				if sock.upgrade().is_none() {
					return Poll::Ready(());
				}
				cx.waker().wake_by_ref();
				Poll::Pending
			}))
			.expect("worker");
	}

	/// Fill the submission queue, so the next push has to enter the ring.
	fn fill(shared: &Shared) {
		while !shared.ring.borrow_mut().submission().is_full() {
			let key = shared.insert(Op::Cancel);
			shared
				.push(&opcode::Nop::new().build().user_data(key))
				.expect("stage NOP");
		}
	}

	/// Only multishot sockets register a buffer ring, so only they take an id.
	#[test]
	fn a_oneshot_socket_takes_no_buffer_group_id() {
		let Some(worker) = worker() else { return };
		let (_sock, shared) = core(&worker.handle());
		let _more = worker.handle().udp(bind(), oneshot()).expect("oneshot socket");
		assert_eq!(shared.bgids.borrow().next, 0);
	}

	/// Churn past the id space reuses the id the kernel released, and a socket
	/// beyond every live id fails to open instead of panicking.
	#[test]
	fn socket_churn_reuses_buffer_group_ids() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let (_core, shared) = core(&handle);
		// Every id below the last names a live ring.
		shared.bgids.borrow_mut().next = u32::from(u16::MAX);

		for _ in 0..3 {
			let sock = handle.udp(bind(), Config::default()).expect("multishot socket");
			assert!(
				shared.bgids.borrow().free.is_empty(),
				"{:?}",
				shared.bgids.borrow().free
			);
			let err = handle.udp(bind(), Config::default()).expect_err("every id is live");
			assert!(err.to_string().contains("buffer group id"), "{err}");

			let weak = sock.downgrade();
			drop(sock);
			release(&mut worker, weak);
			assert_eq!(shared.bgids.borrow().free, [u16::MAX]);
			assert_eq!(shared.bgids.borrow().next, u32::from(u16::MAX) + 1);
		}
	}

	/// A registration the kernel refuses hands its id straight back.
	#[test]
	fn a_failed_registration_returns_its_buffer_group_id() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		let (_core, shared) = core(&handle);

		// Hold id 0 outside the pool, so the socket's registration collides.
		let held = BufRing::new(1, 0);
		// SAFETY: `held` outlives its registration, which ends below.
		unsafe {
			shared
				.ring
				.borrow()
				.submitter()
				.register_buf_ring_with_flags(held.ptr.as_ptr() as u64, 1, 0, 0)
				.expect("register");
		}
		let err = handle.udp(bind(), Config::default()).expect_err("id 0 is taken");
		assert!(
			matches!(&err, Error::Io(err) if err.raw_os_error() == Some(libc::EEXIST)),
			"{err}"
		);
		assert_eq!(shared.bgids.borrow().free, [0]);

		shared
			.ring
			.borrow()
			.submitter()
			.unregister_buf_ring(0)
			.expect("unregister");
		drop(held);
		let _sock = handle.udp(bind(), Config::default()).expect("id 0 is free");
		assert!(shared.bgids.borrow().free.is_empty());
		assert_eq!(shared.bgids.borrow().next, 1);
	}

	/// A dropped socket that cannot stage its cancel fails the worker's next
	/// turn, rather than leaving the receive armed in silence.
	#[test]
	fn a_failed_cancel_fails_the_next_turn() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let (_core, shared) = core(&handle);
		let sock = handle.udp(bind(), Config::default()).expect("multishot socket");
		let weak = sock.downgrade();

		fill(&shared);
		shared.enter_faults.borrow_mut().push_back(libc::EBADF);
		drop(sock);
		assert!(weak.upgrade().is_some(), "the receive is still armed");

		// Even a future that is ready on its first poll cannot return past it.
		let err = worker
			.block_on(std::future::ready(()))
			.expect_err("the cancel failure surfaces");
		assert!(
			matches!(&err, Error::Io(err) if err.raw_os_error() == Some(libc::EBADF)),
			"{err}"
		);
		// Reported once, like any other ring failure.
		assert!(shared.failed.take().is_none());

		// Teardown still cancels the receive.
		drop(worker);
		assert!(weak.upgrade().is_none(), "teardown leaked the receive");
	}

	/// An enter interrupted by a signal is retried, not surfaced.
	#[test]
	fn an_interrupted_enter_is_retried() {
		let Some(worker) = worker() else { return };
		let (_core, shared) = core(&worker.handle());
		fill(&shared);
		shared.enter_faults.borrow_mut().push_back(libc::EINTR);

		let key = shared.insert(Op::Cancel);
		shared
			.push(&opcode::Nop::new().build().user_data(key))
			.expect("an interrupted enter retries");
		assert!(shared.enter_faults.borrow().is_empty());
		assert_eq!(shared.ring.borrow_mut().submission().len(), 1);
	}
}
