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
//! header from another protocol on the port is balanced by its source address
//! and port instead, one flow to one member.
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
//!
//! # Handing the port to a successor
//!
//! A group bound with [`Config::handoff`] holds two generations of members, one
//! per process, so an upgraded process can serve the port beside the one it
//! replaces. Each process serves one generation and issues connection IDs whose
//! index names a member of it, so the predecessor's connections keep reaching
//! the predecessor. The successor takes every member by file descriptor
//! ([`Bound::handoff`], [`Bound::adopt`]) rather than binding its own: both
//! processes then hold every member, so neither one exiting closes a socket and
//! renumbers the survivors, which is what positional steering cannot survive.
//!
//! A client's first Initial carries a connection ID it invented, which names no
//! member, so those packets go to whichever generation last called
//! [`Bound::steer`]: the newest process takes every new handshake. A Handshake
//! or short-header packet carries an ID the server chose and reaches its exact
//! member while two generations serve ([`Steer::Beside`]), or the newest
//! generation's member of the same position once the other is gone
//! ([`Steer::Alone`]), where the connection ID generator refuses it.

use std::io;
use std::net::{SocketAddr, UdpSocket};
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;

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

	/// A handed-off group did not describe the sockets it carried.
	#[error("cannot adopt the handed-off reuseport group: {0}")]
	Adopt(String),
}

/// Bind a steered group of `count` sockets sharing `addr`, with no room for a
/// successor: [`Config::bind`] with [`Config::handoff`] off.
pub fn bind(addr: SocketAddr, count: u16) -> Result<Bound, Error> {
	Config::new(addr, count).bind()
}

/// The shape of a steered group, for [`Config::bind`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The address every member binds. Port `0` resolves an ephemeral port first.
	pub addr: SocketAddr,
	/// How many members this process serves. Zero is a group of one.
	pub count: u16,
	/// Also bind a second generation of `count` members, held in reserve for a
	/// successor process to [`adopt`](Bound::adopt) and serve beside this one.
	/// Doubles the members the steering filter has to address. Defaults to off.
	pub handoff: bool,
}

impl Config {
	/// A group of `count` members on `addr`, with no room for a successor.
	pub fn new(addr: SocketAddr, count: u16) -> Self {
		Self {
			addr,
			count,
			handoff: false,
		}
	}

	/// Bind the group and steer it.
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
	/// This process serves the first generation and takes every new handshake.
	///
	/// An ephemeral port (`0`) is resolved first, with a plain bind that is
	/// dropped again, and the group then takes that port like a named one.
	/// Never a reuseport bind of port `0`: Linux counts a port held only by
	/// same-UID reuseport sockets as free for one, so the kernel could hand
	/// back a port another group holds and this one would silently join it. A
	/// plain bind conflicts with every holder. Another socket taking the port
	/// before the first member binds fails that bind with
	/// [`io::ErrorKind::AddrInUse`].
	pub fn bind(self) -> Result<Bound, Error> {
		let count = self.count.max(1);
		let generations = match self.handoff {
			true => 2,
			false => 1,
		};
		let total = count.saturating_mul(generations);
		if u32::from(count) * u32::from(generations) > u32::from(MAX_SHARDS) {
			return Err(Error::Count {
				count: total,
				max: MAX_SHARDS,
			});
		}

		let addr = match self.addr.port() {
			0 => crate::bind::udp(crate::bind::Udp::new(self.addr))
				.and_then(|socket| socket.local_addr())
				.map_err(Error::Resolve)?,
			_ => self.addr,
		};

		let port = addr.port();
		let lock = Lock::acquire(port).map_err(|_| Error::Overlap { port })?;

		let mut slots = Vec::with_capacity(usize::from(total));
		for index in 0..total {
			slots.push(member(addr, index).map_err(|source| Error::Bind { index, source })?);
		}

		let group = Bound {
			slots,
			count,
			generation: 0,
			next: 0,
			addr,
			_lock: lock,
		};
		// The last member to bind is the first point the filter covers the whole array.
		group.steer(Steer::Alone).map_err(Error::Steer)?;
		Ok(group)
	}
}

/// Which connections the newest generation receives, set by [`Bound::steer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steer {
	/// Another generation still serves its connections: every connection ID a
	/// server chose reaches the member that issued it.
	Beside,
	/// No other generation serves: every packet reaches this generation, and one
	/// for a connection the other generation issued is refused by its connection
	/// ID generator rather than queued on a socket nobody reads.
	Alone,
}

