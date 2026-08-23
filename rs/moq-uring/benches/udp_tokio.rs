//! Tokio/epoll baseline for UDP receive batching and send GSO on Linux.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
mod linux {
	use std::fmt;
	use std::io::{self, IoSliceMut};
	use std::mem;
	use std::net::{Ipv4Addr, UdpSocket as StdUdpSocket};

	use std::os::fd::AsRawFd;
	use std::time::{Duration, Instant};

	use criterion::{BenchmarkId, Criterion, Throughput};
	use nix::sys::socket::{
		ControlMessageOwned, MsgFlags, getsockopt, recvmsg, setsockopt,
		sockopt::{RcvBuf, UdpGroSegment},
	};
	use noq_udp::{RecvMeta, Transmit, UdpSocketState};
	use tokio::io::Interest;
	use tokio::net::UdpSocket;

	const SEGMENT_SIZE: usize = 1280;
	const BURST_SEGMENTS: usize = 32;
	const BURST_BYTES: usize = SEGMENT_SIZE * BURST_SEGMENTS;
	const TOTAL_SEGMENTS: usize = 8 * 1024;
	const TOTAL_BYTES: usize = TOTAL_SEGMENTS * SEGMENT_SIZE;
	const RECEIVE_BUFFER_SIZE: usize = BURST_BYTES * 4;
	const BURST_TIMEOUT: Duration = Duration::from_secs(1);
	const MAX_GRO_SEGMENTS: usize = 64;
	const MAX_RECV_SIZE: usize = SEGMENT_SIZE * MAX_GRO_SEGMENTS;

	#[derive(Clone, Copy)]
	struct Config {
		recvmmsg: bool,
		gro: bool,
		gso: bool,
	}

	impl Config {
		fn all() -> impl Iterator<Item = Self> {
			[false, true].into_iter().flat_map(|gso| {
				[false, true].into_iter().flat_map(move |gro| {
					[false, true]
						.into_iter()
						.map(move |recvmmsg| Self { recvmmsg, gro, gso })
				})
			})
		}
	}

