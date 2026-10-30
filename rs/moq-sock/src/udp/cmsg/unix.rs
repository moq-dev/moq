use std::ffi::{c_int, c_uchar};

use super::{CMsgHdr, MsgHdr};

#[derive(Copy, Clone)]
#[repr(align(8))] // Conservative bound for align_of<libc::cmsghdr>
pub(crate) struct Aligned<T>(pub(crate) T);

/// Helpers for [`libc::msghdr`]
impl MsgHdr for libc::msghdr {
	type ControlMessage = libc::cmsghdr;

	fn cmsg_first_hdr(&self) -> *mut Self::ControlMessage {
		unsafe { libc::CMSG_FIRSTHDR(self) }
	}

	unsafe fn cmsg_nxt_hdr(&self, cmsg: *const Self::ControlMessage) -> *mut Self::ControlMessage {
		unsafe { libc::CMSG_NXTHDR(self, cmsg) }
	}

	fn set_control_len(&mut self, len: usize) {
		self.msg_controllen = len as _;
		if len == 0 {
			// netbsd is particular about this being a NULL pointer if there are no control
			// messages.
			self.msg_control = std::ptr::null_mut();
		}
	}

	fn control_len(&self) -> usize {
		self.msg_controllen as _
	}
}

/// Helpers for [`libc::cmsghdr`]
impl CMsgHdr for libc::cmsghdr {
	fn cmsg_len(length: usize) -> usize {
		unsafe { libc::CMSG_LEN(length as _) as usize }
	}

	fn cmsg_space(length: usize) -> usize {
		unsafe { libc::CMSG_SPACE(length as _) as usize }
	}

	unsafe fn cmsg_data(this: *const Self) -> *mut c_uchar {
		unsafe { libc::CMSG_DATA(this) }
	}

	fn set(&mut self, level: c_int, ty: c_int, len: usize) {
		self.cmsg_level = level as _;
		self.cmsg_type = ty as _;
		self.cmsg_len = len as _;
	}

	fn len(&self) -> usize {
		self.cmsg_len as _
	}
}

/// `CMSG_SPACE` of a `len`-byte payload.
const fn cmsg_space(len: usize) -> usize {
	// SAFETY: a pure size computation.
	unsafe { libc::CMSG_SPACE(len as _) as usize }
}

/// Space for every control message one datagram can carry: the send set (ECN, GSO segment size,
/// and source address) and the larger receive set `UdpSocketState::new` enables, sized for
/// whichever family needs more.
///
/// An IPv4 datagram on a dual-stack socket carries the IPv6 packet info and the IPv4 TOS, which
/// the IPv6 set covers.
pub(crate) const LEN: usize = {
	// IP_TOS (or IP_RECVTOS) and IPV6_TCLASS, budgeted as the wider `c_int`.
	let ecn = cmsg_space(size_of::<c_int>());

	#[cfg(any(target_os = "linux", target_os = "android"))]
	let v4_dst = cmsg_space(size_of::<libc::in_pktinfo>());
	#[cfg(any(bsd, apple, solarish))]
	let v4_dst = cmsg_space(size_of::<libc::in_addr>());
	#[cfg(not(any(target_os = "linux", target_os = "android", bsd, apple, solarish)))]
	let v4_dst = 0;

	#[cfg(not(any(target_os = "redox", target_os = "hurd")))]
	let v6_dst = cmsg_space(size_of::<libc::in6_pktinfo>());
	#[cfg(any(target_os = "redox", target_os = "hurd"))]
	let v6_dst = 0;

	// UDP_GRO's segment size and SO_TIMESTAMPNS's receive time, for either family.
	#[cfg(any(target_os = "linux", target_os = "android"))]
	let shared = cmsg_space(size_of::<c_int>()) + cmsg_space(size_of::<libc::timespec>());
	#[cfg(not(any(target_os = "linux", target_os = "android")))]
	let shared = 0;

	let v4 = v4_dst + ecn;
	let v6 = v6_dst + ecn;
	shared + if v4 > v6 { v4 } else { v6 }
};
// Linux on 64-bit: the IPv4 set needs 112 bytes and the IPv6 one 120.
#[cfg(all(any(target_os = "linux", target_os = "android"), target_pointer_width = "64"))]
const _: () = assert!(LEN == 120);