/// A complete steered group that retains every member socket.
///
/// [`member`](Self::member) returns duplicate handles to the retained sockets.
/// Dropping one handle stops serving that slot without removing it from the
/// kernel group or renumbering any survivor. Dropping this group closes every
/// retained member together and releases the port, unless a successor that
/// [`adopt`](Self::adopt)ed it still holds them.
#[derive(Debug)]
pub struct Bound {
	/// Every member in kernel order: one generation of `count`, or two with a handoff.
	slots: Vec<UdpSocket>,
	/// Members per generation, which is how many this process serves.
	count: u16,
	/// The generation this process serves.
	generation: u16,
	next: u16,
	addr: SocketAddr,

	/// Held for exactly as long as the complete socket group, and released by
	/// the kernel with the process. `None` on a host with no lock directory.
	_lock: Option<Lock>,
}

impl Bound {
	/// How many sockets this process serves.
	pub fn count(&self) -> u16 {
		self.count
	}

	/// The address every member holds, with an ephemeral port already resolved.
	pub fn addr(&self) -> SocketAddr {
		self.addr
	}

	/// How many generations the group holds members for: two when bound for a handoff.
	fn generations(&self) -> u16 {
		u16::try_from(self.slots.len() / usize::from(self.count)).expect("at most 256 members")
	}

	/// The next socket to serve, or `None` once every member was handed out.
	///
	/// The group retains the underlying socket, so dropping the returned handle
	/// cannot resize the kernel's reuseport array.
	pub fn member(&mut self) -> io::Result<Option<Socket>> {
		if self.next >= self.count {
			return Ok(None);
		}
		let index = self.generation * self.count + self.next;
		let socket = self.slots[usize::from(index)].try_clone()?;
		let shard = Shard {
			index,
			count: u16::try_from(self.slots.len()).expect("at most 256 members"),
		};
		let worker = self.next;
		self.next += 1;
		Ok(Some(Socket { shard, worker, socket }))
	}

	/// Make this process's generation the one every new handshake reaches.
	///
	/// [`Config::bind`] steers its group already. A successor calls this once it
	/// serves, with [`Steer::Beside`] while its predecessor still serves and
	/// [`Steer::Alone`] once it is gone. The filter belongs to the whole group, so
	/// the latest call from either process wins.
	pub fn steer(&self, steer: Steer) -> io::Result<()> {
		let first = self.slots.first().expect("a group always has at least one member");
		attach(
			first,
			Program {
				count: self.count,
				total: u16::try_from(self.slots.len()).expect("at most 256 members"),
				base: self.generation * self.count,
				exact: steer == Steer::Beside,
			},
		)
	}

	/// Duplicate every member and the port lock for a successor to [`adopt`](Self::adopt).
	///
	/// Refused with [`io::ErrorKind::Unsupported`] for a group bound without
	/// [`Config::handoff`], which has no generation to give away.
	#[cfg(target_os = "linux")]
	pub fn handoff(&self) -> io::Result<Handoff> {
		if self.generations() < 2 {
			return Err(io::Error::new(
				io::ErrorKind::Unsupported,
				"the reuseport group was bound without room for a successor",
			));
		}
		let sockets = self
			.slots
			.iter()
			.map(|socket| socket.try_clone().map(OwnedFd::from))
			.collect::<io::Result<_>>()?;
		let lock = self
			._lock
			.as_ref()
			.map(|lock| lock._file.try_clone().map(OwnedFd::from))
			.transpose()?;
		Ok(Handoff {
			addr: self.addr,
			count: self.count,
			generation: self.generation,
			sockets,
			lock,
		})
	}

