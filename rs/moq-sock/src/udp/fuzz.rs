//! Test support, enabled by the `fuzz` feature: the `sock_cmsg` fuzz target body for the
//! receive control-message walk, its seeds, and the control-buffer builder that moq-uring's
//! `uring_cmsg` target shares.
//!
//! The walk is the unsafe code a received datagram reaches: `cmsg::Iter` steps through the
//! kernel's control buffer by the lengths written in it, and each payload is read as a C struct.
//! An input describes well-formed control messages, laid out as the kernel lays them, optionally
//! followed by a malformed header and arbitrary bytes. The walk must read exactly what a byte-level
//! reference model reads from the well-formed messages, whether or not the malformed tail follows.
//! The body lives here rather than in `fuzz/fuzz_targets/` because the walk is private, and so the
//! tests below replay it on stable and under Miri.
//!
//! It tracks the crate's internals, so it can change in any release. See `fuzz/README.md` for the
//! workflow.

use std::{
	ffi::c_int,
	mem::{self, MaybeUninit},
	net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
	ptr,
	time::Duration,
};

use super::{EcnCodepoint, RecvMeta, TransportError, cmsg, imp::decode_recv, linux::LinuxError};

/// One fuzz target body.
pub type Target = fn(&[u8]);

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[("sock_cmsg", sock_cmsg)];

/// The control messages an input names by index; anything else is spelled out in full.
const KINDS: &[(c_int, c_int)] = &[
	(libc::SOL_UDP, libc::UDP_GRO),
	(libc::SOL_SOCKET, libc::SCM_TIMESTAMPNS),
	(libc::IPPROTO_IP, libc::IP_TOS),
	(libc::IPPROTO_IP, libc::IP_RECVTOS),
	(libc::IPPROTO_IP, libc::IP_PKTINFO),
	(libc::IPPROTO_IPV6, libc::IPV6_TCLASS),
	(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO),
	(libc::IPPROTO_IP, libc::IP_RECVERR),
	(libc::IPPROTO_IPV6, libc::IPV6_RECVERR),
];

/// The input's opcodes after the indexed kinds.
const RAW: u8 = KINDS.len() as u8;
const SHORT: u8 = RAW + 1;
const LONG: u8 = RAW + 2;
const TRUNCATED: u8 = RAW + 3;
const OPS: u8 = RAW + 4;

/// `CMSG_LEN(0)`, the header every message starts with.
fn header_len() -> usize {
	// SAFETY: a pure size computation.
	unsafe { libc::CMSG_LEN(0) as usize }
}

/// One well-formed control message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
	/// `cmsg_level`.
	pub level: c_int,
	/// `cmsg_type`.
	pub kind: c_int,
	/// The payload, whatever its length.
	pub payload: Vec<u8>,
}

impl Message {
	/// A message carrying `value`'s bytes: a padding-free C struct or integer, as the kernel
	/// writes. Private, since a safe caller could pass a padded type.
	fn new<T: Copy>(level: c_int, kind: c_int, value: T) -> Self {
		Self {
			level,
			kind,
			payload: raw(value),
		}
	}
}

/// How a control buffer ends after its well-formed messages.
#[derive(Clone, Copy, Debug)]
pub enum End {
	/// Nothing follows.
	Clean,
	/// A header whose length is shorter than a header, `n % CMSG_LEN(0)`.
	Short(u8),
	/// A header whose length runs `n + 1` bytes past the buffer, or `usize::MAX` for 255.
	Long(u8),
	/// Fewer bytes than a header.
	Truncated,
}

/// A control buffer decoded from a fuzz input.
pub struct Control {
	/// The well-formed messages, in order.
	pub messages: Vec<Message>,
	/// The kernel's layout of `messages`: each header and payload, padded to `CMSG_SPACE`.
	pub prefix: Vec<u8>,
	/// `prefix` followed by whatever the input's [`End`] appends.
	pub full: Vec<u8>,
}

