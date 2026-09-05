//! Forming and steering a `SO_REUSEPORT` group by QUIC connection ID.
//!
//! A reuseport group left to itself is picked by hashing the packet's 4-tuple,
//! which is wrong for QUIC: a connection is identified by its connection ID, not
//! its address, so a client that changes address (a NAT rebinding, a network
//! change, or plain connection migration) hashes to a different member and its
//! packets arrive at a socket that has never heard of it. The connection dies
//! instead of migrating.
//!
//! The fix is the standard one: the server chooses connection IDs that say which
//! member owns them, and a filter on the group reads that back.
//!
//! # The group
//!
//! [`Group`] is the whole entry point. It takes the port, fixes the member
//! count, and hands out one [`Member`] per slot in index order; binding a member
//! is the only way to join the group, and the last one to bind attaches the
//! steering filter. Hold the group for as long as its sockets are served.
//!
//! ```no_run
//! # fn example(addr: std::net::SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
//! let mut group = moq_sock::shard::Group::acquire(addr, 4)?;
//! while let Some(member) = group.member() {
//!     let shard = member.shard();
//!     let socket = member.bind()?;
//!     // Serve `socket`, issuing connection IDs led by `cid_prefix(shard)`.
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # The encoding
//!
//! [`cid_prefix`] makes the first byte of every connection ID this member issues
//! congruent to its index modulo the group size, keeping the remaining
//! `256 / count` values of that byte (and every later byte) random. The kernel
//! selects `socks[A % count]` for a returned accumulator `A`, so feeding it that
//! byte lands on the owner.
//!
//! A client's first packets carry a connection ID the *client* invented, which
//! encodes nothing. Those hash to an arbitrary member, and that is fine: whoever
//! receives the Initial owns the connection and issues IDs carrying its own
//! index, so every later packet steers to it. Retransmitted Initials repeat the
//! same client-chosen ID and so keep reaching the same member, which is what
//! stops a retry from starting a second handshake elsewhere.
//!
//! # Why classic BPF
//!
//! `SO_ATTACH_REUSEPORT_CBPF` runs the program with the UDP payload at offset 0
//! (the kernel pulls the header off first) and uses the return value as the
//! index. That is the whole rule in seven instructions, with no `CAP_BPF`, no
//! BPF toolchain in the build, and no map to keep alive across restarts. The
//! cost is that the kernel selects by *position*, so the group has to be built
//! once, in order, and never resized, which is what [`Group`] is for.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// One member's slot in a group of sockets sharing a port via `SO_REUSEPORT`.
///
/// Every member binds the same address and the kernel spreads inbound datagrams
/// across the group, so N servers on N threads can serve one port without a
/// shared socket between them. The kernel identifies a member by its *position*
/// in the group, which is why only [`Group::member`] mints one: a shard is
/// meaningful only inside a group that was bound once, in index order, and is
/// never resized.
///
/// Carry it wherever the member issues connection IDs, which [`cid_prefix`]
/// leads with its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shard {
	index: u16,
	count: u16,
}

impl Shard {
	/// Slot `index` of a group of `count` sockets, or `None` if that slot cannot
	/// exist: a `count` of zero or past [`MAX_SHARDS`], or an `index` at or past
	/// the end.
	///
	/// The upper bound is what makes every shard safe to steer: [`cid_prefix`]
	/// spends `256 / count` values of a byte, so a larger group would leave it
	/// none to spend.
	fn new(index: u16, count: u16) -> Option<Self> {
		(count <= MAX_SHARDS && index < count).then_some(Self { index, count })
	}

	/// This member's position in the group, from zero.
	pub fn index(self) -> u16 {
		self.index
	}

	/// How many sockets share the port.
	pub fn count(self) -> u16 {
		self.count
	}
}