	/// Serve the generation a predecessor's [`handoff`](Self::handoff) left free.
	///
	/// Nothing is steered here: the predecessor keeps every new handshake until
	/// this process is ready to serve and calls [`steer`](Self::steer).
	#[cfg(target_os = "linux")]
	pub fn adopt(handoff: Handoff) -> Result<Self, Error> {
		let Handoff {
			addr,
			count,
			generation,
			sockets,
			lock,
		} = handoff;
		let refuse = |reason: String| Err(Error::Adopt(reason));
		if count == 0 || u32::from(count) * 2 > u32::from(MAX_SHARDS) {
			return refuse(format!("{count} members per generation"));
		}
		if generation > 1 {
			return refuse(format!("generation {generation} of two"));
		}
		if sockets.len() != usize::from(count) * 2 {
			return refuse(format!(
				"{} sockets for two generations of {count}",
				sockets.len()
			));
		}
		let mut slots = Vec::with_capacity(sockets.len());
		for (index, socket) in sockets.into_iter().enumerate() {
			let socket = UdpSocket::from(socket);
			match socket.local_addr() {
				Ok(local) if local == addr => slots.push(socket),
				Ok(local) => return refuse(format!("member {index} is bound to {local}, not {addr}")),
				Err(err) => return refuse(format!("member {index} is not a bound UDP socket: {err}")),
			}
		}
		Ok(Self {
			slots,
			count,
			generation: 1 - generation,
			next: 0,
			addr,
			_lock: lock.map(|file| Lock {
				_file: std::fs::File::from(file),
			}),
		})
	}
}

/// A steered group on its way to a successor process: every member in kernel
/// order and the port lock, from [`Bound::handoff`] for [`Bound::adopt`].
///
/// Carries file descriptors, so send it over a Unix socket with
/// [`crate::fd::send`] and rebuild it from what [`crate::fd::recv`] returns.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Handoff {
	/// The address every member holds.
	pub addr: SocketAddr,
	/// Members per generation.
	pub count: u16,
	/// The generation the sender serves; the successor takes the other one.
	pub generation: u16,
	/// Every member, two generations of `count`, in the order the kernel numbers them.
	pub sockets: Vec<OwnedFd>,
	/// The lock holding the port against a second group, if the sender had one.
	pub lock: Option<OwnedFd>,
}

/// One serving handle released by a complete [`Bound`] group.
#[derive(Debug)]
pub struct Socket {
	shard: Shard,
	worker: u16,
	socket: UdpSocket,
}

impl Socket {
	/// The slot this socket holds in the group, which its connection IDs name.
	pub fn shard(&self) -> Shard {
		self.shard
	}

	/// This socket's position among the ones its process serves, from zero.
	pub fn worker(&self) -> u16 {
		self.worker
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

/// The largest group the steering filter can address, both generations counted.
///
/// It selects with one byte of the connection ID, so a member past 255 could
/// never be named: the filter's `byte % count` could never return that index.
pub const MAX_SHARDS: u16 = 256;

/// What the steering filter computes for one group.
#[derive(Clone, Copy, Debug)]
struct Program {
	/// Members per generation.
	count: u16,
	/// Members in the group, every generation counted.
	total: u16,
	/// The first member of the generation that takes new handshakes.
	base: u16,
	/// Steer a server-chosen connection ID to the member that issued it, rather than
	/// to the same position in the newest generation.
	exact: bool,
}

/// Attach the steering filter to the reuseport group `socket` belongs to.
///
/// Applies to the group, so once is enough, and replaces whatever filter the
/// group had: a successor retargets new handshakes this way.
#[cfg(target_os = "linux")]
fn attach(socket: &UdpSocket, program: Program) -> io::Result<()> {
	use std::os::fd::AsRawFd;

	let insns = program.insns();
	let fprog = libc::sock_fprog {
		len: insns.len() as u16,
		filter: insns.as_ptr() as *mut libc::sock_filter,
	};

	// SAFETY: `fprog` points at `insns`, which outlives the call, and its `len`
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

	tracing::debug!(?program, "steering the reuseport group by connection ID");
	Ok(())
}

#[cfg(not(target_os = "linux"))]
fn attach(_socket: &UdpSocket, _program: Program) -> io::Result<()> {
	// Unreachable in practice: a member only gets here once `bind::udp` accepted
	// `SO_REUSEPORT`, which is Linux-only.
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"reuseport steering is Linux-only",
	))
}

