//! Failing unroutable dials while other handshakes are in flight on the same socket.
//!
//! A send that finds no route fails the handshakes dialing that destination.
//! Its cost should follow the failed destinations alone, not how many other
//! handshakes the endpoint is carrying, so the per-dial time stays flat across
//! the `handshakes` axis.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
mod linux {
	use std::future::Future;
	use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
	use std::os::fd::AsRawFd;
	use std::pin::Pin;
	use std::task::Poll;
	use std::time::Duration;

	use criterion::{BenchmarkId, Criterion, Throughput};
	use moq_uring::{Config, Error, Worker, quic, udp};
	use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrIn6, bind, setsockopt, socket, sockopt};

	type Dial<'a> = Pin<Box<dyn Future<Output = Result<quic::Connection, quic::Error>> + 'a>>;

	fn dial_config() -> quic::client::Config {
		let mut config = quic::client::Config::default();
		config.alpn = vec!["moq-uring-bench".to_string()];
		config.verify = false;
		config.system_roots = false;
		config
	}

	/// An IPv6-only socket, which Linux refuses to send to a v4-mapped address
	/// with `ENETUNREACH`: an unroutable family without touching the routes.
	fn v6_only() -> UdpSocket {
		let fd = socket(AddressFamily::Inet6, SockType::Datagram, SockFlag::empty(), None).expect("socket");
		setsockopt(&fd, sockopt::Ipv6V6Only, &true).expect("IPV6_V6ONLY");
		let local: std::net::SocketAddrV6 = "[::1]:0".parse().expect("addr");
		bind(fd.as_raw_fd(), &SockaddrIn6::from(local)).expect("bind");
		UdpSocket::from(fd)
	}

	/// Poll every dial until it resolves, returning how many failed.
	async fn settle(mut dials: Vec<Dial<'_>>) -> usize {
		let mut failed = 0;
		std::future::poll_fn(|cx| {
			dials.retain_mut(|dial| match dial.as_mut().poll(cx) {
				Poll::Ready(result) => {
					failed += usize::from(result.is_err());
					false
				}
				Poll::Pending => true,
			});
			if dials.is_empty() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		})
		.await;
		failed
	}

	pub fn benchmark(c: &mut Criterion) {
		let mut group = c.benchmark_group("unreachable_dials");
		// The background dials end at the 10s handshake timeout, so each point
		// has to finish well inside it.
		group
			.sample_size(10)
			.warm_up_time(Duration::from_millis(500))
			.measurement_time(Duration::from_secs(2));

		for handshakes in [1, 256, 4096] {
			for destinations in [1u16, 8, 64] {
				let mut worker = match Worker::new(Config::default()) {
					Ok(worker) => worker,
					Err(Error::Unsupported(reason)) => {
						eprintln!("skipping unreachable dial benchmark: {reason}");
						return;
					}
					Err(err) => panic!("worker setup failed: {err}"),
				};
				let handle = worker.handle();
				let endpoint = quic::Endpoint::new(
					handle.udp(v6_only(), udp::Config::default()).expect("socket"),
					quic::endpoint::Config::default(),
				)
				.expect("endpoint");

				// Handshakes into a socket nobody reads stay in flight for the run.
				let hole = UdpSocket::bind("[::1]:0").expect("bind");
				let into_hole = hole.local_addr().expect("addr");
				let config = dial_config();
				let mut background: Vec<Dial<'_>> = (0..handshakes)
					.map(|_| Box::pin(endpoint.connect(&config, into_hole, "localhost")) as Dial<'_>)
					.collect();
				worker
					.block_on(std::future::poll_fn(|cx| {
						for dial in &mut background {
							assert!(dial.as_mut().poll(cx).is_pending(), "a dial into a hole resolved");
						}
						Poll::Ready(())
					}))
					.expect("worker");

				let unroutable: Vec<SocketAddr> = (1..=destinations)
					.map(|port| SocketAddr::new(Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), port))
					.collect();

				group.throughput(Throughput::Elements(destinations.into()));
				group.bench_function(
					BenchmarkId::new(format!("handshakes-{handshakes}"), destinations),
					|b| {
						b.iter(|| {
							let dials = unroutable
								.iter()
								.map(|&addr| Box::pin(endpoint.connect(&config, addr, "localhost")) as Dial<'_>)
								.collect();
							let failed = worker.block_on(settle(dials)).expect("worker");
							assert_eq!(failed, unroutable.len(), "every unroutable dial fails");
						});
					},
				);
				drop(background);
			}
		}
		group.finish();
	}
}

#[cfg(target_os = "linux")]
fn benchmark(c: &mut criterion::Criterion) {
	linux::benchmark(c);
}

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark);
criterion_main!(benches);
