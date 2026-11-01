//! A process's UDP sockets as the kernel sees them: the queue each was granted,
//! what sits in it, and what it dropped.
//!
//! The socket list comes from `/proc/<pid>/fd`, and the numbers from a
//! `sock_diag` netlink dump, the interface behind `ss -uam`: per socket, the
//! granted `SO_RCVBUF`/`SO_SNDBUF`, the bytes queued, and `sk_drops`, the same
//! counter `SO_RXQ_OVFL` reports. Reading it needs no privilege and never
//! touches the process. The dump sees the sampler's network namespace, so a
//! target in another namespace reports no sockets.
//!
//! The namespace's `RcvbufErrors`, `SndbufErrors` and `InErrors` from the
//! target's `/proc/<pid>/net/snmp{,6}` ride along as a cross-check: they count
//! every socket in the namespace, not just the target's.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use procfs::process::{FDTarget, Process};

/// `SOCK_DIAG_BY_FAMILY` (linux/sock_diag.h).
const SOCK_DIAG_BY_FAMILY: u16 = 20;
/// `NETLINK_SOCK_DIAG` (linux/netlink.h).
const NETLINK_SOCK_DIAG: libc::c_int = 4;
/// `INET_DIAG_SKMEMINFO` (linux/inet_diag.h): the `SK_MEMINFO_*` array.
const INET_DIAG_SKMEMINFO: u16 = 7;
// `SK_MEMINFO_*` slots (linux/sock_diag.h).
const SK_MEMINFO_RMEM_ALLOC: usize = 0;
const SK_MEMINFO_RCVBUF: usize = 1;
const SK_MEMINFO_SNDBUF: usize = 3;
const SK_MEMINFO_DROPS: usize = 8;

/// `struct inet_diag_req_v2`.
#[repr(C)]
struct Request {
	family: u8,
	protocol: u8,
	ext: u8,
	pad: u8,
	states: u32,
	/// `struct inet_diag_sockid`, zero to match every socket.
	id: [u8; 48],
}

/// `struct nlmsghdr` followed by the request.
#[repr(C)]
struct Message {
	len: u32,
	kind: u16,
	flags: u16,
	seq: u32,
	pid: u32,
	request: Request,
}

/// Bytes of `struct inet_diag_msg` before its attributes.
const DIAG_MSG_LEN: usize = 72;
/// Offset of `idiag_inode` within `struct inet_diag_msg`.
const DIAG_INODE: usize = 68;
const NLMSG_HDRLEN: usize = 16;

/// One socket's kernel accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Socket {
	pub rcvbuf: u32,
	pub sndbuf: u32,
	pub queued: u32,
	pub drops: u32,
}

/// Every UDP socket in the namespace, by inode, over IPv4 and IPv6.
pub fn dump() -> io::Result<HashMap<u64, Socket>> {
	let mut sockets = HashMap::new();
	for family in [libc::AF_INET, libc::AF_INET6] {
		dump_family(family as u8, &mut sockets)?;
	}
	Ok(sockets)
}