/// Why a reuseport group could not be formed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// More members than the steering filter can address.
	#[error("a reuseport group holds at most {max} members; {count} were asked for")]
	Count {
		/// What was asked for.
		count: u16,
		/// The ceiling, set by the byte the steering filter reads.
		max: u16,
	},

	/// Another group of this UID already holds the port.
	///
	/// Over-exclusive on purpose: the lock is keyed by port alone, because every
	/// address spelling that can overlap on a port (`[::]`, `0.0.0.0`, and any
	/// specific address) must share one lock. Two groups on distinct addresses
	/// sharing a port are refused although they could coexist, which is a loud
	/// failure in place of silent traffic loss.
	#[error("another reuseport group already holds port {port}")]
	Overlap {
		/// The port both groups asked for.
		port: u16,
	},
}

/// A `SO_REUSEPORT` group being formed: the port it holds, how many members it
/// has, and the order they bind in.
///
/// Everything a steered group has to get right lives here rather than in its
/// caller, because none of it is visible in a socket afterwards:
///
/// - The port is locked before the first member binds and stays locked until the
///   group is dropped, so a second same-UID group cannot interleave into this
///   one. Hold the group for as long as its sockets are served.
/// - The member count is checked once, against what the steering filter can
///   address, and never changes.
/// - [`member`](Self::member) hands out slots in index order, and a [`Member`]
///   is the only thing that can bind into the group. The kernel numbers the
///   group by bind order, so a member that binds out of turn is refused rather
///   than silently taking a sibling's slot.
///
/// Members may be bound wherever their sockets are served, a worker's own thread
/// included, as long as each one binds before the next takes its turn.
#[derive(Debug)]
pub struct Group {
	count: u16,

	/// The next slot to hand out. Not the same as the next slot to *bind*: a
	/// member may be bound on a thread of its own, which is what [`State`]
	/// tracks.
	next: u16,

	state: Arc<State>,

	/// Held for the group's lifetime, and released by the kernel with the
	/// process. `None` for an ephemeral port, which cannot be named in advance,
	/// and on a host with no lock directory.
	_lock: Option<Lock>,
}

impl Group {
	/// Take the port and fix the group at `count` members.
	///
	/// A `count` of zero is a group of one: a lone member is a valid group, and
	/// the callers that size a group from a worker count would otherwise each
	/// clamp it themselves.
	///
	/// An ephemeral port (`0`) is bound by the first member and shared by the
	/// rest, so a group can take whatever port the kernel hands out. Nothing is
	/// locked in that case: no second group can be aiming at a port that cannot
	/// be named in advance.
	pub fn acquire(addr: SocketAddr, count: u16) -> Result<Self, Error> {
		let count = count.max(1);
		if count > MAX_SHARDS {
			return Err(Error::Count { count, max: MAX_SHARDS });
		}

		let lock = match addr.port() {
			0 => None,
			port => Lock::acquire(port).map_err(|_| Error::Overlap { port })?,
		};

		Ok(Self {
			count,
			next: 0,
			state: Arc::new(State::new(addr)),
			_lock: lock,
		})
	}

	/// How many sockets share the port.
	pub fn count(&self) -> u16 {
		self.count
	}

	/// The address the group holds: what was asked for until the first member
	/// binds, and what it actually bound from there on.
	pub fn addr(&self) -> SocketAddr {
		self.state.lock().addr
	}

	/// The next slot to bind, or `None` once every slot has been handed out.
	///
	/// Bind each member before taking the next: they join the group in the order
	/// they bind, and that order is the only thing the kernel knows them by.
	pub fn member(&mut self) -> Option<Member> {
		let shard = Shard::new(self.next, self.count)?;
		self.next += 1;
		Some(Member {
			shard,
			state: self.state.clone(),
		})
	}
}

/// One member's claim on a slot in a [`Group`], which binding spends.
///
/// Send it wherever the socket is served, a worker's own thread included. It
/// carries the group's address, so every member holds one port whatever the
/// caller thought it asked for.
#[derive(Debug)]
pub struct Member {
	shard: Shard,
	state: Arc<State>,
}

impl Member {
	/// This member's slot, which its connection IDs have to encode
	/// ([`cid_prefix`]).
	pub fn shard(&self) -> Shard {
		self.shard
	}

