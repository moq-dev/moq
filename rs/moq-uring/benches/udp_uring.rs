//! io_uring comparison for UDP receive batching and send GSO on Linux.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
mod linux {
	use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
	use std::fmt;
	use std::io;
	use std::mem::{self};
	use std::net::{Ipv4Addr, UdpSocket};
	use std::os::fd::AsRawFd;
	use std::ptr::{self, NonNull};
	use std::sync::atomic::{AtomicU16, Ordering};
	use std::time::{Duration, Instant};

	use criterion::{BenchmarkId, Criterion, Throughput};
	use io_uring::{IoUring, cqueue, opcode, types};
	use nix::sys::socket::{
		getsockopt, setsockopt,
		sockopt::{RcvBuf, UdpGroSegment},
	};

	const SEGMENT_SIZE: usize = 1280;
	const BURST_SEGMENTS: usize = 32;
	const BURST_BYTES: usize = SEGMENT_SIZE * BURST_SEGMENTS;
	const TOTAL_SEGMENTS: usize = 8 * 1024;
	const TOTAL_BYTES: usize = TOTAL_SEGMENTS * SEGMENT_SIZE;
	const RECEIVE_BUFFER_SIZE: usize = BURST_BYTES * 4;
	const BURST_TIMEOUT: Duration = Duration::from_secs(1);
	const MAX_GRO_SEGMENTS: usize = 64;
	const MAX_RECV_SIZE: usize = SEGMENT_SIZE * MAX_GRO_SEGMENTS;
	const CONTROL_SIZE: usize = 64;
	const PROVIDED_BUFFER_SIZE: usize = MAX_RECV_SIZE + CONTROL_SIZE + 64;
	const PROVIDED_BUFFER_COUNT: usize = 64;
	const BUFFER_GROUP: u16 = 0;
	const RECV_USER_DATA: u64 = 1;
	const SEND_USER_DATA: u64 = 2;

	#[derive(Clone, Copy)]
	struct Config {
		gro: bool,
		gso: bool,
	}

	impl Config {
		fn all() -> impl Iterator<Item = Self> {
			[false, true]
				.into_iter()
				.flat_map(|gso| [false, true].into_iter().map(move |gro| Self { gro, gso }))
		}
	}