fn dump_family(family: u8, sockets: &mut HashMap<u64, Socket>) -> io::Result<()> {
	// SAFETY: plain socket creation; the result is checked before use.
	let fd = unsafe {
		libc::socket(
			libc::AF_NETLINK,
			libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
			NETLINK_SOCK_DIAG,
		)
	};
	if fd < 0 {
		return Err(io::Error::last_os_error());
	}
	// SAFETY: `fd` was just created and is owned here alone.
	let fd = unsafe { OwnedFd::from_raw_fd(fd) };

	let message = Message {
		len: size_of::<Message>() as u32,
		kind: SOCK_DIAG_BY_FAMILY,
		flags: (libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16,
		seq: 1,
		pid: 0,
		request: Request {
			family,
			protocol: libc::IPPROTO_UDP as u8,
			ext: 1 << (INET_DIAG_SKMEMINFO - 1),
			pad: 0,
			states: u32::MAX,
			id: [0; 48],
		},
	};
	// SAFETY: `message` is a plain repr(C) value readable for its size.
	let sent = unsafe {
		libc::send(
			fd.as_raw_fd(),
			std::ptr::from_ref(&message).cast(),
			size_of::<Message>(),
			0,
		)
	};
	if sent < 0 {
		return Err(io::Error::last_os_error());
	}

	let mut buf = vec![0u8; 64 * 1024];
	loop {
		// SAFETY: `buf` is writable for its length.
		let len = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
		if len < 0 {
			return Err(io::Error::last_os_error());
		}
		if parse(&buf[..len as usize], sockets)? {
			return Ok(());
		}
	}
}

/// Parse one datagram of the dump into `sockets`; true once the dump is done.
fn parse(mut data: &[u8], sockets: &mut HashMap<u64, Socket>) -> io::Result<bool> {
	let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("sock_diag: {what}"));
	if data.is_empty() {
		return Ok(true);
	}
	while data.len() >= NLMSG_HDRLEN {
		let len = u32::from_ne_bytes(data[0..4].try_into().expect("4 bytes")) as usize;
		let kind = u16::from_ne_bytes(data[4..6].try_into().expect("2 bytes"));
		if len < NLMSG_HDRLEN || len > data.len() {
			return Err(invalid("truncated message"));
		}
		match kind as libc::c_int {
			libc::NLMSG_DONE => return Ok(true),
			libc::NLMSG_ERROR => {
				let code = i32::from_ne_bytes(data[16..20].try_into().map_err(|_| invalid("short error"))?);
				return Err(io::Error::from_raw_os_error(-code));
			}
			_ => {}
		}
		if kind == SOCK_DIAG_BY_FAMILY {
			let body = &data[NLMSG_HDRLEN..len];
			if body.len() < DIAG_MSG_LEN {
				return Err(invalid("short inet_diag_msg"));
			}
			let inode = u32::from_ne_bytes(body[DIAG_INODE..DIAG_INODE + 4].try_into().expect("4 bytes"));
			if let Some(mem) = meminfo(&body[DIAG_MSG_LEN..]) {
				sockets.insert(
					u64::from(inode),
					Socket {
						queued: mem(SK_MEMINFO_RMEM_ALLOC),
						rcvbuf: mem(SK_MEMINFO_RCVBUF),
						sndbuf: mem(SK_MEMINFO_SNDBUF),
						drops: mem(SK_MEMINFO_DROPS),
					},
				);
			}
		}
		let aligned = len.next_multiple_of(4).min(data.len());
		data = &data[aligned..];
	}
	Ok(false)
}

/// The `SK_MEMINFO_*` array among a message's attributes, as a slot reader.
fn meminfo(mut attrs: &[u8]) -> Option<impl Fn(usize) -> u32 + '_> {
	while attrs.len() >= 4 {
		let len = u16::from_ne_bytes(attrs[0..2].try_into().ok()?) as usize;
		let kind = u16::from_ne_bytes(attrs[2..4].try_into().ok()?);
		if len < 4 || len > attrs.len() {
			return None;
		}
		if kind == INET_DIAG_SKMEMINFO {
			let values = &attrs[4..len];
			// Older kernels send fewer slots; a missing one reads as zero.
			return Some(move |slot: usize| {
				values
					.get(slot * 4..slot * 4 + 4)
					.map_or(0, |bytes| u32::from_ne_bytes(bytes.try_into().expect("4 bytes")))
			});
		}
		attrs = &attrs[len.next_multiple_of(4).min(attrs.len())..];
	}
	None
}

/// The socket inodes `process` holds open.
pub fn inodes(process: &Process) -> io::Result<HashSet<u64>> {
	let fds = process.fd().map_err(io::Error::other)?;
	Ok(fds
		.filter_map(Result::ok)
		.filter_map(|fd| match fd.target {
			FDTarget::Socket(inode) => Some(inode),
			_ => None,
		})
		.collect())
}

/// A process's UDP sockets, summed, plus its namespace's UDP error counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
	pub sockets: u64,
	/// The smallest receive buffer any of the sockets was granted, in the
	/// kernel's units (twice what `SO_RCVBUF` asked for on Linux).
	pub rcvbuf_min: u64,
	pub sndbuf_min: u64,
	/// Bytes queued for receive across the sockets, right now.
	pub queued: u64,
	/// Cumulative drops across every socket ever seen, closed ones included.
	pub drops: u64,
}

/// Monotonic drop totals across a process's sockets.
///
/// `sk_drops` is a wrapping 32-bit counter per socket, and a closed socket's
/// counter vanishes with it, so the total folds in each socket's wrapping
/// delta and keeps a closed socket's share, like the context-switch totals.
#[derive(Default)]
pub struct Drops {
	seen: HashMap<u64, u32>,
	total: u64,
}