impl Control {
	/// Decode an input: messages until an end opcode or the input runs out.
	///
	/// A message is an opcode below the length of the `KINDS` table, or `RAW` followed by its level and type
	/// as native `i32`s, then a payload length byte and the payload, zero-padded if the input
	/// runs short. `SHORT` and `LONG` take a length parameter and a kind index for the malformed
	/// header, then use the rest of the input as the bytes after it. `TRUNCATED` appends the rest,
	/// cut short of a header.
	pub fn parse(data: &[u8]) -> Self {
		let mut input = Input(data);
		let mut messages = Vec::new();
		let mut prefix = Vec::new();
		let mut tail = Vec::new();

		while let Some(op) = input.byte() {
			let (level, kind) = match op % OPS {
				RAW => (input.int(), input.int()),
				SHORT | LONG => {
					let param = input.byte().unwrap_or(0);
					let (level, kind) = KINDS[input.byte().unwrap_or(0) as usize % KINDS.len()];
					let junk = input.rest();
					let len = if op % OPS == SHORT {
						param as usize % header_len()
					} else if param == u8::MAX {
						usize::MAX
					} else {
						header_len() + junk.len() + 1 + param as usize
					};
					tail = header(len, level, kind);
					tail.extend_from_slice(junk);
					break;
				}
				TRUNCATED => {
					let junk = input.rest();
					tail = junk[..junk.len().min(header_len() - 1)].to_vec();
					break;
				}
				index => KINDS[index as usize],
			};
			let len = input.byte().unwrap_or(0) as usize;
			let mut payload = input.take(len).to_vec();
			payload.resize(len, 0);
			let message = Message { level, kind, payload };
			layout(&message, &mut prefix);
			messages.push(message);
		}

		let mut full = prefix.clone();
		full.extend_from_slice(&tail);
		Self { messages, prefix, full }
	}

	/// Encode `messages` and `end` as an input [`Control::parse`] reads back, with `junk` after a
	/// malformed header or truncated to fit under one.
	pub fn input(messages: &[Message], end: End, junk: &[u8]) -> Vec<u8> {
		let mut out = Vec::new();
		for message in messages {
			match KINDS.iter().position(|&kind| kind == (message.level, message.kind)) {
				Some(index) => out.push(index as u8),
				None => {
					out.push(RAW);
					out.extend_from_slice(&message.level.to_ne_bytes());
					out.extend_from_slice(&message.kind.to_ne_bytes());
				}
			}
			out.push(u8::try_from(message.payload.len()).expect("a seed payload fits a length byte"));
			out.extend_from_slice(&message.payload);
		}
		match end {
			End::Clean => return out,
			End::Short(param) => out.extend_from_slice(&[SHORT, param, 0]),
			End::Long(param) => out.extend_from_slice(&[LONG, param, 0]),
			End::Truncated => out.push(TRUNCATED),
		}
		out.extend_from_slice(junk);
		out
	}

	/// Inputs for every target that walks a control buffer: the receive sets a socket enables,
	/// the error queue's entries, and payloads one byte short, each followed by every [`End`].
	pub fn seeds() -> Vec<Vec<u8>> {
		let gro = Message::new::<c_int>(libc::SOL_UDP, libc::UDP_GRO, 1200);
		let stamp = Message::new(
			libc::SOL_SOCKET,
			libc::SCM_TIMESTAMPNS,
			libc::timespec {
				tv_sec: 1_700_000_000,
				tv_nsec: 123_456_789,
			},
		);
		let v4 = Message::new(
			libc::IPPROTO_IP,
			libc::IP_PKTINFO,
			libc::in_pktinfo {
				ipi_ifindex: 2,
				ipi_spec_dst: libc::in_addr {
					s_addr: u32::from_ne_bytes([192, 0, 2, 1]),
				},
				ipi_addr: libc::in_addr {
					s_addr: u32::from_ne_bytes([192, 0, 2, 2]),
				},
			},
		);
		let v6 = |ip: Ipv6Addr| {
			Message::new(
				libc::IPPROTO_IPV6,
				libc::IPV6_PKTINFO,
				libc::in6_pktinfo {
					ipi6_addr: libc::in6_addr { s6_addr: ip.octets() },
					ipi6_ifindex: 2,
				},
			)
		};
		let tos = Message::new(libc::IPPROTO_IP, libc::IP_TOS, 0b10u8);
		let tclass = Message::new::<c_int>(libc::IPPROTO_IPV6, libc::IPV6_TCLASS, 0b11);
		let error = |kind, origin, offender: &[u8]| {
			let mut message = Message::new(
				libc::IPPROTO_IP,
				kind,
				libc::sock_extended_err {
					ee_errno: libc::EMSGSIZE as u32,
					ee_origin: origin,
					ee_type: 3,
					ee_code: 4,
					ee_pad: 0,
					ee_info: 1400,
					ee_data: 0,
				},
			);
			message.payload.extend_from_slice(offender);
			message
		};
		let sin = sockaddr(SocketAddr::from(([198, 51, 100, 7], 4443)));
		let sin6 = sockaddr(SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7), 4443)));
		let unspec = (libc::AF_UNSPEC as libc::sa_family_t).to_ne_bytes();
		let short = |message: &Message| Message {
			payload: message.payload[..message.payload.len() - 1].to_vec(),
			..message.clone()
		};

		let sets = [
			vec![gro.clone(), stamp.clone(), v4.clone(), tos.clone()],
			vec![
				gro.clone(),
				stamp.clone(),
				v6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)),
				tclass.clone(),
			],
			vec![v6(Ipv4Addr::new(192, 0, 2, 2).to_ipv6_mapped()), tos.clone()],
			vec![error(libc::IP_RECVERR, libc::SO_EE_ORIGIN_ICMP, &sin)],
			vec![error(libc::IPV6_RECVERR, libc::SO_EE_ORIGIN_ICMP6, &sin6)],
			vec![error(libc::IP_RECVERR, libc::SO_EE_ORIGIN_LOCAL, &unspec)],
			[&gro, &stamp, &v4, &tclass].map(short).to_vec(),
		];
		let ends = [
			End::Clean,
			End::Short(0),
			End::Short(u8::MAX),
			End::Long(0),
			End::Long(u8::MAX),
			End::Truncated,
		];
		let junk = gro.payload.repeat(8);
		sets.iter()
			.flat_map(|set| ends.map(|end| Self::input(set, end, &junk)))
			.collect()
	}
}

