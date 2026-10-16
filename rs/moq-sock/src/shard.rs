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
//! [`bind`] is the whole entry point. It takes the port, binds every member in
//! index order, attaches the steering filter, and returns a [`Bound`] group
//! that hands out one serving [`Socket`] per member while retaining every
//! underlying socket for its own lifetime.
//!
//! ```no_run
//! # fn example(addr: std::net::SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
//! let mut group = moq_sock::shard::bind(addr, 4)?;
//! let mut members = Vec::new();
//! while let Some(member) = group.member()? {
//!     members.push((member.shard(), member.into_inner()));
//! }
//! // Serve each socket with an endpoint whose connection IDs name its shard.
//! # Ok(())
//! # }
//! ```
//!
//! # The encoding
//!
//! The filter reads the first byte of a packet's destination connection ID and
//! reduces it modulo the group size. The endpoint serving a member has to issue
//! 8-byte connection IDs whose first byte is congruent to the member's
//! [`Shard::index`] modulo [`Shard::count`], and whose fourth byte has both
//! bits of `0x18` set, a pattern no valid SRT data header carries, so a short
//! header from another protocol on the port keeps the 4-tuple hash.
//! `moq_quic::RandomConnectionIdGenerator::set_shard` issues exactly those.
//! The kernel selects `socks[A % count]` for a returned accumulator `A`, so
//! feeding it that byte lands on the owner.
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
//! index. That rule fits in classic BPF, with no `CAP_BPF`, no
//! BPF toolchain in the build, and no map to keep alive across restarts. The
//! cost is that the kernel selects by *position*, so the group has to be bound
//! once, in order, and never resized, which is why [`bind`] does all of it.

use std::io;
use std::net::{SocketAddr, UdpSocket};

/// One member's slot in a group of sockets sharing a port via `SO_REUSEPORT`.
///
/// Every member binds the same address and the kernel spreads inbound datagrams
/// across the group, so N servers on N threads can serve one port without a
/// shared socket between them. The kernel identifies a member by its *position*
/// in the group, which is why only [`bind`] mints one: a shard is meaningful
/// only inside a group that was bound once, in index order, and is never
/// resized.
///
/// Carry it wherever the member issues connection IDs, which have to name it
/// (see [the encoding](self#the-encoding)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shard {
	index: u16,
	count: u16,
}

impl Shard {
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