impl Drops {
	/// Fold the target's sockets from `all` into a sample.
	pub fn observe(&mut self, owned: &HashSet<u64>, all: &HashMap<u64, Socket>) -> Sample {
		let mut sample = Sample {
			rcvbuf_min: u64::MAX,
			sndbuf_min: u64::MAX,
			..Sample::default()
		};
		for inode in owned {
			let Some(socket) = all.get(inode) else { continue };
			let previous = self.seen.insert(*inode, socket.drops).unwrap_or(0);
			self.total += u64::from(socket.drops.wrapping_sub(previous));
			sample.sockets += 1;
			sample.rcvbuf_min = sample.rcvbuf_min.min(u64::from(socket.rcvbuf));
			sample.sndbuf_min = sample.sndbuf_min.min(u64::from(socket.sndbuf));
			sample.queued += u64::from(socket.queued);
		}
		self.seen
			.retain(|inode, _| owned.contains(inode) && all.contains_key(inode));
		if sample.sockets == 0 {
			sample.rcvbuf_min = 0;
			sample.sndbuf_min = 0;
		}
		sample.drops = self.total;
		sample
	}
}

/// UDP error counters for the namespace `process` lives in, IPv4 and IPv6
/// summed: `RcvbufErrors`, `SndbufErrors`, `InErrors`.
pub fn snmp(process: &Process) -> (u64, u64, u64) {
	let root = std::path::Path::new("/proc").join(process.pid.to_string()).join("net");
	let v4 = std::fs::read_to_string(root.join("snmp")).unwrap_or_default();
	let v6 = std::fs::read_to_string(root.join("snmp6")).unwrap_or_default();
	snmp_parse(&v4, &v6)
}

fn snmp_parse(v4: &str, v6: &str) -> (u64, u64, u64) {
	let mut udp = v4.lines().filter(|line| line.starts_with("Udp: "));
	let v4: HashMap<&str, u64> = match (udp.next(), udp.next()) {
		(Some(names), Some(values)) => names
			.split_whitespace()
			.zip(values.split_whitespace())
			.filter_map(|(name, value)| Some((name, value.parse().ok()?)))
			.collect(),
		_ => HashMap::new(),
	};
	let v6: HashMap<&str, u64> = v6
		.lines()
		.filter_map(|line| {
			let mut fields = line.split_whitespace();
			Some((fields.next()?.strip_prefix("Udp6")?, fields.next()?.parse().ok()?))
		})
		.collect();
	let both = |name: &str| v4.get(name).copied().unwrap_or(0) + v6.get(name).copied().unwrap_or(0);
	(both("RcvbufErrors"), both("SndbufErrors"), both("InErrors"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn snmp_sums_both_families() {
		let v4 = "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors\nUdp: 10 0 7 9 5 2\n";
		let v6 = "Udp6InDatagrams 4\nUdp6InErrors 1\nUdp6RcvbufErrors 1\nUdp6SndbufErrors 0\n";
		assert_eq!(snmp_parse(v4, v6), (6, 2, 8));
		assert_eq!(snmp_parse("", ""), (0, 0, 0));
	}

	/// Drops stay monotonic across a 32-bit wrap and a closed socket, and a
	/// socket first seen with drops counts them in full.
	#[test]
	fn drops_survive_wrap_and_close() {
		let mut drops = Drops::default();
		let socket = |drops: u32| Socket {
			rcvbuf: 8,
			sndbuf: 4,
			queued: 1,
			drops,
		};
		let owned: HashSet<u64> = [1, 2].into();
		let sample = drops.observe(
			&owned,
			&[(1, socket(u32::MAX - 1)), (2, socket(3)), (9, socket(100))].into(),
		);
		assert_eq!(sample.sockets, 2);
		assert_eq!(sample.drops, u64::from(u32::MAX - 1) + 3);
		assert_eq!((sample.rcvbuf_min, sample.sndbuf_min, sample.queued), (8, 4, 2));

		// Socket 1 wraps by 4; socket 2 closes and keeps its share.
		let owned: HashSet<u64> = [1].into();
		let sample = drops.observe(&owned, &[(1, socket(2))].into());
		assert_eq!(sample.drops, u64::from(u32::MAX - 1) + 3 + 4);

		// No sockets: zeros, with the total kept.
		let sample = drops.observe(&HashSet::new(), &HashMap::new());
		assert_eq!((sample.sockets, sample.rcvbuf_min), (0, 0));
		assert_eq!(sample.drops, u64::from(u32::MAX - 1) + 7);
	}

	/// The dump reads this process's own socket, granted buffer included.
	#[test]
	fn dump_sees_own_socket() {
		let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
		let process = Process::myself().unwrap();
		let owned = inodes(&process).unwrap();
		let all = dump().unwrap();
		let mine: Vec<_> = owned.iter().filter_map(|inode| all.get(inode)).collect();
		assert!(
			!mine.is_empty(),
			"the dump must include the bound socket: owned {owned:?}, dumped {}",
			all.len()
		);
		assert!(mine.iter().all(|socket| socket.rcvbuf > 0));
		drop(socket);
	}
}