	/// Bind this member's socket into the group, steering the whole group once
	/// the last member has joined.
	///
	/// Fails when a sibling has not bound yet: the kernel numbers a reuseport
	/// group by bind order, so a member joining out of turn would take another's
	/// slot and steer its traffic, with no error to show for it.
	pub fn bind(self) -> io::Result<UdpSocket> {
		let mut progress = self.state.lock();
		if progress.bound != self.shard.index() {
			return Err(io::Error::other(format!(
				"reuseport member {} cannot bind while {} of {} are in: the kernel numbers a group by bind order",
				self.shard.index(),
				progress.bound,
				self.shard.count(),
			)));
		}

		let socket = bind(progress.addr, self.shard)?;

		// An ephemeral request gives each member a port of its own, so the rest
		// of the group joins the port the first member actually got.
		if self.shard.index() == 0 {
			progress.addr = socket.local_addr()?;
		}
		progress.bound += 1;

		Ok(socket)
	}
}

/// What the members of a forming group share: how far the group has been bound,
/// and the address it holds.
///
/// Shared rather than owned by the [`Group`] because a member is bound wherever
/// its socket is served, which is usually not where the group lives.
#[derive(Debug)]
struct State(Mutex<Progress>);

/// How far a group has been bound, and the address its members hold.
#[derive(Debug)]
struct Progress {
	/// How many members have bound, which is also the only index allowed to bind
	/// next.
	bound: u16,
	addr: SocketAddr,
}

impl State {
	fn new(addr: SocketAddr) -> Self {
		Self(Mutex::new(Progress { bound: 0, addr }))
	}

