//! Test support, enabled by the `fuzz` feature: the `uring_cmsg` fuzz target body for
//! `RecvMeta::parse`, the receive control-message walk, plus its seeds.
//!
//! The walk is the unsafe code a received datagram reaches: it steps through the kernel's control
//! buffer by the lengths written in it and reads each payload as a C struct. Inputs use moq-sock's
//! format ([`Control`]): well-formed control messages laid out as the kernel lays them, optionally
//! followed by a malformed header and arbitrary bytes. The walk must read exactly what a byte-level
//! reference model reads from the well-formed messages, whether or not the malformed tail follows.
//! The body lives here because the walk is private, and so the tests below replay it on stable and
//! under Miri.
//!
//! It tracks the crate's internals, so it can change in any release. See `fuzz/README.md` for the
//! workflow.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use moq_sock::udp::fuzz::{Control, Message};

use super::{Ecn, RecvMeta, Wall};

/// One fuzz target body.
pub type Target = fn(&[u8]);

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[("uring_cmsg", uring_cmsg)];

/// `RecvMeta::parse` reads what the reference model reads from the well-formed messages, with
/// or without the malformed tail after them, and never reports a zero stride.
pub fn uring_cmsg(data: &[u8]) {
	let control = Control::parse(data);
	let expected = Expected::model(&control.messages);
	for buffer in [&control.prefix, &control.full] {
		let RecvMeta {
			stride,
			ecn,
			dst_ip,
			timestamp,
		} = RecvMeta::parse(buffer);
		let actual = Expected {
			stride,
			ecn,
			dst_ip,
			timestamp,
		};
		assert_eq!(
			actual,
			expected,
			"{} messages, {} bytes",
			control.messages.len(),
			buffer.len()
		);
		assert_ne!(stride, Some(0), "a zero stride never ends a GRO split");
	}
}

/// What a receive reports.
#[derive(Debug, PartialEq)]
struct Expected {
	stride: Option<usize>,
	ecn: Option<Ecn>,
	dst_ip: Option<IpAddr>,
	timestamp: Option<Wall>,
}

impl Expected {
	/// What each message means, read from its bytes rather than through the C structs, applied
	/// in order. A recognized message that does not hold a valid value clears its field.
	fn model(messages: &[Message]) -> Self {
		let mut expected = Self {
			stride: None,
			ecn: None,
			dst_ip: None,
			timestamp: None,
		};
		for Message { level, kind, payload } in messages {
			let p = payload.as_slice();
			match (*level, *kind) {
				(libc::SOL_UDP, libc::UDP_GRO) => {
					expected.stride = int(p)
						.and_then(|stride| usize::try_from(stride).ok())
						.filter(|&stride| stride > 0);
				}
				(libc::IPPROTO_IP, libc::IP_TOS) => expected.ecn = p.first().and_then(|&bits| Ecn::from_bits(bits)),
				(libc::IPPROTO_IPV6, libc::IPV6_TCLASS) => {
					expected.ecn = int(p).and_then(|value| Ecn::from_bits(value as u8));
				}
				// in_pktinfo: ifindex, the route's preferred source, then the destination.
				(libc::IPPROTO_IP, libc::IP_PKTINFO) => {
					expected.dst_ip = bytes::<4>(p, 8)
						.filter(|_| p.len() >= 12)
						.map(|ip| Ipv4Addr::from(ip).into());
				}
				// in6_pktinfo: the destination, then ifindex.
				(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO) => {
					expected.dst_ip = bytes::<16>(p, 0)
						.filter(|_| p.len() >= 20)
						.map(|ip| Ipv6Addr::from(ip).into());
				}
				(libc::SOL_SOCKET, libc::SCM_TIMESTAMPNS) => {
					expected.timestamp = (|| {
						let secs = u64::try_from(i64::from_ne_bytes(bytes(p, 0)?)).ok()?;
						let nanos = u32::try_from(i64::from_ne_bytes(bytes(p, 8)?)).ok()?;
						Wall::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
					})();
				}
				_ => {}
			}
		}
		expected
	}
}

/// A payload's leading native `c_int`.
fn int(p: &[u8]) -> Option<i32> {
	bytes(p, 0).map(i32::from_ne_bytes)
}

/// `N` bytes of `p` from `at`, if it holds them.
fn bytes<const N: usize>(p: &[u8], at: usize) -> Option<[u8; N]> {
	p.get(at..at + N)?.try_into().ok()
}

/// One input to start a target from.
pub struct Seed {
	/// The target it feeds, a key of [`TARGETS`].
	pub target: &'static str,
	/// The input.
	pub data: Vec<u8>,
}

/// The starting corpus, moq-sock's receive sets and malformed ends.
pub fn seeds() -> Vec<Seed> {
	Control::seeds()
		.into_iter()
		.map(|data| Seed {
			target: "uring_cmsg",
			data,
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every seed passes. Runs under Miri, which checks the walk's pointer arithmetic on each.
	#[test]
	fn seeds_pass() {
		for seed in seeds() {
			uring_cmsg(&seed.data);
		}
	}

	/// The model reads the seeds' first receive set as the socket tests expect.
	#[test]
	fn model_reads_the_seeds() {
		let control = Control::parse(&seeds()[0].data);
		let expected = Expected::model(&control.messages);
		assert_eq!(expected.stride, Some(1200));
		assert_eq!(expected.ecn, Some(Ecn::Ect0));
		assert_eq!(expected.dst_ip, Some(Ipv4Addr::new(192, 0, 2, 2).into()));
		assert_eq!(
			expected.timestamp,
			Some(Wall::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789))
		);
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