	impl fmt::Display for Config {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
			let gro = if self.gro { "gro-on" } else { "gro-off" };
			let gso = if self.gso { "gso-on" } else { "gso-off" };
			write!(f, "recvmsg-multishot_{gro}_{gso}")
		}
	}

	#[derive(Default)]
	struct Received {
		bytes: usize,
		datagrams: usize,
	}

	struct Pair {
		config: Config,
		sender: UdpSocket,
		receiver: UdpSocket,
		ring: Option<IoUring>,
		recv_header: Box<libc::msghdr>,
		send: SendState,
		buffers: ProvidedBuffers,
	}

	impl Pair {
		fn new(config: Config) -> anyhow::Result<Self> {
			let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
			let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
			setsockopt(&receiver, RcvBuf, &RECEIVE_BUFFER_SIZE)?;
			let receive_buffer = getsockopt(&receiver, RcvBuf)?;
			anyhow::ensure!(
				receive_buffer >= RECEIVE_BUFFER_SIZE,
				"kernel capped SO_RCVBUF at {receive_buffer} bytes, below the required {RECEIVE_BUFFER_SIZE}"
			);
			sender.connect(receiver.local_addr()?)?;
			receiver.connect(sender.local_addr()?)?;
			setsockopt(&receiver, UdpGroSegment, &config.gro)?;

			let ring = IoUring::builder()
				.setup_single_issuer()
				.setup_defer_taskrun()
				.build(128)
				.map_err(|err| anyhow::anyhow!("create io_uring: {err}"))?;
			let buffers = ProvidedBuffers::new(&ring)?;
			let mut recv_header = Box::new(unsafe { mem::zeroed::<libc::msghdr>() });
			recv_header.msg_controllen = CONTROL_SIZE;
			let send = SendState::new(config.gso);

			let mut pair = Self {
				config,
				sender,
				receiver,
				ring: Some(ring),
				recv_header,
				send,
				buffers,
			};
			pair.arm_receive()?;
			pair.ring.as_mut().expect("ring is live").submit()?;
			Ok(pair)
		}

		fn transfer(&mut self) -> anyhow::Result<()> {
			for _ in 0..TOTAL_SEGMENTS / BURST_SEGMENTS {
				let mut sends = self.submit_burst()?;
				let mut received = Received::default();
				let deadline = Instant::now() + BURST_TIMEOUT;

				while sends > 0 || received.datagrams < BURST_SEGMENTS {
					let completion = self.next_completion(deadline).map_err(|err| {
						anyhow::anyhow!(
							"UDP burst stalled with {} send completions pending and {} of {BURST_SEGMENTS} datagrams: {err}",
							sends,
							received.datagrams
						)
					})?;
					match completion.user_data {
						SEND_USER_DATA => {
							let expected = if self.config.gso {
								SEGMENT_SIZE * BURST_SEGMENTS
							} else {
								SEGMENT_SIZE
							};
							anyhow::ensure!(
								completion.result == expected as i32,
								"io_uring sendmsg returned {}, expected {expected}",
								completion.result
							);
							sends -= 1;
						}
						RECV_USER_DATA => {
							let next = self.receive(completion)?;
							received.bytes += next.bytes;
							received.datagrams += next.datagrams;
						}
						other => anyhow::bail!("unknown io_uring user data {other}"),
					}
				}

				anyhow::ensure!(
					received.bytes == SEGMENT_SIZE * BURST_SEGMENTS,
					"received {} bytes for a {} byte burst",
					received.bytes,
					SEGMENT_SIZE * BURST_SEGMENTS
				);
				anyhow::ensure!(
					received.datagrams == BURST_SEGMENTS,
					"received {} datagrams for a {BURST_SEGMENTS} datagram burst",
					received.datagrams
				);
			}

			Ok(())
		}

		fn submit_burst(&mut self) -> anyhow::Result<usize> {
			let sends = if self.config.gso { 1 } else { BURST_SEGMENTS };
			let fd = types::Fd(self.sender.as_raw_fd());
			let mut queue = self.ring.as_mut().expect("ring is live").submission();

			for message in &self.send.messages[..sends] {
				let send = opcode::SendMsg::new(fd, &message.header)
					.build()
					.user_data(SEND_USER_DATA);
				unsafe {
					queue
						.push(&send)
						.map_err(|_| anyhow::anyhow!("submission queue is full while sending burst"))?;
				}
			}
			drop(queue);
			self.ring.as_mut().expect("ring is live").submit()?;
			Ok(sends)
		}

		fn next_completion(&mut self, deadline: Instant) -> anyhow::Result<Completion> {
			loop {
				if let Some(completion) = self.ring.as_mut().expect("ring is live").completion().next() {
					return Ok(Completion {
						user_data: completion.user_data(),
						result: completion.result(),
						flags: completion.flags(),
					});
				}

				let remaining = deadline
					.checked_duration_since(Instant::now())
					.ok_or_else(|| anyhow::anyhow!("timed out waiting for an io_uring completion"))?;
				let timeout = types::Timespec::from(remaining);
				let args = types::SubmitArgs::new().timespec(&timeout);
				match self
					.ring
					.as_mut()
					.expect("ring is live")
					.submitter()
					.submit_with_args(1, &args)
				{
					Ok(_) => {}
					Err(err) if err.raw_os_error() == Some(libc::ETIME) => {
						anyhow::bail!("timed out waiting for an io_uring completion")
					}
					Err(err) => return Err(err.into()),
				}
			}
		}

		fn receive(&mut self, completion: Completion) -> anyhow::Result<Received> {
			anyhow::ensure!(
				completion.result >= 0,
				"io_uring multishot recvmsg failed: {}",
				io::Error::from_raw_os_error(-completion.result)
			);
			let buffer_id = cqueue::buffer_select(completion.flags)
				.ok_or_else(|| anyhow::anyhow!("multishot recvmsg completed without a provided buffer"))?;
			let result_len = usize::try_from(completion.result)?;
			let buffer = self.buffers.get(buffer_id, result_len)?;
			let message = types::RecvMsgOut::parse(buffer, &self.recv_header)
				.map_err(|()| anyhow::anyhow!("invalid multishot recvmsg buffer layout"))?;
			anyhow::ensure!(!message.is_payload_truncated(), "truncated UDP payload");
			anyhow::ensure!(!message.is_control_data_truncated(), "truncated UDP control data");

			let bytes = message.payload_data().len();
			let stride = gro_stride(message.control_data())?.unwrap_or(bytes);
			let received = validate_receive(bytes, stride)?;
			self.buffers.recycle(buffer_id);

			if !cqueue::more(completion.flags) {
				self.arm_receive()?;
			}

			Ok(received)
		}

		fn arm_receive(&mut self) -> anyhow::Result<()> {
			let recv = opcode::RecvMsgMulti::new(
				types::Fd(self.receiver.as_raw_fd()),
				self.recv_header.as_ref(),
				BUFFER_GROUP,
			)
			.build()
			.user_data(RECV_USER_DATA);
			unsafe {
				self.ring
					.as_mut()
					.expect("ring is live")
					.submission()
					.push(&recv)
					.map_err(|_| anyhow::anyhow!("submission queue is full while rearming receive"))?;
			}
			Ok(())
		}
	}

	impl Drop for Pair {
		fn drop(&mut self) {
			// Closing the ring cancels the multishot request before its buffers are freed.
			drop(self.ring.take());
		}
	}

	struct Completion {
		user_data: u64,
		result: i32,
		flags: u32,
	}

	struct SendState {
		_payload: Vec<u8>,
		messages: Box<[SendMessage; BURST_SEGMENTS]>,
	}

	impl SendState {
		fn new(gso: bool) -> Self {
			let mut payload = vec![0xab; SEGMENT_SIZE * BURST_SEGMENTS];
			let mut messages = Box::new(std::array::from_fn(|_| SendMessage::new()));

			for (index, message) in messages.iter_mut().enumerate() {
				let (offset, len) = if gso {
					(0, SEGMENT_SIZE * BURST_SEGMENTS)
				} else {
					(index * SEGMENT_SIZE, SEGMENT_SIZE)
				};
				message.iovec.iov_base = payload[offset..].as_mut_ptr().cast();
				message.iovec.iov_len = len;
				message.header.msg_iov = &mut message.iovec;
				message.header.msg_iovlen = 1;

				if gso {
					message.set_gso(SEGMENT_SIZE as u16);
				}
			}

			Self {
				_payload: payload,
				messages,
			}
		}
	}

	struct SendMessage {
		header: libc::msghdr,
		iovec: libc::iovec,
		control: [u8; CONTROL_SIZE],
	}

	impl SendMessage {
		fn new() -> Self {
			Self {
				header: unsafe { mem::zeroed() },
				iovec: libc::iovec {
					iov_base: ptr::null_mut(),
					iov_len: 0,
				},
				control: [0; CONTROL_SIZE],
			}
		}

		fn set_gso(&mut self, segment_size: u16) {
			self.header.msg_control = self.control.as_mut_ptr().cast();
			self.header.msg_controllen = unsafe { libc::CMSG_SPACE(mem::size_of::<u16>() as _) as usize };
			let control = unsafe { libc::CMSG_FIRSTHDR(&self.header) };
			assert!(!control.is_null());
			unsafe {
				(*control).cmsg_level = libc::SOL_UDP;
				(*control).cmsg_type = libc::UDP_SEGMENT;
				(*control).cmsg_len = libc::CMSG_LEN(mem::size_of::<u16>() as _) as usize;
				ptr::write_unaligned(libc::CMSG_DATA(control).cast::<u16>(), segment_size);
			}
		}
	}

	struct ProvidedBuffers {
		ring: NonNull<types::BufRingEntry>,
		layout: Layout,
		buffers: Box<[Vec<u8>; PROVIDED_BUFFER_COUNT]>,
		tail: u16,
	}

	impl ProvidedBuffers {
		fn new(ring: &IoUring) -> anyhow::Result<Self> {
			let layout = Layout::from_size_align(PROVIDED_BUFFER_COUNT * mem::size_of::<types::BufRingEntry>(), 4096)?;
			let pointer = unsafe { alloc_zeroed(layout) };
			let ring_pointer =
				NonNull::new(pointer.cast::<types::BufRingEntry>()).unwrap_or_else(|| handle_alloc_error(layout));
			let buffers = Box::new(std::array::from_fn(|_| vec![0; PROVIDED_BUFFER_SIZE]));

			unsafe {
				ring.submitter().register_buf_ring_with_flags(
					ring_pointer.as_ptr() as u64,
					PROVIDED_BUFFER_COUNT as u16,
					BUFFER_GROUP,
					0,
				)?;
			}

			let mut provided = Self {
				ring: ring_pointer,
				layout,
				buffers,
				tail: 0,
			};
			for id in 0..PROVIDED_BUFFER_COUNT as u16 {
				provided.add(id);
			}
			provided.publish();
			Ok(provided)
		}

		fn get(&self, id: u16, len: usize) -> anyhow::Result<&[u8]> {
			let buffer = self
				.buffers
				.get(id as usize)
				.ok_or_else(|| anyhow::anyhow!("kernel selected invalid buffer {id}"))?;
			anyhow::ensure!(len <= buffer.len(), "kernel reported oversized buffer length {len}");
			Ok(&buffer[..len])
		}

		fn recycle(&mut self, id: u16) {
			self.add(id);
			self.publish();
		}

		fn add(&mut self, id: u16) {
			let mask = PROVIDED_BUFFER_COUNT - 1;
			let index = self.tail as usize & mask;
			let buffer = &mut self.buffers[id as usize];
			let entry = unsafe { &mut *self.ring.as_ptr().add(index) };
			entry.set_addr(buffer.as_mut_ptr() as u64);
			entry.set_len(buffer.len() as u32);
			entry.set_bid(id);
			self.tail = self.tail.wrapping_add(1);
		}

		fn publish(&self) {
			let tail = unsafe { types::BufRingEntry::tail(self.ring.as_ptr()) }.cast_mut();
			unsafe { AtomicU16::from_ptr(tail) }.store(self.tail, Ordering::Release);
		}
	}

	impl Drop for ProvidedBuffers {
		fn drop(&mut self) {
			unsafe { dealloc(self.ring.as_ptr().cast(), self.layout) };
		}
	}

	fn gro_stride(control: &[u8]) -> anyhow::Result<Option<usize>> {
		let mut offset = 0;
		let header_len = unsafe { libc::CMSG_LEN(0) as usize };

		while offset + header_len <= control.len() {
			let header = unsafe { control.as_ptr().add(offset).cast::<libc::cmsghdr>().read_unaligned() };
			let message_len = header.cmsg_len;
			anyhow::ensure!(
				message_len >= header_len,
				"invalid control message length {message_len}"
			);
			anyhow::ensure!(offset + message_len <= control.len(), "truncated control message");

			if header.cmsg_level == libc::SOL_UDP && header.cmsg_type == libc::UDP_GRO {
				anyhow::ensure!(
					message_len >= header_len + mem::size_of::<i32>(),
					"short UDP_GRO control message"
				);
				let value = unsafe { control.as_ptr().add(offset + header_len).cast::<i32>().read_unaligned() };
				return Ok(Some(usize::try_from(value)?));
			}

			let aligned = unsafe { libc::CMSG_SPACE((message_len - header_len) as _) as usize };
			offset = offset.saturating_add(aligned);
		}

		Ok(None)
	}

	fn validate_receive(bytes: usize, stride: usize) -> anyhow::Result<Received> {
		anyhow::ensure!(
			bytes > 0 && stride > 0 && bytes.is_multiple_of(SEGMENT_SIZE) && stride == SEGMENT_SIZE,
			"invalid UDP receive: bytes={bytes}, stride={stride}"
		);
		Ok(Received {
			bytes,
			datagrams: bytes.div_ceil(stride),
		})
	}

	pub fn benchmark(c: &mut Criterion) {
		let mut group = c.benchmark_group("udp_io_uring");
		group.throughput(Throughput::Bytes(TOTAL_BYTES as u64));

		for config in Config::all() {
			let mut pair = Pair::new(config).expect("create io_uring UDP socket pair");
			group.bench_with_input(BenchmarkId::from_parameter(config), &config, |b, _| {
				b.iter_custom(|iterations| {
					let start = Instant::now();
					for _ in 0..iterations {
						pair.transfer().expect("transfer UDP benchmark payload");
					}
					start.elapsed()
				});
			});
		}

		group.finish();
	}
}

#[cfg(target_os = "linux")]
use linux::benchmark;

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark);
criterion_main!(benches);