/// A cursor over an input that reads zeros once it runs out.
struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
	fn byte(&mut self) -> Option<u8> {
		let (&first, rest) = self.0.split_first()?;
		self.0 = rest;
		Some(first)
	}

	fn take(&mut self, len: usize) -> &'a [u8] {
		let (head, rest) = self.0.split_at(len.min(self.0.len()));
		self.0 = rest;
		head
	}

	fn int(&mut self) -> c_int {
		let mut bytes = [0; 4];
		let head = self.take(4);
		bytes[..head.len()].copy_from_slice(head);
		c_int::from_ne_bytes(bytes)
	}

	fn rest(&mut self) -> &'a [u8] {
		self.take(usize::MAX)
	}
}

/// A `cmsghdr`'s bytes.
fn header(len: usize, level: c_int, kind: c_int) -> Vec<u8> {
	// SAFETY: all-zero is a valid `cmsghdr`.
	let mut header: libc::cmsghdr = unsafe { mem::zeroed() };
	header.cmsg_len = len as _;
	header.cmsg_level = level;
	header.cmsg_type = kind;
	// `cmsghdr` is plain integers with no padding on 64-bit Linux, as the assertion below checks.
	raw(header)
}

const _: () = assert!(size_of::<libc::cmsghdr>() == 16);

/// Append `message` as the kernel writes it: the header, the payload, and padding to
/// `CMSG_SPACE`.
fn layout(message: &Message, out: &mut Vec<u8>) {
	let start = out.len();
	// SAFETY: pure size computations.
	let (len, space) = unsafe {
		(
			libc::CMSG_LEN(message.payload.len() as _) as usize,
			libc::CMSG_SPACE(message.payload.len() as _) as usize,
		)
	};
	out.extend_from_slice(&header(len, message.level, message.kind));
	out.extend_from_slice(&message.payload);
	out.resize(start + space, 0);
}

/// A socket address's kernel bytes.
fn sockaddr(addr: SocketAddr) -> Vec<u8> {
	match addr {
		SocketAddr::V4(v4) => raw(libc::sockaddr_in {
			sin_family: libc::AF_INET as libc::sa_family_t,
			sin_port: v4.port().to_be(),
			sin_addr: libc::in_addr {
				s_addr: u32::from_ne_bytes(v4.ip().octets()),
			},
			sin_zero: [0; 8],
		}),
		SocketAddr::V6(v6) => raw(libc::sockaddr_in6 {
			sin6_family: libc::AF_INET6 as libc::sa_family_t,
			sin6_port: v6.port().to_be(),
			sin6_flowinfo: v6.flowinfo(),
			sin6_addr: libc::in6_addr {
				s6_addr: v6.ip().octets(),
			},
			sin6_scope_id: v6.scope_id(),
		}),
	}
}

/// A plain C value's bytes.
///
/// Only for the padding-free structs and integers the kernel writes into control messages:
/// padding bytes would be uninitialized.
fn raw<T: Copy>(value: T) -> Vec<u8> {
	// SAFETY: `value` is a live `T`, readable for its size, with no padding per the contract above.
	unsafe { std::slice::from_raw_parts(ptr::from_ref(&value).cast::<u8>(), size_of::<T>()) }.to_vec()
}

/// The datagram length every walk reports, which is also the stride without `UDP_GRO`.
const DATAGRAM: usize = 1500;

