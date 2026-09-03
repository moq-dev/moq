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
//! message explicitly; the socket default is never relied on.
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
use std::net::{SocketAddr, SocketAddrV6, UdpSocket};
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU16, Ordering};
use std::task::Poll;

use io_uring::{cqueue, opcode, types};

use crate::Error;
use crate::shared::{Cqe, Op, Shared};

/// Space reserved for received control messages (`UDP_GRO` needs one int).
const CONTROL_LEN: usize = 64;
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
	/// datagram.
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
	stride: usize,
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
/// The `Box<[u8]>` allocations are stable across the `Vec` growth, which is
/// what lets a live [`TxBuf`] keep a raw pointer into one.
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
	ptr: NonNull<types::BufRingEntry>,
	layout: Layout,
	mask: u16,
	tail: u16,
}

impl BufRing {
	fn new(entries: u16) -> Self {
		let layout = Layout::from_size_align(entries as usize * std::mem::size_of::<types::BufRingEntry>(), 4096)
			.expect("buffer ring layout");
		// SAFETY: layout is non-zero.
		let ptr = unsafe { alloc_zeroed(layout) };
		let ptr = NonNull::new(ptr.cast::<types::BufRingEntry>()).unwrap_or_else(|| handle_alloc_error(layout));
		Self {
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
}

/// Send-side state.
struct Tx {
	bufs: Vec<TxSlot>,
	free: Vec<u16>,
	waiters: kio::WaiterList,
	/// Terminal failure, surfaced by `poll_acquire`.
	error: Option<i32>,
}

/// Stable storage reused by every checkout of one transmit slot: the payload
/// the kernel reads and the `sendmsg` headers pointing into it.
struct TxSlot {
	data: Box<[u8]>,
	headers: Vec<SendHdr>,
	/// Sends staged from this slot that the kernel has not completed. The slot
	/// returns to the free list when it hits zero.
	in_flight: usize,
}

impl TxSlot {
	fn new(len: usize) -> Self {
		Self {
			data: vec![0u8; len].into_boxed_slice(),
			headers: Vec::new(),
			in_flight: 0,
		}
	}
}

/// Everything both the [`Socket`] handle and in-flight ops keep alive.
pub(crate) struct SockShared {
	io: UdpSocket,
	worker: Weak<Shared>,
	config: Config,
	bgid: u16,
	closed: Cell<bool>,
	rx: RefCell<Rx>,
	tx: RefCell<Tx>,
}

impl SockShared {
	/// Whether the worker loop that would drive this socket is gone: dropped
	/// outright, or torn down while handles keep the shared state alive.
	fn worker_gone(&self) -> bool {
		match self.worker.upgrade() {
			Some(shared) => shared.stopped.get(),
			None => true,
		}
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
			if let Some(shared) = self.worker.upgrade() {
				arm_recv(&shared, self);
			}
		}
	}

	fn release_tx(&self, id: u16) {
		let mut tx = self.tx.borrow_mut();
		debug_assert_eq!(tx.bufs[id as usize].in_flight, 0);
		tx.free.push(id);
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
			tx.waiters.wake();
		}
	}

	fn fail_rx(&self, code: i32) {
		let mut rx = self.rx.borrow_mut();
		rx.error.get_or_insert(code);
		rx.waiters.wake();
	}

	fn fail_tx(&self, code: i32) {
		let mut tx = self.tx.borrow_mut();
		tx.error.get_or_insert(code);
		tx.waiters.wake();
	}
}