	/// The progress, whatever a panicking member left behind: a failed bind is
	/// reported by the count it did not advance, so there is no torn state a
	/// poisoned lock would be protecting.
	fn lock(&self) -> MutexGuard<'_, Progress> {
		self.0.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

/// Bind one member's socket into the group at `addr`.
///
/// Private because the order is the whole invariant: reachable only through a
/// [`Member`], which a [`Group`] hands out in index order and only while it
/// holds the port.
fn bind(addr: SocketAddr, shard: Shard) -> io::Result<UdpSocket> {
	// The first member probes with a plain bind before the group forms.
	// `SO_REUSEPORT` groups by address and UID, so a member would otherwise
	// *join* any group a same-UID process already has on the address, as two
	// relays overlapping in a rolling restart do: the old group's filter keeps
	// steering every packet to the old process, the new group reports ready
	// while serving nothing, and the old process exiting renumbers the
	// survivors. A plain bind refuses a held port outright, which turns that
	// overlap back into the `AddrInUse` startup failure it is everywhere else.
	//
	// The probe only sees a group that is already bound. Two processes
	// *constructing* concurrently could each probe while the other holds
	// nothing yet, which is what [`Lock`] excludes; the probe's job is the
	// holder the lock cannot see, one that predates it or never took it.
	if shard.index() == 0 {
		drop(crate::bind::udp(crate::bind::Udp::new(addr))?);
	}

	let socket = crate::bind::udp(crate::bind::Udp::new(addr).with_reuse_port(true))?;

	// The filter covers the group, not the socket, so it goes on once everyone is
	// in. Attaching earlier would steer by an index range that is still growing,
	// and the members that joined later would be unreachable until it was redone.
	if shard.index() + 1 == shard.count() {
		attach(&socket, shard.count())?;
	}

	Ok(socket)
}

/// Holds a listen port for one group's lifetime.
///
/// The [`bind`] probe refuses an address whose group is already *bound*, but
/// two processes constructing concurrently could each probe while the other
/// holds nothing yet, then both join one interleaved reuseport group. This is
/// the exclusion the probe cannot provide: an `flock`ed file named by the
/// port, taken before the first member binds and released by the kernel with
/// the file descriptor, so the lock cannot go stale and dies with its process.
///
/// Keyed by the port alone, because every address space that can overlap on a
/// port must share one lock: `[::]` (dual-stack), `0.0.0.0`, and any specific
/// address all conflict with each other, and giving each spelling a lock of
/// its own would let two of them construct concurrently and race the probe.
/// The price is over-exclusion: two same-UID groups on *distinct* specific
/// addresses sharing a port, or in distinct network namespaces sharing this
/// filesystem, are refused although they could coexist. That failure is loud
/// and names the port; the alternative failure was silent traffic loss.
///
/// The file lives in a directory only this UID can write (see `dir`), which is
/// what makes the lock immune to other users: a reuseport group only admits
/// same-UID sockets, so same-UID is exactly the set that must be excluded,
/// and no other user can touch the lock to deny it. When no such directory
/// can be had, the lock is skipped with a warning rather than refusing to
/// start, and the probe carries the remaining risk.
#[derive(Debug)]
struct Lock {
	#[cfg(target_os = "linux")]
	_file: std::fs::File,
}

impl Lock {
	/// Take the lock for `port`. An error always means another group holds it;
	/// lock infrastructure that is missing or broken (no protected directory,
	/// an unwritable file) skips the lock with a warning and returns `Ok(None)`,
	/// preferring to start on probe-only protection over refusing to start.
	///
	/// Elsewhere than Linux this is a no-op: the platform refuses the reuseport
	/// bind itself, so there is nothing to exclude.
	fn acquire(port: u16) -> io::Result<Option<Self>> {
		#[cfg(target_os = "linux")]
		{
			use std::os::unix::fs::OpenOptionsExt;

			let Some(dir) = dir() else {
				return Ok(None);
			};

			// Mode 0600 and O_NOFOLLOW are defense in depth: the directory is
			// verified accessible to this UID alone, so nobody else can reach the
			// file (an exclusive flock needs no write access, so a readable lock
			// file would be deniable), and a symlink here would be our own doing.
			// O_NONBLOCK keeps a planted FIFO from hanging the open; on a regular
			// file it does nothing.
			let path = dir.join(format!("quic-workers-{port}.lock"));
			let file = match std::fs::OpenOptions::new()
				.write(true)
				.create(true)
				.truncate(false)
				.mode(0o600)
				.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
				.open(&path)
			{
				Ok(file) => file,
				Err(err) => {
					tracing::warn!(?path, %err, "cannot open the lock file; group overlap detection falls back to the bind probe");
					return Ok(None);
				}
			};

			// `mode` above only applies when the open creates the file, so verify
			// and normalize what was actually opened, through the descriptor
			// rather than the path: an inode that predates this directory's
			// lockdown could still be reachable by another user, and an exclusive
			// flock on it needs no write access.
			{
				use std::os::unix::fs::{MetadataExt, PermissionsExt};

				let euid = unsafe { libc::geteuid() };
				let trusted = file.metadata().is_ok_and(|meta| meta.is_file() && meta.uid() == euid);
				if !trusted || file.set_permissions(std::fs::Permissions::from_mode(0o600)).is_err() {
					tracing::warn!(
						?path,
						"the lock file is not exclusively ours; group overlap detection falls back to the bind probe"
					);
					return Ok(None);
				}
			}

			// SAFETY: `file` is an open descriptor for the duration of the call.
			let res = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX | libc::LOCK_NB) };
			if res == 0 {
				return Ok(Some(Self { _file: file }));
			}

			let err = io::Error::last_os_error();
			if err.kind() == io::ErrorKind::WouldBlock {
				return Err(err);
			}
			tracing::warn!(?path, %err, "cannot lock the lock file; group overlap detection falls back to the bind probe");
			Ok(None)
		}
		#[cfg(not(target_os = "linux"))]
		{
			let _ = port;
			Ok(None)
		}
	}
}