	impl fmt::Display for Config {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
			let receive = if self.recvmmsg { "recvmmsg" } else { "recvmsg" };
			let gro = if self.gro { "gro-on" } else { "gro-off" };
			let gso = if self.gso { "gso-on" } else { "gso-off" };
			write!(f, "{receive}_{gro}_{gso}")
		}
	}

	struct Pair {
		config: Config,
		sender: UdpSocket,
		receiver: UdpSocket,
		send_state: UdpSocketState,
		recv_state: UdpSocketState,
		receiver_addr: std::net::SocketAddr,
		payload: Vec<u8>,
		send_messages: Box<[libc::mmsghdr; BURST_SEGMENTS]>,
		_send_iovecs: Box<[libc::iovec; BURST_SEGMENTS]>,
		single_buffer: Vec<u8>,
		batch_buffers: Box<[Vec<u8>; noq_udp::BATCH_SIZE]>,
		batch_meta: [RecvMeta; noq_udp::BATCH_SIZE],
	}

	#[derive(Default)]
	struct Received {
		bytes: usize,
		datagrams: usize,
	}

	impl Pair {
		fn new(config: Config) -> anyhow::Result<Self> {
			let sender = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
			let receiver = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
			setsockopt(&receiver, RcvBuf, &RECEIVE_BUFFER_SIZE)?;
			let receive_buffer = getsockopt(&receiver, RcvBuf)?;
			anyhow::ensure!(
				receive_buffer >= RECEIVE_BUFFER_SIZE,
				"kernel capped SO_RCVBUF at {receive_buffer} bytes, below the required {RECEIVE_BUFFER_SIZE}"
			);
			let sender_addr = sender.local_addr()?;
			let receiver_addr = receiver.local_addr()?;
			sender.connect(receiver_addr)?;
			receiver.connect(sender_addr)?;
			sender.set_nonblocking(true)?;
			receiver.set_nonblocking(true)?;

			let send_state = UdpSocketState::new((&sender).into())?;
			let recv_state = UdpSocketState::new((&receiver).into())?;
			setsockopt(&receiver, UdpGroSegment, &config.gro)?;

			if config.gso {
				anyhow::ensure!(
					send_state.max_gso_segments().get() >= BURST_SEGMENTS,
					"kernel reports fewer than {BURST_SEGMENTS} UDP GSO segments"
				);
			}
			if config.gro {
				anyhow::ensure!(
					recv_state.gro_segments().get() <= MAX_GRO_SEGMENTS,
					"kernel reports more than {MAX_GRO_SEGMENTS} UDP GRO segments"
				);
			}

			let batch_buffers = (0..noq_udp::BATCH_SIZE)
				.map(|_| vec![0; MAX_RECV_SIZE])
				.collect::<Vec<_>>()
				.into_boxed_slice()
				.try_into()
				.expect("batch buffer count is fixed");

			let payload = vec![0xab; SEGMENT_SIZE * BURST_SEGMENTS];
			let mut send_messages = Box::new(std::array::from_fn(|_| unsafe { mem::zeroed::<libc::mmsghdr>() }));
			let mut send_iovecs = Box::new(std::array::from_fn(|_| libc::iovec {
				iov_base: payload.as_ptr().cast_mut().cast(),
				iov_len: SEGMENT_SIZE,
			}));
			for (message, iovec) in send_messages.iter_mut().zip(send_iovecs.iter_mut()) {
				message.msg_hdr.msg_iov = iovec;
				message.msg_hdr.msg_iovlen = 1;
			}

			Ok(Self {
				config,
				sender: UdpSocket::from_std(sender)?,
				receiver: UdpSocket::from_std(receiver)?,
				send_state,
				recv_state,
				receiver_addr,
				payload,
				send_messages,
				_send_iovecs: send_iovecs,
				single_buffer: vec![0; MAX_RECV_SIZE],
				batch_buffers,
				batch_meta: [RecvMeta::default(); noq_udp::BATCH_SIZE],
			})
		}

		async fn transfer(&mut self) -> anyhow::Result<()> {
			for _ in 0..TOTAL_SEGMENTS / BURST_SEGMENTS {
				self.send_burst().await?;

				let received = tokio::time::timeout(BURST_TIMEOUT, self.receive_burst())
					.await
					.map_err(|_| {
						anyhow::anyhow!("timed out receiving a UDP burst; the kernel may have dropped a datagram")
					})??;

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

		async fn receive_burst(&mut self) -> anyhow::Result<Received> {
			let mut received = Received::default();
			while received.datagrams < BURST_SEGMENTS {
				let next = if self.config.recvmmsg {
					self.recv_many().await?
				} else {
					self.recv_one().await?
				};
				received.bytes += next.bytes;
				received.datagrams += next.datagrams;
			}
			Ok(received)
		}

		async fn send_burst(&mut self) -> io::Result<()> {
			if self.config.gso {
				self.send(&self.payload, Some(SEGMENT_SIZE)).await
			} else {
				self.send_many().await
			}
		}

		async fn send_many(&mut self) -> io::Result<()> {
			let mut sent = 0;
			while sent < self.send_messages.len() {
				self.sender.writable().await?;
				let fd = self.sender.as_raw_fd();
				let messages = &mut self.send_messages[sent..];
				let result = self.sender.try_io(Interest::WRITABLE, || {
					let count =
						unsafe { libc::sendmmsg(fd, messages.as_mut_ptr(), messages.len() as u32, libc::MSG_DONTWAIT) };
					if count < 0 {
						Err(io::Error::last_os_error())
					} else {
						Ok(count as usize)
					}
				});

				match result {
					Ok(count) => sent += count,
					Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
					Err(err) => return Err(err),
				}
			}

			Ok(())
		}

		async fn send(&self, payload: &[u8], segment_size: Option<usize>) -> io::Result<()> {
			let transmit = Transmit {
				destination: self.receiver_addr,
				ecn: None,
				contents: payload,
				segment_size,
				src_ip: None,
			};

			loop {
				self.sender.writable().await?;
				match self.sender.try_io(Interest::WRITABLE, || {
					self.send_state.try_send((&self.sender).into(), &transmit)
				}) {
					Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
					result => return result,
				}
			}
		}

		async fn recv_one(&mut self) -> io::Result<Received> {
			let buffer_len = self.recv_buffer_len();
			loop {
				self.receiver.readable().await?;
				let result = self.receiver.try_io(Interest::READABLE, || {
					let mut iov = [IoSliceMut::new(&mut self.single_buffer[..buffer_len])];
					let mut control = nix::cmsg_space!(nix::libc::in_pktinfo, u8, i32, u32, nix::libc::timespec);
					let message = recvmsg::<()>(
						self.receiver.as_raw_fd(),
						&mut iov,
						Some(&mut control),
						MsgFlags::MSG_DONTWAIT,
					)
					.map_err(io::Error::from)?;
					if message.flags.contains(MsgFlags::MSG_TRUNC) {
						return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated UDP datagram"));
					}

					let mut stride = message.bytes;
					for control in message.cmsgs().map_err(io::Error::from)? {
						if let ControlMessageOwned::UdpGroSegments(value) = control {
							stride = usize::try_from(value)
								.map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative UDP GRO stride"))?;
						}
					}
					Self::received(message.bytes, stride)
				});

				match result {
					Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
					result => return result,
				}
			}
		}

		async fn recv_many(&mut self) -> io::Result<Received> {
			let buffer_len = self.recv_buffer_len();
			loop {
				self.receiver.readable().await?;
				let result = self.receiver.try_io(Interest::READABLE, || {
					let mut buffers = self
						.batch_buffers
						.each_mut()
						.map(|buffer| IoSliceMut::new(&mut buffer[..buffer_len]));
					let count = self
						.recv_state
						.recv((&self.receiver).into(), &mut buffers, &mut self.batch_meta)?;

					let mut received = Received::default();
					for meta in &self.batch_meta[..count] {
						let next = Self::received(meta.len, meta.stride)?;
						received.bytes += next.bytes;
						received.datagrams += next.datagrams;
					}
					Ok(received)
				});

				match result {
					Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
					result => return result,
				}
			}
		}

		fn recv_buffer_len(&self) -> usize {
			if self.config.gro {
				SEGMENT_SIZE * self.recv_state.gro_segments().get()
			} else {
				SEGMENT_SIZE
			}
		}

		fn received(bytes: usize, stride: usize) -> io::Result<Received> {
			if bytes == 0 || stride == 0 || !bytes.is_multiple_of(SEGMENT_SIZE) || stride != SEGMENT_SIZE {
				return Err(io::Error::new(
					io::ErrorKind::InvalidData,
					format!("invalid UDP receive: bytes={bytes}, stride={stride}"),
				));
			}
			Ok(Received {
				bytes,
				datagrams: bytes.div_ceil(stride),
			})
		}
	}

	pub fn benchmark(c: &mut Criterion) {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_io()
			.enable_time()
			.build()
			.expect("build Tokio current-thread runtime");

		let mut group = c.benchmark_group("udp_tokio_epoll");
		group.throughput(Throughput::Bytes(TOTAL_BYTES as u64));

		for config in Config::all() {
			let mut pair = runtime
				.block_on(async { Pair::new(config) })
				.expect("create UDP socket pair");
			group.bench_with_input(BenchmarkId::from_parameter(config), &config, |b, _| {
				b.iter_custom(|iterations| {
					let start = Instant::now();
					runtime.block_on(async {
						for _ in 0..iterations {
							pair.transfer().await.expect("transfer UDP benchmark payload");
						}
					});
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