impl Drop for SockShared {
	fn drop(&mut self) {
		// Every op referencing our buffers has completed (ops own an `Rc` of
		// us), so the kernel is done; give the buffer group id back.
		if self.rx.borrow().ring.is_some()
			&& let Some(shared) = self.worker.upgrade()
		{
			let ring = shared.ring.borrow_mut();
			let _ = ring.submitter().unregister_buf_ring(self.bgid);
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
	pub(crate) fn downgrade(&self) -> Weak<SockShared> {
		Rc::downgrade(&self.shared)
	}

	pub(crate) fn bind(shared: &Rc<Shared>, io: UdpSocket, config: Config) -> Result<Self, Error> {
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
			let on: libc::c_int = 1;
			// SAFETY: valid fd, valid option buffer.
			let ret = unsafe {
				libc::setsockopt(
					io.as_raw_fd(),
					libc::SOL_UDP,
					libc::UDP_GRO,
					(&raw const on).cast(),
					std::mem::size_of::<libc::c_int>() as libc::socklen_t,
				)
			};
			if ret != 0 {
				return Err(io::Error::last_os_error().into());
			}
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

		let bgid = shared.next_bgid.get();
		shared
			.next_bgid
			.set(bgid.checked_add(1).expect("buffer group ids exhausted"));

		let ring = if config.multishot {
			let mut ring = BufRing::new(rx_cap);
			{
				let io_ring = shared.ring.borrow_mut();
				// SAFETY: the ring allocation lives in `SockShared`, which the
				// armed receive's `Op` keeps alive until its terminal CQE, and
				// is unregistered before it drops.
				unsafe {
					io_ring
						.submitter()
						.register_buf_ring_with_flags(ring.ptr.as_ptr() as u64, rx_cap, bgid, 0)?;
				}
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
			error: None,
		};

		let sock = Rc::new(SockShared {
			io,
			worker: Rc::downgrade(shared),
			config,
			bgid,
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
			let slot = &mut tx.bufs[id as usize];
			// SAFETY: `id` was exclusively checked out of the free list; the
			// allocation is stable (see TxBuf).
			let ptr = unsafe { NonNull::new_unchecked(slot.data.as_mut_ptr()) };
			let cap = slot.data.len();
			return Poll::Ready(Ok(TxBuf {
				sock: self.shared.clone(),
				id,
				ptr,
				cap,
				armed: false,
			}));
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
		if let (Some(key), Some(shared)) = (rx.armed, self.shared.worker.upgrade()) {
			drop(rx);
			// Fire-and-forget: the cancel's own CQE is consumed by the worker,
			// and the receive's terminal CQE releases the socket state.
			let _ = shared.cancel(key);
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
}

impl Packet {
	/// The datagrams' source address.
	pub fn from(&self) -> SocketAddr {
		self.from
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

	/// The whole coalesced payload, mutably (QUIC decrypts in place).
	pub fn payload_mut(&mut self) -> &mut [u8] {
		// SAFETY: as `payload`, and `&mut self` forbids aliasing our slices.
		unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
	}

	/// The individual datagrams.
	pub fn segments(&mut self) -> impl Iterator<Item = &mut [u8]> {
		let stride = self.stride;
		self.payload_mut().chunks_mut(stride)
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
			.field("len", &self.len)
			.field("stride", &self.stride)
			.finish()
	}
}

/// A checked-out send-staging buffer: fill it, then [`send`](Self::send) it.
///
/// The buffer belongs to the socket it was acquired from, and sending goes
/// back through that socket. Dropping it unsent returns it to the pool.
pub struct TxBuf {
	sock: Rc<SockShared>,
	id: u16,
	ptr: NonNull<u8>,
	cap: usize,
	armed: bool,
}

impl TxBuf {
	/// Send `self[..len]` on the owning socket, to `to`, as datagrams of
	/// `segment` bytes (the last may be short). Fire-and-forget: the buffer
	/// returns to the pool when the kernel completes, and a failed send
	/// surfaces on the next pool acquire.
	///
	/// This only stages an SQE, so the datagram reaches the kernel when the
	/// worker next enters the ring. Dropping the worker makes a bounded attempt
	/// to submit staged datagrams and drain their completions, but does not
	/// guarantee kernel completion or delivery.
	pub fn send(mut self, len: usize, to: SocketAddr, segment: usize) -> io::Result<()> {
		// `UDP_SEGMENT` is a u16, so an oversized segment would silently
		// truncate into a tiny stride and explode the implied segment count.
		if len == 0 || len > self.cap || segment == 0 || segment > usize::from(u16::MAX) {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"invalid send: {len} bytes in {segment} byte segments from a {} byte buffer",
					self.cap
				),
			));
		}
		let shared = match self.sock.worker.upgrade() {
			Some(shared) if !shared.stopped.get() => shared,
			_ => return Err(Shared::gone_error()),
		};

		// A GSO train is one `sendmsg` the kernel caps at 64 segments. Without
		// GSO every segment is its own `sendmsg`, so the ring is the limit
		// instead: staging more than the submission queue holds makes `push`
		// submit inline and go round again without reaping a single
		// completion, which starves the worker and overflows the queue.
		let segments = len.div_ceil(segment);
		let limit = match self.sock.config.gso {
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
		let base = self.ptr.as_ptr();
		let headers = {
			let mut tx = sock.tx.borrow_mut();
			let headers = &mut tx.bufs[self.id as usize].headers;
			if headers.len() < segments {
				headers.resize_with(segments, SendHdr::zeroed);
			}
			// SAFETY: the slot is checked out, so it cannot be sent from again
			// (and its headers cannot grow again) until every send below
			// completes and returns it to the free list.
			unsafe { NonNull::new_unchecked(headers.as_mut_ptr()) }
		};
		let staging = Staging {
			sock: sock.clone(),
			id: self.id,
			headers,
		};

		if sock.config.gso {
			send_one(&shared, &staging, 0, base, len, to, Some(segment as u16))?;
		} else {
			for index in 0..segments {
				let offset = index * segment;
				let chunk = segment.min(len - offset);
				// SAFETY: offset stays within the leased buffer.
				send_one(&shared, &staging, index, unsafe { base.add(offset) }, chunk, to, None)?;
			}
		}
		Ok(())
	}
}

impl std::ops::Deref for TxBuf {
	type Target = [u8];

	fn deref(&self) -> &[u8] {
		// SAFETY: `id` is exclusively ours until release; stable allocation.
		unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.cap) }
	}
}

impl std::ops::DerefMut for TxBuf {
	fn deref_mut(&mut self) -> &mut [u8] {
		// SAFETY: as `deref`.
		unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.cap) }
	}
}

impl Drop for TxBuf {
	fn drop(&mut self) {
		if !self.armed {
			self.sock.release_tx(self.id);
		}
	}
}

impl std::fmt::Debug for TxBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("TxBuf").field("cap", &self.cap).finish()
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
}

impl Drop for SendOp {
	fn drop(&mut self) {
		self.sock.complete_tx(self.id);
	}
}

fn send_one(
	shared: &Rc<Shared>,
	staging: &Staging,
	index: usize,
	base: *mut u8,
	len: usize,
	to: SocketAddr,
	segment: Option<u16>,
) -> io::Result<()> {
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

	if let Some(segment) = segment {
		hdr.hdr.msg_control = hdr.control.0.as_mut_ptr().cast();
		// SAFETY: the control buffer is zeroed, aligned, and large enough for
		// one u16 control message.
		unsafe {
			hdr.hdr.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<u16>() as _) as usize;
			let cmsg = libc::CMSG_FIRSTHDR(&hdr.hdr);
			(*cmsg).cmsg_level = libc::SOL_UDP;
			(*cmsg).cmsg_type = libc::UDP_SEGMENT;
			(*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<u16>() as _) as usize;
			std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<u16>(), segment);
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
	}));
	let entry = opcode::SendMsg::new(types::Fd(staging.sock.io.as_raw_fd()), hdr_ptr)
		.build()
		.user_data(key);
	if let Err(err) = shared.push(&entry) {
		shared.ops.borrow_mut().remove(key as usize);
		return Err(err);
	}
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
			return;
		}
		let key = shared.insert(Op::Recv {
			sock: sock.clone(),
			one: None,
		});
		rx.armed = Some(key);
		opcode::RecvMsgMulti::new(types::Fd(sock.io.as_raw_fd()), &*rx.hdr, sock.bgid)
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
			libc::ENOBUFS => sock.rx.borrow_mut().starved = true,
			// Socket teardown; nothing to surface.
			libc::ECANCELED => return,
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
	let stride = gro_stride(out.control_data()).unwrap_or(payload.len());
	let payload_start = payload.as_ptr() as usize - buf.data.as_ptr() as usize;
	Ok((
		bid,
		Some(Queued {
			bid,
			start: payload_start,
			len: payload.len(),
			from,
			stride,
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
	let stride = gro_stride(control).unwrap_or(len);
	// `claimed` stays set: the packet owns the buffer until released.
	Ok((
		bid,
		Some(Queued {
			bid,
			start: 0,
			len,
			from,
			stride,
		}),
	))
}

/// Handle one send completion; the buffer lease releases when the last
/// completion drops its `SendOp`.
pub(crate) fn on_send(op: SendOp, cqe: Cqe) {
	if cqe.result < 0 {
		let code = -cqe.result;
		if code == libc::ECONNREFUSED {
			// ICMP unreachable noise; QUIC treats it as loss.
			tracing::debug!("send completed with ECONNREFUSED");
			return;
		}
		if code != libc::ECANCELED {
			op.sock.fail_tx(code);
		}
	} else if cqe.result as usize != op.expect {
		tracing::warn!(sent = cqe.result, expected = op.expect, "short UDP send");
		op.sock.fail_tx(libc::EIO);
	}
}

/// The `UDP_GRO` segment size in a received control buffer, if present.
fn gro_stride(control: &[u8]) -> Option<usize> {
	let header_len = unsafe { libc::CMSG_LEN(0) as usize };
	let mut offset = 0;

	while offset + header_len <= control.len() {
		// SAFETY: bounds-checked read of a cmsghdr-sized prefix.
		let header = unsafe { control.as_ptr().add(offset).cast::<libc::cmsghdr>().read_unaligned() };
		let message_len = header.cmsg_len;
		if message_len < header_len || offset + message_len > control.len() {
			return None;
		}
		if header.cmsg_level == libc::SOL_UDP && header.cmsg_type == libc::UDP_GRO {
			if message_len < header_len + std::mem::size_of::<libc::c_int>() {
				return None;
			}
			// SAFETY: length-checked just above.
			let value = unsafe {
				control
					.as_ptr()
					.add(offset + header_len)
					.cast::<libc::c_int>()
					.read_unaligned()
			};
			return usize::try_from(value).ok();
		}
		// SAFETY: CMSG_SPACE is a pure size computation.
		let aligned = unsafe { libc::CMSG_SPACE((message_len - header_len) as _) as usize };
		offset = offset.saturating_add(aligned.max(header_len));
	}

	None
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
			ring: Some(BufRing::new(64)),
			// SAFETY: all-zero is valid for `msghdr`, and nothing reads it here.
			hdr: Box::new(unsafe { std::mem::zeroed() }),
			queue: VecDeque::new(),
			waiters: kio::WaiterList::new(),
			armed: None,
			starved: false,
			error: None,
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
}