/// A directory only this UID can write, for the group locks. `None` disables
/// locking with a warning rather than trusting a directory another user could
/// tamper with.
///
/// `XDG_RUNTIME_DIR` is preferred: per-UID, mode 0700, and living with the
/// session is exactly the shape the lock wants. But it is an environment
/// variable, not a guarantee, so its candidate is verified exactly like the
/// temp-dir fallback rather than trusted, and a bad value falls through to
/// the fallback instead of costing more protection than no value at all.
#[cfg(target_os = "linux")]
fn dir() -> Option<std::path::PathBuf> {
	if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())
		&& let Some(dir) = prepare(std::path::PathBuf::from(runtime).join("moq"))
	{
		return Some(dir);
	}

	let euid = unsafe { libc::geteuid() };
	prepare(std::env::temp_dir().join(format!("moq-{euid}")))
}

/// Create `dir` with mode 0700 if missing, then verify it is a directory this
/// UID alone can reach. `/tmp` is world-writable and `XDG_RUNTIME_DIR` is only
/// an environment variable, so the name could already be held by another
/// user's file or symlink, and trusting it would hand them the lock.
///
/// The parent is checked first: it must be sticky (an entry in `/tmp` can only
/// be renamed or unlinked by its owner) or writable by this UID alone, or
/// another user could rename the verified directory out from under its path
/// and split two groups onto different lock files. Components above the
/// parent are the operator's: a runtime directory placed inside another
/// user's tree is a misconfiguration no check here can launder.
#[cfg(target_os = "linux")]
fn prepare(dir: std::path::PathBuf) -> Option<std::path::PathBuf> {
	use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

	let euid = unsafe { libc::geteuid() };
	let parent = dir
		.parent()
		.and_then(|parent| std::fs::symlink_metadata(parent).ok())
		.is_some_and(|meta| {
			let mode = meta.permissions().mode();
			// A trusted owner first: the parent's owner can rename entries
			// regardless of the sticky bit, so an arbitrary user's 1777
			// directory protects nothing while root's /tmp does. Then either
			// sticky (other writers cannot touch entries they do not own) or
			// no other writers at all.
			let owner = meta.uid() == euid || meta.uid() == 0;
			meta.is_dir() && owner && (mode & 0o1000 != 0 || mode & 0o022 == 0)
		});
	if !parent {
		tracing::warn!(
			?dir,
			"the lock directory's parent cannot protect it; group overlap detection falls back to the bind probe"
		);
		return None;
	}

	let created = std::fs::DirBuilder::new().mode(0o700).create(&dir);
	if let Err(err) = &created
		&& err.kind() != io::ErrorKind::AlreadyExists
	{
		tracing::warn!(?dir, %err, "cannot create a lock directory; group overlap detection falls back to the bind probe");
		return None;
	}

	// `symlink_metadata` so a symlink is seen as itself and fails `is_dir`,
	// rather than being followed to wherever it points. No group or world
	// access at all: an exclusive flock needs no write access, so even a
	// readable lock file would let another user deny the port.
	let safe = std::fs::symlink_metadata(&dir)
		.map(|meta| meta.is_dir() && meta.uid() == euid && meta.permissions().mode() & 0o077 == 0)
		.unwrap_or(false);
	if !safe {
		tracing::warn!(
			?dir,
			"lock directory is not exclusively ours; group overlap detection falls back to the bind probe"
		);
		return None;
	}
	Some(dir)
}

/// The largest group the steering filter can address.
///
/// It selects with one byte of the connection ID, so a member past 255 could
/// never be named: [`cid_prefix`] would have no stride to spend and the filter's
/// `byte % count` could never return that index.
pub const MAX_SHARDS: u16 = 256;

/// The first byte of a connection ID issued by `shard`.
///
/// `count` values of the byte are reserved per member, so this keeps
/// `256 / count` of its randomness: 32 values across 8 members, and the other
/// bytes of the ID stay fully random either way. Connection IDs are not secrets,
/// but they are unlinkable only while they look random, which is why this spends
/// as little of that byte as it can.
pub fn cid_prefix(shard: Shard) -> u8 {
	use rand::RngExt;

	let count = u32::from(shard.count());
	// The number of whole strides of `count` that fit in a byte. At least one,
	// because a group is never larger than `MAX_SHARDS`.
	let strides = 256 / count;
	let stride = rand::rng().random_range(0..strides);

	(stride * count + u32::from(shard.index())) as u8
}