impl Program {
	/// The classic-BPF program that maps a QUIC packet to its member's index.
	///
	/// A long header is QUIC when it carries v1 or v2. A short header is QUIC when
	/// its fixed bit and relay CID marker are set. A short header and a Handshake
	/// packet carry a connection ID the server chose, so its first byte names a
	/// member: modulo the group size when [`exact`](Self::exact), else modulo
	/// one generation's size, offset into the newest. An Initial or 0-RTT packet
	/// carries the one the client invented, which names nothing, so it always
	/// goes to the newest generation, the same member for every retransmission.
	/// Everything else (another protocol sharing the port, or a QUIC version this
	/// cannot read) hashes its source address and port into the newest
	/// generation, so one flow stays on one member and never lands on a
	/// generation nobody reads. The kernel's own 4-tuple hash would pick from
	/// every member, the reserve generation included. The packet starts at
	/// offset 0 because the kernel pulls the UDP header off before running
	/// this; the addresses are read back through the network header.
	///
	/// A load past the end of the datagram aborts the program with 0, which
	/// reuseport takes as member 0, so anything too short for every load is
	/// rejected first. Jumps only go forward, which is why the two reductions
	/// are spelled out twice.
	#[cfg(target_os = "linux")]
	fn insns(self) -> [libc::sock_filter; 42] {
		/// Long-header form bit in the first byte of a QUIC packet.
		const LONG_HEADER: u32 = 0x80;
		/// Fixed bit on short headers. Greasing is disabled on sharded endpoints.
		const FIXED_BIT: u32 = 0x40;
		/// The long header's packet type bits.
		const TYPE_MASK: u32 = 0x30;
		/// QUIC v1's Handshake packet type (RFC 9000).
		const HANDSHAKE_V1: u32 = 0x20;
		/// QUIC v2's Handshake packet type (RFC 9369).
		const HANDSHAKE_V2: u32 = 0x30;
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
		/// Loads at this offset read from the IP header rather than the payload.
		const NET: u32 = libc::SKF_NET_OFF as u32;
		/// Where the source address sits in an IPv4 header.
		const V4_SRC: u32 = 12;
		/// The last word of the source address in an IPv6 header.
		const V6_SRC: u32 = 20;
		/// The fixed IPv6 header, which a UDP header follows on every flow worth balancing.
		const V6_LEN: u32 = 40;

		fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
			libc::sock_filter {
				code: code as u16,
				jt,
				jf,
				k,
			}
		}

		let count = u32::from(self.count);
		let base = u32::from(self.base);
		// A server-chosen ID either names its exact member, or the member at the same
		// position in the newest generation.
		let (server_mod, server_add) = match self.exact {
			true => (u32::from(self.total), 0),
			false => (count, base),
		};