/// The control-message walk reads what the reference model reads from the well-formed messages,
/// with or without the malformed tail after them, and never reports a zero stride.
pub fn sock_cmsg(data: &[u8]) {
	let control = Control::parse(data);
	let expected = Expected::model(&control.messages);
	for buffer in [&control.prefix, &control.full] {
		let actual = Expected::walk(buffer);
		assert_eq!(
			actual,
			expected,
			"{} messages, {} bytes",
			control.messages.len(),
			buffer.len()
		);
		assert!(actual.stride > 0, "a zero stride never ends a GRO split");
	}
}

/// What a receive and an error-queue read report.
#[derive(Debug, PartialEq)]
struct Expected {
	ecn: Option<EcnCodepoint>,
	dst_ip: Option<IpAddr>,
	interface_index: Option<u32>,
	stride: usize,
	timestamp: Option<Duration>,
	/// The first error-queue entry: its offender and errno.
	error: Option<(Option<SocketAddr>, i32)>,
}

impl Expected {
	/// Walk `control` as a receive and as an error-queue read.
	fn walk(control: &[u8]) -> Self {
		// The kernel's buffer is aligned for `cmsghdr`; a `u64` array is too.
		let mut buffer = vec![0u64; control.len().div_ceil(8)];
		// SAFETY: `buffer` holds at least `control.len()` bytes.
		unsafe { ptr::copy_nonoverlapping(control.as_ptr(), buffer.as_mut_ptr().cast::<u8>(), control.len()) };
		// SAFETY: all-zero is a valid `msghdr`.
		let mut hdr: libc::msghdr = unsafe { mem::zeroed() };
		hdr.msg_control = buffer.as_mut_ptr().cast();
		hdr.msg_controllen = control.len() as _;

		let mut name = MaybeUninit::<libc::sockaddr_storage>::zeroed();
		// SAFETY: `sockaddr_in` fits in `sockaddr_storage`.
		unsafe {
			name.as_mut_ptr().cast::<libc::sockaddr_in>().write(libc::sockaddr_in {
				sin_family: libc::AF_INET as libc::sa_family_t,
				sin_port: 4443u16.to_be(),
				sin_addr: libc::in_addr { s_addr: 0 },
				sin_zero: [0; 8],
			})
		};
		let meta: RecvMeta = decode_recv(&name, &hdr, 0, DATAGRAM).expect("the name is an IPv4 address");

		// SAFETY: `hdr` points at `buffer`, aligned and initialized for its whole length.
		let error = unsafe { cmsg::Iter::new(&hdr) }
			.find_map(LinuxError::decode)
			.map(TransportError::from)
			.map(|error| (error.addr, error.raw_errno));

		Self {
			ecn: meta.ecn,
			dst_ip: meta.dst_ip,
			interface_index: meta.interface_index,
			stride: meta.stride,
			timestamp: meta.timestamp,
			error,
		}
	}

	/// What each message means, read from its bytes rather than through the C structs, applied
	/// in order; a payload too short for its type is skipped.
	fn model(messages: &[Message]) -> Self {
		let mut expected = Self {
			ecn: None,
			dst_ip: None,
			interface_index: None,
			stride: DATAGRAM,
			timestamp: None,
			error: None,
		};
		for Message { level, kind, payload } in messages {
			let p = payload.as_slice();
			match (*level, *kind) {
				(libc::IPPROTO_IP, libc::IP_TOS | libc::IP_RECVTOS) if !p.is_empty() => {
					expected.ecn = EcnCodepoint::from_bits(p[0]);
				}
				(libc::IPPROTO_IPV6, libc::IPV6_TCLASS) if p.len() >= 4 => {
					expected.ecn = EcnCodepoint::from_bits(i32::from_ne_bytes(bytes(p, 0)) as u8);
				}
				// in_pktinfo: ifindex, the route's preferred source, then the destination.
				(libc::IPPROTO_IP, libc::IP_PKTINFO) if p.len() >= 12 => {
					expected.dst_ip = Some(Ipv4Addr::from(bytes::<4>(p, 8)).into());
					expected.interface_index = Some(i32::from_ne_bytes(bytes(p, 0)) as u32);
				}
				// in6_pktinfo: the destination, then ifindex.
				(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO) if p.len() >= 20 => {
					expected.dst_ip = Some(Ipv6Addr::from(bytes::<16>(p, 0)).into());
					expected.interface_index = Some(u32::from_ne_bytes(bytes(p, 16)));
				}
				(libc::SOL_UDP, libc::UDP_GRO) if p.len() >= 4 => {
					let stride = i32::from_ne_bytes(bytes(p, 0));
					if stride > 0 {
						expected.stride = stride as usize;
					}
				}
				(libc::SOL_SOCKET, libc::SCM_TIMESTAMPNS) if p.len() >= 16 => {
					let secs = u64::try_from(i64::from_ne_bytes(bytes(p, 0))).unwrap_or(0);
					let nanos = u32::try_from(i64::from_ne_bytes(bytes(p, 8))).unwrap_or(0);
					expected.timestamp = Some(Duration::new(secs, nanos));
				}
				_ => {}
			}
			if expected.error.is_none() {
				expected.error = error(*level, *kind, p);
			}
		}
		expected
	}
}