/// Attach the steering filter to a socket in a reuseport group of `count`.
///
/// Applies to the group, so once is enough, and the last member to bind is the
/// first point where that is true.
#[cfg(target_os = "linux")]
fn attach(socket: &UdpSocket, count: u16) -> io::Result<()> {
	use std::os::fd::AsRawFd;

	let program = program(count);
	let fprog = libc::sock_fprog {
		len: program.len() as u16,
		filter: program.as_ptr() as *mut libc::sock_filter,
	};

	// SAFETY: `fprog` points at `program`, which outlives the call, and its `len`
	// is that slice's length.
	let res = unsafe {
		libc::setsockopt(
			socket.as_raw_fd(),
			libc::SOL_SOCKET,
			libc::SO_ATTACH_REUSEPORT_CBPF,
			std::ptr::from_ref(&fprog).cast(),
			size_of::<libc::sock_fprog>() as libc::socklen_t,
		)
	};

	if res != 0 {
		return Err(io::Error::last_os_error());
	}

	tracing::debug!(count, "steering the reuseport group by connection ID");
	Ok(())
}

#[cfg(not(target_os = "linux"))]
fn attach(_socket: &UdpSocket, _count: u16) -> io::Result<()> {
	// Unreachable in practice: a member only gets here once `bind::udp` accepted
	// `SO_REUSEPORT`, which is Linux-only.
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"reuseport steering is Linux-only",
	))
}