		[
			// 0: A = datagram length; too short to be QUIC is hashed as a flow.
			insn(libc::BPF_LD | libc::BPF_W | libc::BPF_LEN, 0, 0, 0),
			insn(libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K, 0, 25, MIN_LEN),
			// 2: A = packet[0]
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, 0),
			// Long header? Jump to its version check at 10.
			insn(libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K, 6, 0, LONG_HEADER),
			// 4: A short header must retain QUIC's fixed bit.
			insn(libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K, 0, 22, FIXED_BIT),
			// 5: A = packet[4], the fourth destination CID byte.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, SHORT_DCID + 3),
			insn(libc::BPF_ALU | libc::BPF_AND | libc::BPF_K, 0, 0, MAGIC_MASK),
			// 7: A short header without the relay CID marker is hashed as a flow.
			insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 0, 19, MAGIC_MASK),
			// 8: A = packet[1], then the server-chosen reduction at 20.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, SHORT_DCID),
			insn(libc::BPF_JMP | libc::BPF_JA, 0, 0, 10),
			// 10: Long headers steer only supported QUIC versions.
			insn(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 1),
			insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 4, 0, VERSION_1),
			insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 0, 14, VERSION_2),
			// 13: v2: a Handshake loads the server-chosen ID at 19, else the client's at 23.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, 0),
			insn(libc::BPF_ALU | libc::BPF_AND | libc::BPF_K, 0, 0, TYPE_MASK),
			insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 3, 7, HANDSHAKE_V2),
			// 16: v1: the same split by v1's type.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, 0),
			insn(libc::BPF_ALU | libc::BPF_AND | libc::BPF_K, 0, 0, TYPE_MASK),
			insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 0, 4, HANDSHAKE_V1),
			// 19: A = packet[6], a Handshake's first destination CID byte.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, LONG_DCID),
			// 20: A server-chosen ID names its member.
			insn(libc::BPF_ALU | libc::BPF_MOD | libc::BPF_K, 0, 0, server_mod),
			insn(libc::BPF_ALU | libc::BPF_ADD | libc::BPF_K, 0, 0, server_add),
			insn(libc::BPF_RET | libc::BPF_A, 0, 0, 0),
			// 23: A = packet[6], the ID the client invented, into the newest generation.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, LONG_DCID),
			insn(libc::BPF_ALU | libc::BPF_MOD | libc::BPF_K, 0, 0, count),
			insn(libc::BPF_ALU | libc::BPF_ADD | libc::BPF_K, 0, 0, base),
			insn(libc::BPF_RET | libc::BPF_A, 0, 0, 0),
			// 27: Not QUIC: hash the flow into the newest generation, by IP version.
			insn(libc::BPF_LD | libc::BPF_B | libc::BPF_ABS, 0, 0, NET),
			insn(libc::BPF_ALU | libc::BPF_RSH | libc::BPF_K, 0, 0, 4),
			insn(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 5, 0, 6),
			// 30: IPv4: X = the header length, A = the UDP source port, X = A, A = the source address.
			insn(libc::BPF_LDX | libc::BPF_B | libc::BPF_MSH, 0, 0, NET),
			insn(libc::BPF_LD | libc::BPF_H | libc::BPF_IND, 0, 0, NET),
			insn(libc::BPF_MISC | libc::BPF_TAX, 0, 0, 0),
			insn(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, NET + V4_SRC),
			insn(libc::BPF_JMP | libc::BPF_JA, 0, 0, 3),
			// 35: IPv6: the same past the fixed header.
			insn(libc::BPF_LD | libc::BPF_H | libc::BPF_ABS, 0, 0, NET + V6_LEN),
			insn(libc::BPF_MISC | libc::BPF_TAX, 0, 0, 0),
			insn(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, NET + V6_SRC),
			// 38: A = (address ^ port) % count + base.
			insn(libc::BPF_ALU | libc::BPF_XOR | libc::BPF_X, 0, 0, 0),
			insn(libc::BPF_ALU | libc::BPF_MOD | libc::BPF_K, 0, 0, count),
			insn(libc::BPF_ALU | libc::BPF_ADD | libc::BPF_K, 0, 0, base),
			insn(libc::BPF_RET | libc::BPF_A, 0, 0, 0),
		]
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The length of the connection IDs a sharded endpoint issues.
	#[cfg(target_os = "linux")]
	const CID_LEN: usize = 8;

	/// A connection ID shaped like a sharded endpoint's (see [the
	/// encoding](super#the-encoding)): the first byte randomized within the
	/// member's stride, so nothing but the reduction can be steering, and the
	/// non-SRT marker set.
	#[cfg(target_os = "linux")]
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

	/// Which member received something, given every send targets exactly one.
	#[cfg(target_os = "linux")]
	fn receiver(group: &[UdpSocket]) -> Option<usize> {
		use std::os::fd::AsRawFd;

		// Block until a member is readable: the filter runs on the send, but the
		// datagram lands in the member's queue asynchronously.
		let mut fds: Vec<_> = group
			.iter()
			.map(|socket| libc::pollfd {
				fd: socket.as_raw_fd(),
				events: libc::POLLIN,
				revents: 0,
			})
			.collect();
		let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 5_000) };
		assert!(n >= 0, "poll failed: {}", std::io::Error::last_os_error());
		let mut buf = [0u8; 64];
		group.iter().position(|socket| socket.recv_from(&mut buf).is_ok())
	}

	/// Send `packet` to `addr` from a fresh source port, so nothing but its
	/// connection ID can be steering it.
	#[cfg(target_os = "linux")]
	fn send(packet: &[u8], addr: SocketAddr) {
		let sender = crate::bind::udp(crate::bind::Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
		sender.send_to(packet, addr).unwrap();
	}

	/// A short header carrying `id`.
	#[cfg(target_os = "linux")]
	fn short(id: [u8; CID_LEN]) -> Vec<u8> {
		let mut packet = vec![0x40];
		packet.extend_from_slice(&id);
		packet
	}

	/// A long header of `first` byte and `version`, addressed to `id`.
	#[cfg(target_os = "linux")]
	fn long(first: u8, version: [u8; 4], id: [u8; CID_LEN]) -> Vec<u8> {
		let mut packet = vec![first, version[0], version[1], version[2], version[3], CID_LEN as u8];
		packet.extend_from_slice(&id);
		packet
	}

	const V1: [u8; 4] = [0, 0, 0, 1];
	const V2: [u8; 4] = [0x6b, 0x33, 0x43, 0xcf];

	/// Every packet a client sends with a connection ID the server chose.
	#[cfg(target_os = "linux")]
	fn chosen(id: [u8; CID_LEN]) -> [(&'static str, Vec<u8>); 3] {
		[
			("short", short(id)),
			("v1 handshake", long(0xe0, V1, id)),
			("v2 handshake", long(0xf0, V2, id)),
		]
	}

	/// Every packet a client sends with the connection ID it invented.
	#[cfg(target_os = "linux")]
	fn invented(id: [u8; CID_LEN]) -> [(&'static str, Vec<u8>); 4] {
		[
			("v1 initial", long(0xc0, V1, id)),
			("v1 0-rtt", long(0xd0, V1, id)),
			("v2 initial", long(0xd0, V2, id)),
			("v2 0-rtt", long(0xe0, V2, id)),
		]
	}

	/// Serve every member of `group`, non-blocking.
	#[cfg(target_os = "linux")]
	fn serve(group: &mut Bound) -> (Vec<UdpSocket>, Vec<Shard>) {
		let mut sockets = Vec::new();
		let mut shards = Vec::new();
		while let Some(member) = group.member().expect("clone retained socket") {
			assert_eq!(usize::from(member.worker()), sockets.len(), "workers count from zero");
			shards.push(member.shard());
			let socket = member.into_inner();
			socket.set_nonblocking(true).unwrap();
			sockets.push(socket);
		}
		(sockets, shards)
	}

	/// The whole point, end to end against a real kernel: a packet carrying a
	/// member's connection ID has to reach that member and no other, and a client's
	/// retransmitted Initial has to reach the same member as its first.
	///
	/// This is what the 4-tuple hash cannot do. Every packet here is sent from a
	/// different ephemeral source port, so under the default hashing the landing
	/// socket would be unrelated to the connection ID.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_connection_id_reaches_its_own_member() {
		const COUNT: u16 = 4;

		let mut group = bind("127.0.0.1:0".parse().unwrap(), COUNT).unwrap();
		let (sockets, shards) = serve(&mut group);
		let addr = group.addr();

		for (index, shard) in shards.into_iter().enumerate() {
			let id = cid(shard);
			for (form, packet) in chosen(id).into_iter().chain(invented(id)) {
				send(&packet, addr);
				assert_eq!(
					receiver(&sockets),
					Some(index),
					"{form} for member {index} landed on the wrong socket"
				);
			}
		}
	}

	/// Two processes serving one port through a handoff, against a real kernel:
	/// each keeps its own connections, the newest takes every new handshake, and
	/// the predecessor leaving renumbers nothing.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_successor_serves_beside_its_predecessor() {
		const COUNT: u16 = 3;

		let mut config = Config::new("127.0.0.1:0".parse().unwrap(), COUNT);
		config.handoff = true;
		let mut old = config.bind().unwrap();
		let addr = old.addr();
		let (old_sockets, old_shards) = serve(&mut old);
		assert_eq!(old.count(), COUNT, "a process serves one generation");
		assert_eq!(
			old_shards,
			(0..COUNT).map(|index| Shard { index, count: COUNT * 2 }).collect::<Vec<_>>(),
			"the first process serves the first generation, named against both"
		);

		// Until the successor steers, it receives nothing at all.
		let mut new = Bound::adopt(old.handoff().unwrap()).unwrap();
		let (new_sockets, new_shards) = serve(&mut new);
		assert_eq!(
			new_shards,
			(COUNT..COUNT * 2)
				.map(|index| Shard { index, count: COUNT * 2 })
				.collect::<Vec<_>>(),
			"the successor serves the generation its predecessor left free"
		);
		let invented_id: [u8; CID_LEN] = rand::random();
		for (form, packet) in invented(invented_id) {
			send(&packet, addr);
			assert!(receiver(&old_sockets).is_some(), "a {form} skipped the serving generation");
		}

		new.steer(Steer::Beside).unwrap();
		for (generation, sockets, shards) in [(0, &old_sockets, &old_shards), (1, &new_sockets, &new_shards)] {
			for (index, shard) in shards.iter().enumerate() {
				for (form, packet) in chosen(cid(*shard)) {
					send(&packet, addr);
					assert_eq!(
						receiver(sockets),
						Some(index),
						"a {form} for generation {generation} member {index} left its member"
					);
				}
			}
		}
		for _ in 0..16 {
			let id: [u8; CID_LEN] = rand::random();
			for (form, packet) in invented(id) {
				send(&packet, addr);
				let first = receiver(&new_sockets);
				assert!(first.is_some(), "a new {form} reached the predecessor");
				// A retransmission repeats the ID it invented, so it reaches the same member.
				send(&packet, addr);
				assert_eq!(receiver(&new_sockets), first, "a retransmitted {form} changed members");
			}
		}

		// The predecessor exits. Its sockets stay open in the successor, so nothing
		// renumbers, and once alone the successor takes its stale packets too.
		drop(old_sockets);
		drop(old);
		new.steer(Steer::Alone).unwrap();
		for (index, shard) in new_shards.iter().enumerate() {
			for (form, packet) in chosen(cid(*shard)) {
				send(&packet, addr);
				assert_eq!(receiver(&new_sockets), Some(index), "a {form} left member {index}");
			}
			// The predecessor's member at the same position.
			let stale = Shard {
				index: shard.index() - COUNT,
				count: shard.count(),
			};
			for (form, packet) in chosen(cid(stale)) {
				send(&packet, addr);
				assert_eq!(
					receiver(&new_sockets),
					Some(index),
					"a stale {form} for member {index} was not folded onto its position"
				);
			}
		}
	}

	/// A group bound without room for a successor has none to give, and a
	/// handoff that does not describe its sockets is refused.
	#[test]
	#[cfg(target_os = "linux")]
	fn a_handoff_needs_a_second_generation() {
		let group = bind("127.0.0.1:0".parse().unwrap(), 2).unwrap();
		assert_eq!(group.handoff().unwrap_err().kind(), io::ErrorKind::Unsupported);

		let mut config = Config::new("127.0.0.1:0".parse().unwrap(), 2);
		config.handoff = true;
		let group = config.bind().unwrap();
		let mut handoff = group.handoff().unwrap();
		handoff.sockets.pop();
		assert!(matches!(Bound::adopt(handoff), Err(Error::Adopt(_))));

		let mut handoff = group.handoff().unwrap();
		handoff.addr.set_port(handoff.addr.port().wrapping_add(1));
		assert!(matches!(Bound::adopt(handoff), Err(Error::Adopt(_))));

		// Two generations must both fit the byte the filter reads.
		let mut config = Config::new("127.0.0.1:0".parse().unwrap(), MAX_SHARDS / 2 + 1);
		config.handoff = true;
		assert!(matches!(config.bind(), Err(Error::Count { max: MAX_SHARDS, .. })));
	}

	/// Datagram protocols that share the port are balanced by flow, even when
	/// fields at the QUIC connection-ID offsets change or are missing, and only
	/// across the generation taking new handshakes: a member nobody reads would
	/// swallow the flow.
	#[test]
	#[cfg(target_os = "linux")]
	fn non_quic_flows_stay_on_one_member() {
		use std::net::IpAddr;

		const COUNT: u16 = 4;

		/// Send each protocol's flow to `addr` and require that every packet of it,
		/// truncated or not, lands on one member of `serving`, among `group`.
		fn flows(addr: SocketAddr, group: &[UdpSocket], serving: std::ops::Range<usize>) {
			let flows = [
				("rtp", [0x80, 96, 0, 1, 0, 0, 0, 1, 0x12, 0x34, 0x56, 0x78]),
				// SRT data with KK=0b10 (no encryption); R is toggled below.
				("srt", [0x40, 0, 0, 1, 0x10, 0, 0, 1, 0x12, 0x34, 0x56, 0x78]),
				("stun", [0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4]),
				("dtls", [22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
			];
			let local = SocketAddr::new(addr.ip(), 0);
			for (name, mut packet) in flows {
				// A flow hashed off the generation's first member, so a filter that
				// faults to its first index shows up. Every flow lands in the generation.
				let (sender, owner) = (0..64)
					.find_map(|_| {
						let sender = crate::bind::udp(crate::bind::Udp::new(local)).unwrap();
						sender.send_to(&packet, addr).unwrap();
						let owner = receiver(group).expect("one member receives the flow");
						assert!(serving.contains(&owner), "{name} reached member {owner}, outside {serving:?}");
						(owner != serving.start).then_some((sender, owner))
					})
					.expect("a flow hashed off the first member");

				for sequence in 2..18u8 {
					packet[3] = sequence;
					if name == "srt" {
						packet[4] ^= 0x04; // retransmission flag
					}
					sender.send_to(&packet, addr).unwrap();
					assert_eq!(receiver(group), Some(owner), "{name} packet {sequence} changed members");
				}

				// Too short to reach the connection-ID offsets the filter reads.
				sender.send_to(&packet[..2], addr).unwrap();
				assert_eq!(receiver(group), Some(owner), "a truncated {name} packet changed members");
			}

			// Flows spread across the generation: 64 flows on one member of four is
			// a 4^-63 chance, not a balance.
			let owners: std::collections::HashSet<_> = (0..64)
				.map(|_| {
					let sender = crate::bind::udp(crate::bind::Udp::new(local)).unwrap();
					sender.send_to(&flows[2].1, addr).unwrap();
					receiver(group).expect("one member receives the flow")
				})
				.collect();
			assert!(owners.len() > 1, "every flow reached one member: {owners:?}");
		}

		for host in [IpAddr::from([127, 0, 0, 1]), IpAddr::from(std::net::Ipv6Addr::LOCALHOST)] {
			let mut config = Config::new(SocketAddr::new(host, 0), COUNT);
			config.handoff = true;
			let old = config.bind().unwrap();
			let addr = old.addr();
			// Every member, the unserved generation included, so a stray lands somewhere seen.
			let group: Vec<UdpSocket> = old
				.handoff()
				.unwrap()
				.sockets
				.into_iter()
				.map(|fd| {
					let socket = UdpSocket::from(fd);
					socket.set_nonblocking(true).unwrap();
					socket
				})
				.collect();

			flows(addr, &group, 0..usize::from(COUNT));

			let new = Bound::adopt(old.handoff().unwrap()).unwrap();
			new.steer(Steer::Beside).unwrap();
			flows(addr, &group, usize::from(COUNT)..usize::from(COUNT) * 2);
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
		/// Where a jump at `at` lands when taken (`jt`) or not (`jf`).
		fn target(at: usize, offset: u8) -> usize {
			at + 1 + usize::from(offset)
		}

		let program = Program {
			count: 4,
			total: 8,
			base: 4,
			exact: true,
		}
		.insns();
		const HASH: usize = 27;
		assert_eq!(program[1].k, 7, "every load must stay inside the datagram");
		assert_eq!(target(1, program[1].jf), HASH, "a short datagram is hashed as a flow");
		assert_eq!(target(3, program[3].jt), 10, "long header must land on the version load");
		assert_eq!(target(3, program[3].jf), 4, "short header must fall through");
		assert_eq!(target(4, program[4].jf), HASH, "no fixed bit is hashed as a flow");
		assert_eq!(program[5].k, 4, "short header reads the fourth CID byte");
		assert_eq!(target(7, program[7].jf), HASH, "no marker is hashed as a flow");
		assert_eq!(program[8].k, 1, "short header reads the shard byte");
		assert_eq!(target(9, program[9].k as u8), 20, "short header takes the server reduction");
		assert_eq!(program[10].k, 1, "long header reads its version");
		assert_eq!(target(11, program[11].jt), 16, "v1 checks v1's packet types");
		assert_eq!(target(12, program[12].jt), 13, "v2 checks v2's packet types");
		assert_eq!(target(12, program[12].jf), HASH, "another version is hashed as a flow");
		for (at, handshake) in [(15, 0x30), (18, 0x20)] {
			assert_eq!(program[at].k, handshake, "the Handshake type");
			assert_eq!(target(at, program[at].jt), 19, "a Handshake reads the server's ID");
			assert_eq!(target(at, program[at].jf), 23, "anything else reads the client's ID");
		}
		assert_eq!(program[19].k, 6, "a Handshake reads past version and length");
		assert_eq!(program[23].k, 6, "an Initial reads past version and length");
		assert_eq!((program[20].k, program[21].k), (8, 0), "beside, a server's ID names its member");
		assert_eq!((program[24].k, program[25].k), (4, 4), "a client's ID reaches the newest generation");
		assert_eq!(program[HASH].k, libc::SKF_NET_OFF as u32, "a flow reads its IP version");
		assert_eq!(target(29, program[29].jt), 35, "IPv6 reads past its fixed header");
		assert_eq!(target(29, program[29].jf), 30, "IPv4 reads its header length");
		assert_eq!(target(34, program[34].k as u8), 38, "IPv4 joins the flow reduction");
		assert_eq!((program[39].k, program[40].k), (4, 4), "a flow reaches the newest generation");

		let alone = Program {
			count: 4,
			total: 8,
			base: 4,
			exact: false,
		}
		.insns();
		assert_eq!((alone[20].k, alone[21].k), (4, 4), "alone, a server's ID folds onto the newest");
	}
}
