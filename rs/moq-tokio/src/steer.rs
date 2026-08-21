//! Steering a `SO_REUSEPORT` group by QUIC connection ID.
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
//! once, in order, and never resized. [`crate::worker::Workers`] is what
//! guarantees that, which is why [`crate::listen::Shard`] cannot be minted from
//! outside this crate.

use std::io;
use std::net::{SocketAddr, UdpSocket};

use crate::listen::Shard;

/// Bind this member's socket into the group, steering the whole group once the
/// last member has joined.
///
/// Call it for every member in index order and nothing else in between: the
/// kernel identifies a member by its position in the group, which is the order
/// the sockets bound. An unsharded bind is the plain one.
pub(crate) fn bind(addr: SocketAddr, shard: Option<Shard>) -> io::Result<UdpSocket> {
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
	if shard.is_some_and(|shard| shard.index() == 0) {
		drop(crate::bind::udp(crate::bind::Udp::new(addr))?);
	}

	let options = crate::bind::Udp::new(addr).with_reuse_port(shard.is_some());
	let socket = crate::bind::udp(options)?;

	// The filter covers the group, not the socket, so it goes on once everyone is
	// in. Attaching earlier would steer by an index range that is still growing,
	// and the members that joined later would be unreachable until it was redone.
	if let Some(shard) = shard.filter(|shard| shard.index() + 1 == shard.count()) {
		attach(&socket, shard.count())?;
	}

	Ok(socket)
}

/// Holds a listen port for one worker group's lifetime.
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
/// The file lives in a directory only this UID can write ([`dir`]), which is
/// what makes the lock immune to other users: a reuseport group only admits
/// same-UID sockets, so same-UID is exactly the set that must be excluded,
/// and no other user can touch the lock to deny it. When no such directory
/// can be had, the lock is skipped with a warning rather than refusing to
/// start, and the probe carries the remaining risk.
pub(crate) struct Lock {
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
	pub(crate) fn acquire(port: u16) -> io::Result<Option<Self>> {
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
					tracing::warn!(?path, %err, "cannot open the lock file; worker overlap detection falls back to the bind probe");
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
						"the lock file is not exclusively ours; worker overlap detection falls back to the bind probe"
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
			tracing::warn!(?path, %err, "cannot lock the lock file; worker overlap detection falls back to the bind probe");
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
			"the lock directory's parent cannot protect it; worker overlap detection falls back to the bind probe"
		);
		return None;
	}

	let created = std::fs::DirBuilder::new().mode(0o700).create(&dir);
	if let Err(err) = &created
		&& err.kind() != io::ErrorKind::AlreadyExists
	{
		tracing::warn!(?dir, %err, "cannot create a lock directory; worker overlap detection falls back to the bind probe");
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
			"lock directory is not exclusively ours; worker overlap detection falls back to the bind probe"
		);
		return None;
	}
	Some(dir)
}

/// The first byte of a connection ID issued by `shard`.
///
/// `count` values of the byte are reserved per member, so this keeps
/// `256 / count` of its randomness: 32 values across 8 members, and the other 19
/// bytes of the ID stay fully random either way. Connection IDs are not secrets,
/// but they are unlinkable only while they look random, which is why this spends
/// as little of that byte as it can.
/// The largest group the steering filter can address.
///
/// It selects with one byte of the connection ID, so a member past 255 could
/// never be named: [`cid_prefix`] would have no stride to spend and the filter's
/// `byte % count` could never return that index.
pub(crate) const MAX_SHARDS: u16 = 256;

pub(crate) fn cid_prefix(shard: Shard) -> u8 {
	use rand::RngExt;

	let count = u32::from(shard.count());
	// The number of whole strides of `count` that fit in a byte. At least one,
	// because `Workers::bind` refuses a group larger than `MAX_SHARDS`.
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
	// Unreachable in practice: a shard only exists once `bind::udp` accepted
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

		// Bound in index order, because the kernel identifies a member by its
		// position in the group.
		let mut group = Vec::new();
		let mut addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		for index in 0..COUNT {
			let shard = Shard::new(index, COUNT).unwrap();
			let socket = bind(addr, Some(shard)).unwrap();
			addr = socket.local_addr().unwrap();
			socket.set_nonblocking(true).unwrap();
			group.push(socket);
		}

		/// Which member received something, given every send targets exactly one.
		fn receiver(group: &[UdpSocket]) -> Option<usize> {
			let mut buf = [0u8; 64];
			group.iter().position(|socket| socket.recv_from(&mut buf).is_ok())
		}

		for index in 0..COUNT {
			let prefix = cid_prefix(Shard::new(index, COUNT).unwrap());

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
					receiver(&group),
					Some(usize::from(index)),
					"{form} header for member {index} landed on the wrong socket"
				);
			}
		}
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