	/// No port could be resolved for an ephemeral request.
	#[error("failed to resolve an ephemeral port")]
	Resolve(#[source] io::Error),

	/// A member could not bind, including [`io::ErrorKind::Unsupported`]
	/// anywhere but Linux and [`io::ErrorKind::AddrInUse`] when the address is
	/// already held.
	#[error("failed to bind reuseport member {index}")]
	Bind {
		/// The member that failed.
		index: u16,
		/// What the bind returned.
		#[source]
		source: io::Error,
	},

	/// The steering filter could not be attached.
	#[error("failed to steer the reuseport group")]
	Steer(#[source] io::Error),
}

/// Bind a steered group of `count` sockets sharing `addr`.
///
/// Everything a steered group has to get right happens here rather than in its
/// caller, because none of it is visible in a socket afterwards:
///
/// - The port is locked before the first member binds and stays locked until the
///   returned group and every socket it handed out are gone, so a second
///   same-UID group cannot interleave into this one. Hold the group for as long
///   as its sockets are served.
/// - The member count is checked once, against what the steering filter can
///   address, and never changes.
/// - Members bind in index order, which is the order the kernel numbers them
///   by, and the filter is attached before any socket is released.
///
/// A `count` of zero is a group of one: a lone member is a valid group.
///
/// An ephemeral port (`0`) is resolved first, with a plain bind that is
/// dropped again, and the group then takes that port like a named one.
/// Never a reuseport bind of port `0`: Linux counts a port held only by
/// same-UID reuseport sockets as free for one, so the kernel could hand
/// back a port another group holds and this one would silently join it. A
/// plain bind conflicts with every holder. Another socket taking the port
/// before the first member binds fails that bind with
/// [`io::ErrorKind::AddrInUse`].
pub fn bind(addr: SocketAddr, count: u16) -> Result<Bound, Error> {
	let count = count.max(1);
	if count > MAX_SHARDS {
		return Err(Error::Count { count, max: MAX_SHARDS });
	}

	let addr = match addr.port() {
		0 => crate::bind::udp(crate::bind::Udp::new(addr))
			.and_then(|socket| socket.local_addr())
			.map_err(Error::Resolve)?,
		_ => addr,
	};

	let port = addr.port();
	let lock = Lock::acquire(port).map_err(|_| Error::Overlap { port })?;

	let mut sockets = Vec::with_capacity(usize::from(count));
	for index in 0..count {
		let socket = member(addr, index).map_err(|source| Error::Bind { index, source })?;
		sockets.push((Shard { index, count }, socket));
	}

	let (_, last) = sockets.last().expect("a group always has at least one member");
	attach(last, count).map_err(Error::Steer)?;

	Ok(Bound {
		sockets,
		next: 0,
		addr,
		_lock: lock,
	})
}

/// A complete steered group that retains every member socket.
///
/// [`member`](Self::member) returns duplicate handles to the retained sockets.
/// Dropping one handle stops serving that slot without removing it from the
/// kernel group or renumbering any survivor. Dropping this group closes every
/// retained member together and releases the port.
#[derive(Debug)]
pub struct Bound {
	sockets: Vec<(Shard, UdpSocket)>,
	next: usize,
	addr: SocketAddr,

	/// Held for exactly as long as the complete socket group, and released by
	/// the kernel with the process. `None` on a host with no lock directory.
	_lock: Option<Lock>,
}

impl Bound {
	/// How many sockets share the port.
	pub fn count(&self) -> u16 {
		self.sockets.len().try_into().expect("group count fits u16")
	}

	/// The address every member holds, with an ephemeral port already resolved.
	pub fn addr(&self) -> SocketAddr {
		self.addr
	}

	/// The next socket to serve, or `None` once every member was handed out.
	///
	/// The group retains the underlying socket, so dropping the returned handle
	/// cannot resize the kernel's reuseport array.
	pub fn member(&mut self) -> io::Result<Option<Socket>> {
		let Some((shard, socket)) = self.sockets.get(self.next) else {
			return Ok(None);
		};
		let socket = socket.try_clone()?;
		self.next += 1;
		Ok(Some(Socket { shard: *shard, socket }))
	}
}

/// One serving handle released by a complete [`Bound`] group.
#[derive(Debug)]
pub struct Socket {
	shard: Shard,
	socket: UdpSocket,
}

impl Socket {
	/// This socket's position in the group.
	pub fn shard(&self) -> Shard {
		self.shard
	}

	/// Release the UDP handle while the [`Bound`] group retains its member.
	pub fn into_inner(self) -> UdpSocket {
		self.socket
	}
}

/// Bind member `index` of the group at `addr`.
///
/// Private because the order is the whole invariant: only [`bind`] calls this,
/// once per index in order, while it holds the port.
fn member(addr: SocketAddr, index: u16) -> io::Result<UdpSocket> {
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
	// holder the lock cannot see, one that predates it or never took it, or
	// one that took an ephemeral group's port after [`bind`] resolved it.
	if index == 0 {
		drop(crate::bind::udp(crate::bind::Udp::new(addr))?);
	}

	crate::bind::udp(crate::bind::Udp::new(addr).with_reuse_port(true))
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
/// never be named: the filter's `byte % count` could never return that index.
pub const MAX_SHARDS: u16 = 256;

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
/// A long header is QUIC when it carries v1 or v2. A short header is QUIC when
/// its fixed bit and relay CID marker are set. For either, the first destination
/// CID byte is reduced modulo the group size. Everything else returns an invalid
/// index, which asks reuseport to use its 4-tuple hash. The packet starts at
/// offset 0 because the kernel pulls the UDP header off before running this.
///
/// A load past the end of the datagram aborts the program with 0, which
/// reuseport takes as member 0, so anything too short for every load is
/// rejected first.
#[cfg(target_os = "linux")]
fn program(count: u16) -> [libc::sock_filter; 18] {
	/// Long-header form bit in the first byte of a QUIC packet.
	const LONG_HEADER: u32 = 0x80;
	/// Fixed bit on short headers. Greasing is disabled on sharded endpoints.
	const FIXED_BIT: u32 = 0x40;
	const VERSION_1: u32 = 0x0000_0001;
	const VERSION_2: u32 = 0x6b33_43cf;
	/// The SRT key-encryption bits overlaid by the fourth destination CID byte.
	const MAGIC_MASK: u32 = 0x18;
	/// 1 first byte + 4 version bytes + 1 length byte precede a long header's
	/// destination connection ID.
	const LONG_DCID: u32 = 6;
	/// A short header's destination connection ID follows the first byte.
	const SHORT_DCID: u32 = 1;
	/// The shortest datagram every load below stays inside.
	const MIN_LEN: u32 = LONG_DCID + 1;

	fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
		libc::sock_filter {
			code: code as u16,
			jt,
			jf,
			k,
		}
	}

	[
		// A = datagram length; too short to be QUIC uses the kernel hash.
		insn(libc::BPF_LD | libc::BPF_W | libc::BPF_LEN, 0, 0, 0),
		insn(libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K, 0, 12, MIN_LEN),
		// A = packet[0]
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, 0),
		// Long header? Jump to its version check.
		insn(libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K, 7, 0, LONG_HEADER),
		// A short header must retain QUIC's fixed bit.
		insn(libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K, 0, 9, FIXED_BIT),
		// A = packet[4], the fourth destination CID byte.
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, SHORT_DCID + 3),
		insn(libc::BPF_ALU | libc::BPF_AND | libc::BPF_K, 0, 0, MAGIC_MASK),
		// A short header without the relay CID marker uses the kernel hash.
		insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 0, 6, MAGIC_MASK),
		// A = packet[1]
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, SHORT_DCID),
		insn(libc::BPF_ALU | libc::BPF_MOD | libc::BPF_K, 0, 0, u32::from(count)),
		insn(libc::BPF_RET | libc::BPF_A, 0, 0, 0),
		// Long headers steer only supported QUIC versions.
		insn(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 1),
		insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 2, 0, VERSION_1),
		insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 1, 0, VERSION_2),
		// An out-of-range result asks reuseport to use its 4-tuple hash.
		insn(libc::BPF_RET | libc::BPF_K, 0, 0, u32::from(count)),
		// A = packet[6]
		insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, LONG_DCID),
		insn(libc::BPF_ALU | libc::BPF_MOD | libc::BPF_K, 0, 0, u32::from(count)),
		insn(libc::BPF_RET | libc::BPF_A, 0, 0, 0),
	]
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The length of the connection IDs a sharded endpoint issues.
	const CID_LEN: usize = 8;

	/// A connection ID shaped like a sharded endpoint's (see [the
	/// encoding](super#the-encoding)): the first byte randomized within the
	/// member's stride, so nothing but the reduction can be steering, and the
	/// non-SRT marker set.
	fn cid(shard: Shard) -> [u8; CID_LEN] {
		use rand::RngExt;
		let mut id: [u8; CID_LEN] = rand::random();
		let count = u32::from(shard.count());
		let stride = rand::rng().random_range(0..256 / count);
		id[0] = (stride * count + u32::from(shard.index())) as u8;
		id[3] |= 0x18;
		id
	}

	/// The steering filter names a member with one byte, so a group past 256 has
	/// members it could never reach. Refused before anything binds.
	#[test]
	fn an_unaddressable_group_is_refused() {
		let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		assert!(matches!(
			bind(addr, MAX_SHARDS + 1),
			Err(Error::Count { count: 257, max: 256 })
		));
	}

	/// Elsewhere the kernel cannot steer, so a group is refused at its first
	/// member rather than served by a socket that silently balances nothing.
	#[test]
	#[cfg(not(target_os = "linux"))]
	fn a_group_is_refused_off_linux() {
		let err = bind("127.0.0.1:0".parse().unwrap(), 2).expect_err("steered a group off Linux");
		assert!(matches!(err, Error::Bind { index: 0, ref source } if source.kind() == io::ErrorKind::Unsupported));
	}

	/// The group hands out exactly the slots it was sized for, in order, every
	/// one on the port the first member resolved.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_group_hands_out_every_slot_on_one_port() {
		const COUNT: u16 = 3;

		let mut group = bind("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		assert_eq!(group.count(), COUNT);
		let addr = group.addr();
		assert_ne!(addr.port(), 0, "the group holds the port its first member bound");

		for index in 0..COUNT {
			let member = group.member().unwrap().expect("a socket per member");
			assert_eq!(member.shard(), Shard { index, count: COUNT });
			assert_eq!(member.into_inner().local_addr().unwrap(), addr);
		}
		assert!(group.member().unwrap().is_none(), "the group cannot be resized");
	}

	/// A group of zero is a group of one: a lone member is a valid group.
	#[test]
	#[cfg(target_os = "linux")]
	fn an_empty_group_holds_one_member() {
		let group = bind("127.0.0.1:0".parse().unwrap(), 0).unwrap();
		assert_eq!(group.count(), 1);
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

		let mut group = bind("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		let mut sockets = Vec::new();
		let mut shards = Vec::new();
		while let Some(member) = group.member().expect("clone retained socket") {
			shards.push(member.shard());
			let socket = member.into_inner();
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
			let id = cid(shard);

			// A short header: form bit clear, connection ID straight after.
			let mut short = [0; CID_LEN + 1];
			short[0] = 0x40;
			short[1..].copy_from_slice(&id);
			let long = |version: [u8; 4]| {
				let mut packet = [0; CID_LEN + 6];
				packet[..6].copy_from_slice(&[0xc0, version[0], version[1], version[2], version[3], CID_LEN as u8]);
				packet[6..].copy_from_slice(&id);
				packet
			};
			let v1 = long([0, 0, 0, 1]);
			let v2 = long([0x6b, 0x33, 0x43, 0xcf]);

			for (form, packet) in [("short", &short[..]), ("v1", &v1[..]), ("v2", &v2[..])] {
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

	/// Datagram protocols that share the port stay on the kernel's 4-tuple hash,
	/// even when fields at the QUIC connection-ID offsets change or are missing.
	#[test]
	#[cfg(target_os = "linux")]
	fn non_quic_flows_stay_on_one_member() {
		const COUNT: u16 = 4;
		let mut group = bind("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		let mut sockets = Vec::new();
		while let Some(member) = group.member().expect("clone retained socket") {
			let socket = member.into_inner();
			socket
				.set_read_timeout(Some(std::time::Duration::from_millis(100)))
				.unwrap();
			sockets.push(socket);
		}

		let flows = [
			("rtp", [0x80, 96, 0, 1, 0, 0, 0, 1, 0x12, 0x34, 0x56, 0x78]),
			// SRT data with KK=0b10 (no encryption); R is toggled below.
			("srt", [0x40, 0, 0, 1, 0x10, 0, 0, 1, 0x12, 0x34, 0x56, 0x78]),
			("stun", [0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4]),
			("dtls", [22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
		];
		for (name, mut packet) in flows {
			let mut buf = [0; 64];
			// A flow hashed off member 0, so a filter that faults to index 0 shows up.
			let (sender, owner) = (0..64)
				.find_map(|_| {
					let sender = crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
					sender.send_to(&packet, group.addr()).unwrap();
					let owner = sockets
						.iter()
						.position(|socket| socket.recv_from(&mut buf).is_ok())
						.expect("one member receives the flow");
					(owner != 0).then_some((sender, owner))
				})
				.expect("a flow hashed off member 0");

			for sequence in 2..18u8 {
				packet[3] = sequence;
				if name == "srt" {
					packet[4] ^= 0x04; // retransmission flag
				}
				sender.send_to(&packet, group.addr()).unwrap();
				sockets[owner]
					.recv_from(&mut buf)
					.unwrap_or_else(|err| panic!("{name} packet {sequence} changed shards: {err}"));
			}

			// Too short to reach the connection-ID offsets the filter reads.
			sender.send_to(&packet[..2], group.addr()).unwrap();
			sockets[owner]
				.recv_from(&mut buf)
				.unwrap_or_else(|err| panic!("truncated {name} packet changed shards: {err}"));
		}
	}

	/// Dropping one serving handle leaves its retained member in the kernel
	/// array, so every later slot keeps the position its connection IDs encode.
	#[test]
	#[cfg(target_os = "linux")]
	fn dropping_a_serving_handle_does_not_resize_the_group() {
		let mut group = bind("127.0.0.1:0".parse().unwrap(), 2).unwrap();
		let first = group.member().unwrap().expect("first serving socket");
		let second = group.member().unwrap().expect("second serving socket");
		let shard = second.shard();
		let socket = second.into_inner();
		socket
			.set_read_timeout(Some(std::time::Duration::from_secs(1)))
			.unwrap();
		drop(first);

		let mut packet = [0; CID_LEN + 1];
		packet[0] = 0x40;
		packet[1..].copy_from_slice(&cid(shard));
		let sender = crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
		sender.send_to(&packet, group.addr()).unwrap();

		let mut buf = [0; 16];
		assert_eq!(socket.recv_from(&mut buf).unwrap().0, packet.len());
	}

	/// A second group on a held port must lose it while the first is alive, for
	/// a named port and a resolved ephemeral one alike: two groups constructing
	/// at once would each pass the bind probe before either held the port, then
	/// interleave into one group whose filter steers each process's traffic to
	/// the other's sockets.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_second_group_cannot_take_the_port() {
		// A port the kernel just handed out and nothing holds any more, which is
		// as close to a reserved port as a test gets.
		let named = {
			let probe = crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
			probe.local_addr().unwrap()
		};

		for addr in [named, "127.0.0.1:0".parse().unwrap()] {
			let first = bind(addr, 1).expect("bind the first group");
			let addr = first.addr();

			assert!(
				matches!(bind(addr, 1), Err(Error::Overlap { .. })),
				"a second group took a held port"
			);

			// The lock dies with the group, so the port is takeable again.
			drop(first);
			bind(addr, 1).expect("the released port must be takeable again");
		}
	}

	/// A same-UID reuseport socket already on the address, one the lock cannot
	/// see, must fail the first member's bind rather than be joined by it.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_held_address_is_refused_not_joined() {
		let intruder =
			crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap()).with_reuse_port(true)).unwrap();
		let addr = intruder.local_addr().unwrap();

		let err = bind(addr, 1).expect_err("joined the intruder's group");
		assert!(matches!(err, Error::Bind { index: 0, ref source } if source.kind() == io::ErrorKind::AddrInUse));
	}

	/// The jumps are offsets from the *following* instruction, so an off-by-one
	/// silently reads the wrong byte rather than failing to load.
	#[test]
	#[cfg(target_os = "linux")]
	fn the_program_branches_to_the_right_loads() {
		let program = program(4);
		assert_eq!(program.len(), 18);

		// Instruction 1 sends a short datagram to the kernel-hash return at 14.
		assert_eq!(program[1].k, 7, "every load must stay inside the datagram");
		assert_eq!(program[1].jf, 12, "a short datagram must use the kernel hash");
		assert_eq!(program[14].k, 4, "the kernel-hash return is out of range");
		// Instruction 3 falls through to the short-header checks, and jumps to
		// the long-header version load at 11.
		assert_eq!(program[3].jt, 7, "long header must land on the version load");
		assert_eq!(program[3].jf, 0, "short header must fall through");
		assert_eq!(program[5].k, 4, "short header reads the fourth CID byte");
		assert_eq!(program[8].k, 1, "short header reads the shard byte");
		assert_eq!(program[11].k, 1, "long header reads its version");
		assert_eq!(program[15].k, 6, "long header reads past version and length");
		assert_eq!(program[9].k, 4, "the short modulus is the group size");
		assert_eq!(program[16].k, 4, "the long modulus is the group size");
	}
}