/// The classic-BPF program that maps a QUIC packet to its owner's index.
///
/// Reads the first byte of the destination connection ID, whose offset depends
/// on the header form, and reduces it modulo the group size. The packet starts
/// at offset 0 because the kernel pulls the UDP header off before running this.
///
/// A long header carries an explicit ID length, and a zero there would make
/// offset 6 the *source* ID's length byte instead. Not worth a branch: only a
/// malformed or hostile packet gets there, since a client's Initial must carry a
/// destination ID of at least 8 bytes, and the worst outcome is that one junk
/// packet reaches a member that discards it.
#[cfg(target_os = "linux")]
fn program(count: u16) -> [libc::sock_filter; 7] {
	/// Long-header form bit in the first byte of a QUIC packet.
	const LONG_HEADER: u32 = 0x80;
	/// 1 first byte + 4 version bytes + 1 length byte precede a long header's
	/// destination connection ID.
	const LONG_DCID: u32 = 6;
	/// A short header's destination connection ID follows the first byte.
	const SHORT_DCID: u32 = 1;

	fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
		libc::sock_filter {
			code: code as u16,
			jt,
			jf,
			k,
		}
	}

	[
		// A = packet[0]
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, 0),
		// Long header? Skip the short-header load.
		insn(libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K, 2, 0, LONG_HEADER),
		// A = packet[1]
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, SHORT_DCID),
		// Skip the long-header load.
		insn(libc::BPF_JMP | libc::BPF_JA | libc::BPF_K, 0, 0, 1),
		// A = packet[6]
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, LONG_DCID),
		// A %= count. The kernel would reduce out-of-range indices to a 4-tuple
		// hash rather than an index, so the program has to land in range itself.
		insn(libc::BPF_ALU | libc::BPF_MOD | libc::BPF_K, 0, 0, u32::from(count)),
		insn(libc::BPF_RET | libc::BPF_A, 0, 0, 0),
	]
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A slot has to exist in the group it names, and the group has to be one
	/// the steering filter can address: `cid_prefix` divides by the count, so
	/// an oversized group would panic on a zero-width stride.
	#[test]
	fn shard_slots_are_bounded() {
		assert_eq!(Shard::new(0, 1).map(|shard| shard.count()), Some(1));
		assert_eq!(Shard::new(3, 4).map(|shard| shard.index()), Some(3));
		assert!(Shard::new(4, 4).is_none());
		assert!(Shard::new(0, 0).is_none());
		assert!(Shard::new(0, MAX_SHARDS).is_some());
		assert!(Shard::new(0, MAX_SHARDS + 1).is_none());
	}

	/// The group is the only source of slots, and it hands out exactly the ones
	/// it was sized for, in order.
	#[test]
	fn a_group_hands_out_every_slot_in_order() {
		const COUNT: u16 = 4;

		let mut group = Group::acquire("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		assert_eq!(group.count(), COUNT);

		for index in 0..COUNT {
			let member = group.member().expect("a slot per member");
			assert_eq!(member.shard().index(), index);
			assert_eq!(member.shard().count(), COUNT);
		}
		assert!(group.member().is_none(), "the group cannot be resized");
	}

	/// A group of zero is a group of one: every caller sizes a group from a
	/// worker count, and a lone member is a valid group.
	#[test]
	fn an_empty_group_holds_one_member() {
		let mut group = Group::acquire("127.0.0.1:0".parse().unwrap(), 0).unwrap();
		assert_eq!(group.count(), 1);
		assert_eq!(group.member().map(|member| member.shard().count()), Some(1));
	}

	/// The steering filter names a member with one byte, so a group past 256 has
	/// members it could never reach. Refused where the group is sized, rather
	/// than when the first connection ID has no stride left to spend.
	#[test]
	fn an_unaddressable_group_is_refused() {
		let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		assert!(Group::acquire(addr, MAX_SHARDS).is_ok());
		assert!(matches!(
			Group::acquire(addr, MAX_SHARDS + 1),
			Err(Error::Count { count: 257, max: 256 })
		));
	}

	/// The encoding's only real requirement: whatever byte a member issues has to
	/// reduce back to that member.
	#[test]
	fn a_prefix_reduces_to_its_own_shard() {
		// `MAX_SHARDS` is the interesting end: one stride per member, so the prefix
		// is the index itself and there is no randomness left to spend.
		for count in [1u16, 2, 3, 4, 7, 8, 16, 64, 255, MAX_SHARDS] {
			for index in 0..count {
				let shard = Shard::new(index, count).unwrap();
				// Sampled, since the prefix is randomized within the member's stride.
				for _ in 0..64 {
					let prefix = cid_prefix(shard);
					assert_eq!(
						u16::from(prefix) % count,
						index,
						"prefix {prefix} of shard {index}/{count} steers elsewhere"
					);
				}
			}
		}
	}

	/// Every member of a group has to be reachable, or the ones that aren't sit
	/// idle while their share of the traffic goes to a sibling.
	#[test]
	fn prefixes_cover_every_shard() {
		const COUNT: u16 = 8;
		let mut seen = std::collections::HashSet::new();
		for index in 0..COUNT {
			let shard = Shard::new(index, COUNT).unwrap();
			for _ in 0..256 {
				seen.insert(u16::from(cid_prefix(shard)) % COUNT);
			}
		}
		assert_eq!(seen.len(), usize::from(COUNT));
	}

	/// An ephemeral request gives the first member a port of the kernel's
	/// choosing, and the group is only balanced over a port every member holds.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_group_shares_one_ephemeral_port() {
		const COUNT: u16 = 3;

		let mut group = Group::acquire("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		let sockets: Vec<UdpSocket> = (0..COUNT)
			.map(|_| group.member().expect("a slot per member").bind().expect("bind member"))
			.collect();

		let addr = group.addr();
		assert_ne!(addr.port(), 0, "the group holds the port its first member bound");
		for socket in &sockets {
			assert_eq!(
				socket.local_addr().unwrap(),
				addr,
				"every member holds the group's port"
			);
		}
	}

	/// The kernel numbers a group by bind order, so a member that binds out of
	/// turn would take a sibling's slot and steer its traffic. Refused instead,
	/// which is the one part of the ordering rule a caller can still get wrong
	/// once its members are bound on threads of their own.
	#[test]
	#[cfg(target_os = "linux")]
	fn binding_out_of_order_is_refused() {
		let mut group = Group::acquire("127.0.0.1:0".parse().unwrap(), 2).unwrap();
		let first = group.member().expect("first slot");
		let second = group.member().expect("second slot");

		second.bind().expect_err("the second member cannot bind first");

		// Refused, not deferred: nothing joined the group, so the slot the
		// kernel would have handed the second member is still the first's.
		let socket = first.bind().expect("bind the first member");
		assert_eq!(socket.local_addr().unwrap(), group.addr());
	}

	/// The whole point, end to end against a real kernel: a packet carrying a
	/// member's connection ID has to reach that member and no other.
	///
	/// This is what the 4-tuple hash cannot do. Every packet here is sent from a
	/// different ephemeral source port, so under the default hashing the landing
	/// socket would be unrelated to the connection ID.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_connection_id_reaches_its_own_member() {
		const COUNT: u16 = 4;

		let mut group = Group::acquire("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		let mut sockets = Vec::new();
		let mut shards = Vec::new();
		while let Some(member) = group.member() {
			shards.push(member.shard());
			let socket = member.bind().expect("bind group member");
			socket.set_nonblocking(true).unwrap();
			sockets.push(socket);
		}
		let addr = group.addr();

		/// Which member received something, given every send targets exactly one.
		fn receiver(group: &[UdpSocket]) -> Option<usize> {
			let mut buf = [0u8; 64];
			group.iter().position(|socket| socket.recv_from(&mut buf).is_ok())
		}

		for (index, shard) in shards.into_iter().enumerate() {
			let prefix = cid_prefix(shard);

			// A short header: form bit clear, connection ID straight after.
			let short = [0x40, prefix, 1, 2, 3, 4, 5, 6, 7, 8];
			// A long header: form bit set, then version, then the ID's length.
			let long = [0xc0, 0, 0, 0, 1, 8, prefix, 1, 2, 3, 4, 5, 6, 7];

			for (form, packet) in [("short", &short[..]), ("long", &long[..])] {
				// A fresh source port per packet, so nothing but the connection ID
				// can be steering this.
				let sender = crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
				sender.send_to(packet, addr).unwrap();

				// The send is local and the filter is synchronous, but the receive
				// still has to be given a moment to land.
				std::thread::sleep(std::time::Duration::from_millis(50));
				assert_eq!(
					receiver(&sockets),
					Some(index),
					"{form} header for member {index} landed on the wrong socket"
				);
			}
		}
	}

	/// A second group on a named port must lose it while the first is alive: two
	/// groups constructing at once would each pass the bind probe before either
	/// held the port, then interleave into one group whose filter steers each
	/// process's traffic to the other's sockets.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_second_group_cannot_take_the_port() {
		// A port the kernel just handed out and nothing holds any more, which is
		// as close to a reserved port as a test gets.
		let addr = {
			let probe = crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
			probe.local_addr().unwrap()
		};

		let mut first = Group::acquire(addr, 1).unwrap();
		let socket = first.member().unwrap().bind().expect("bind the first group");

		assert!(
			matches!(Group::acquire(addr, 1), Err(Error::Overlap { .. })),
			"a second group took a held port"
		);

		// The lock dies with the group, so the port is takeable again.
		drop(first);
		drop(socket);
		Group::acquire(addr, 1).expect("the released port must be takeable again");
	}

	/// The jumps are offsets from the *following* instruction, so an off-by-one
	/// silently reads the wrong byte rather than failing to load.
	#[test]
	#[cfg(target_os = "linux")]
	fn the_program_branches_to_the_right_loads() {
		let program = program(4);
		assert_eq!(program.len(), 7);

		// Instruction 1 falls through to the short-header load at 2, and jumps
		// over it (and its trailing jump) to the long-header load at 4.
		assert_eq!(program[1].jt, 2, "long header must land on the long load");
		assert_eq!(program[1].jf, 0, "short header must fall through");
		assert_eq!(program[2].k, 1, "short header reads the byte after the first");
		assert_eq!(program[3].k, 1, "the short path must skip the long load");
		assert_eq!(program[4].k, 6, "long header reads past version and length");
		assert_eq!(program[5].k, 4, "the modulus is the group size");
	}
}
