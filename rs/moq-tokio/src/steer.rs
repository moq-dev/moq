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
