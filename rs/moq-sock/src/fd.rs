//! Passing open file descriptors between processes over a Unix stream socket.
//!
//! The kernel copies each descriptor sent this way into the receiver, which
//! then holds the same open socket the sender does: a listener or a reuseport
//! member keeps its place, its queue, and its connections. A process hands its
//! sockets to its successor this way (see [`crate::shard::Handoff`]).
//!
//! Both calls work on non-blocking sockets and report
//! [`io::ErrorKind::WouldBlock`], so an async runtime can drive them from its
//! readiness loop.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// The most descriptors one [`send`] may carry: the kernel's `SCM_MAX_FD`.
pub const MAX: usize = 253;

/// Space for one `SCM_RIGHTS` message carrying [`MAX`] descriptors.
const CONTROL: usize = unsafe { libc::CMSG_SPACE((MAX * size_of::<libc::c_int>()) as u32) } as usize;

/// Send `data` with `fds` attached, returning how many bytes went out.
///
/// The descriptors arrive with the first byte the receiver reads of this call,
/// even when the write is partial, so resend the rest without them. At most
/// [`MAX`] descriptors, and at least one byte of `data` to carry them.
pub fn send(socket: BorrowedFd<'_>, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
	if fds.len() > MAX {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("{} descriptors in one message; the kernel takes {MAX}", fds.len()),
		));
	}
	if data.is_empty() && !fds.is_empty() {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			"descriptors need at least one byte to ride on",
		));
	}

	let mut iov = libc::iovec {
		iov_base: data.as_ptr() as *mut libc::c_void,
		iov_len: data.len(),
	};
	let mut control = Control([0; CONTROL]);
	// SAFETY: an all-zero msghdr is a valid empty message.
	let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
	msg.msg_iov = &mut iov;
	msg.msg_iovlen = 1;

	if !fds.is_empty() {
		let len = fds.len() * size_of::<libc::c_int>();
		msg.msg_control = control.0.as_mut_ptr().cast();
		// SAFETY: CMSG_SPACE is a pure size computation.
		msg.msg_controllen = unsafe { libc::CMSG_SPACE(len as u32) } as _;
		// SAFETY: `msg_control` points at `control`, which has room for one
		// header and `fds.len()` descriptors, so the first header and its data
		// are in bounds.
		unsafe {
			let header = libc::CMSG_FIRSTHDR(&msg);
			(*header).cmsg_level = libc::SOL_SOCKET;
			(*header).cmsg_type = libc::SCM_RIGHTS;
			(*header).cmsg_len = libc::CMSG_LEN(len as u32) as _;
			let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
			for (index, fd) in fds.iter().enumerate() {
				data.add(index).write_unaligned(fd.as_raw_fd());
			}
		}
	}

	// SAFETY: `msg` points at `iov` and `control`, both alive for the call.
	let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
	match sent {
		-1 => Err(io::Error::last_os_error()),
		sent => Ok(sent as usize),
	}
}

/// Receive into `buf`, appending any descriptors that arrived with it to `fds`.
///
/// Returns how many bytes were read; zero means the peer closed. Each received
/// descriptor is close-on-exec. A message whose descriptors did not fit is an
/// error, since the kernel has already closed the ones it dropped.
pub fn recv(socket: BorrowedFd<'_>, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
	let mut iov = libc::iovec {
		iov_base: buf.as_mut_ptr().cast(),
		iov_len: buf.len(),
	};
	let mut control = Control([0; CONTROL]);
	// SAFETY: an all-zero msghdr is a valid empty message.
	let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
	msg.msg_iov = &mut iov;
	msg.msg_iovlen = 1;
	msg.msg_control = control.0.as_mut_ptr().cast();
	msg.msg_controllen = CONTROL as _;

	// SAFETY: `msg` points at `buf` and `control`, both alive for the call.
	let read = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
	if read == -1 {
		return Err(io::Error::last_os_error());
	}

	// Take ownership of everything that arrived before judging the message, so an
	// error below still closes them.
	// SAFETY: the kernel filled `msg_control` with `msg_controllen` bytes of
	// well-formed headers, which the CMSG macros walk within those bounds.
	unsafe {
		let mut header = libc::CMSG_FIRSTHDR(&msg);
		while !header.is_null() {
			if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
				let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
				let len = (*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
				for index in 0..len / size_of::<libc::c_int>() {
					fds.push(OwnedFd::from_raw_fd(data.add(index).read_unaligned()));
				}
			}
			header = libc::CMSG_NXTHDR(&msg, header);
		}
	}
	if msg.msg_flags & libc::MSG_CTRUNC != 0 {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"a message carried more descriptors than fit",
		));
	}
	Ok(read as usize)
}

/// A control buffer aligned for `cmsghdr`, as the CMSG macros require.
#[repr(C, align(8))]
struct Control([u8; CONTROL]);

#[cfg(test)]
mod tests {
	use super::*;
	use std::io::{Read, Write};
	use std::os::fd::AsFd;
	use std::os::unix::net::UnixStream;

	/// A descriptor sent across arrives as the same open file: what one side
	/// writes, the other reads.
	#[test]
	fn a_descriptor_arrives_as_the_same_open_file() {
		let (left, right) = UnixStream::pair().unwrap();
		let (mut ours, theirs) = UnixStream::pair().unwrap();
		assert_eq!(send(left.as_fd(), b"x", &[theirs.as_fd()]).unwrap(), 1);
		drop(theirs);

		let mut buf = [0; 8];
		let mut fds = Vec::new();
		assert_eq!(recv(right.as_fd(), &mut buf, &mut fds).unwrap(), 1);
		assert_eq!(&buf[..1], b"x");
		assert_eq!(fds.len(), 1);

		let mut received = UnixStream::from(fds.pop().unwrap());
		received.write_all(b"hello").unwrap();
		let mut got = [0; 5];
		ours.read_exact(&mut got).unwrap();
		assert_eq!(&got, b"hello");
	}

	/// The kernel's limit is enforced before anything is sent, and the most it
	/// allows arrives whole.
	#[test]
	fn the_most_descriptors_arrive_whole() {
		let (left, right) = UnixStream::pair().unwrap();
		let files: Vec<_> = (0..=MAX).map(|_| UnixStream::pair().unwrap().0).collect();
		let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
		assert_eq!(
			send(left.as_fd(), b"x", &borrowed).unwrap_err().kind(),
			io::ErrorKind::InvalidInput
		);
		assert_eq!(send(left.as_fd(), b"x", &borrowed[..MAX]).unwrap(), 1);

		let mut fds = Vec::new();
		recv(right.as_fd(), &mut [0; 1], &mut fds).unwrap();
		assert_eq!(fds.len(), MAX);
	}
}