/// An error-queue entry: a 16-byte `sock_extended_err`, then the offender's address.
fn error(level: c_int, kind: c_int, p: &[u8]) -> Option<(Option<SocketAddr>, i32)> {
	if !matches!(level, libc::IPPROTO_IP | libc::IPPROTO_IPV6) || !matches!(kind, libc::IP_RECVERR | libc::IPV6_RECVERR)
	{
		return None;
	}
	if p.len() < 18 {
		return None;
	}
	let errno = u32::from_ne_bytes(bytes(p, 0)) as i32;
	let offender = match c_int::from(u16::from_ne_bytes(bytes(p, 16))) {
		libc::AF_UNSPEC => None,
		libc::AF_INET if p.len() >= 16 + 16 => Some(SocketAddr::V4(SocketAddrV4::new(
			Ipv4Addr::from(bytes::<4>(p, 20)),
			u16::from_be_bytes(bytes(p, 18)),
		))),
		libc::AF_INET6 if p.len() >= 16 + 28 => Some(SocketAddr::V6(SocketAddrV6::new(
			Ipv6Addr::from(bytes::<16>(p, 24)),
			u16::from_be_bytes(bytes(p, 18)),
			u32::from_ne_bytes(bytes(p, 20)),
			u32::from_ne_bytes(bytes(p, 40)),
		))),
		_ => return None,
	};
	Some((offender, errno))
}

/// `N` bytes of `p` from `at`.
fn bytes<const N: usize>(p: &[u8], at: usize) -> [u8; N] {
	p[at..at + N].try_into().expect("the caller checked the length")
}

/// One input to start a target from.
pub struct Seed {
	/// The target it feeds, a key of [`TARGETS`].
	pub target: &'static str,
	/// The input.
	pub data: Vec<u8>,
}

/// The starting corpus, from [`Control::seeds`].
pub fn seeds() -> Vec<Seed> {
	Control::seeds()
		.into_iter()
		.map(|data| Seed {
			target: "sock_cmsg",
			data,
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every seed holds a message and passes, so the fuzzer starts from real receives. Runs under
	/// Miri, which checks the walk's pointer arithmetic on each.
	#[test]
	fn seeds_pass() {
		for (index, seed) in seeds().iter().enumerate() {
			assert!(!Control::parse(&seed.data).messages.is_empty(), "seed {index} is empty");
			sock_cmsg(&seed.data);
		}
	}

	/// The model reads the seeds' receive sets as the socket tests expect.
	#[test]
	fn model_reads_the_seeds() {
		let control = Control::parse(&seeds()[0].data);
		let expected = Expected::model(&control.messages);
		assert_eq!(expected.stride, 1200);
		assert_eq!(expected.ecn, Some(EcnCodepoint::Ect0));
		assert_eq!(expected.dst_ip, Some(Ipv4Addr::new(192, 0, 2, 2).into()));
		assert_eq!(expected.interface_index, Some(2));
		assert_eq!(expected.timestamp, Some(Duration::new(1_700_000_000, 123_456_789)));
	}

	/// A zero `UDP_GRO` size keeps the datagram's own length as its stride.
	#[test]
	fn zero_gro_keeps_the_stride() {
		let gro = Message::new::<c_int>(libc::SOL_UDP, libc::UDP_GRO, 0);
		sock_cmsg(&Control::input(&[gro], End::Clean, &[]));
	}

	/// Replay the inputs committed under `fuzz/regressions/`, so a bug the fuzzer found stays
	/// fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	#[cfg_attr(miri, ignore = "reads the regression files, which Miri's isolation refuses")]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			// Nothing has failed yet for this target; `fuzz/README.md` says what to drop in here
			// when something does.
			let Ok(entries) = std::fs::read_dir(root.join(name)) else {
				continue;
			};
			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				let data = std::fs::read(&path).expect("could not read a regression input");
				target(&data);
			}
		}
	}
}
