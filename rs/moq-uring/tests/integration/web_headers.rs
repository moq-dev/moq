//! Partial stream headers must not make unrelated ready streams cost a scan of every stream.

#![cfg(feature = "quic")]

use std::net::UdpSocket;
use std::time::Duration;

use moq_uring::{Config, Worker, quic, udp};

use crate::support;

/// Serve ready uni and bi streams while other streams hold partial headers, then finish them.
#[expect(
	clippy::disallowed_types,
	clippy::disallowed_methods,
	reason = "shared benchmark measures host time"
)]
pub fn run(pending: usize, ready: usize, iterations: u64) -> Duration {
	let mut worker = Worker::new(Config::default()).expect("worker");
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let mut server = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&["h3"], quic::server::ClientAuth::None)
			.expect("TLS"),
	);
	server.transport.limits.max_streams = std::num::NonZeroU64::new((pending + ready + 3) as u64);
	let socket = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let endpoint =
		quic::Endpoint::new(socket, quic::endpoint::Config::default().with_server(server)).expect("endpoint");
	let address = endpoint.local_addr();
	let (start_tx, mut start_rx) = tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<()>>(1);
	let (acked, acknowledgement) = tokio::sync::oneshot::channel();
	let client = std::thread::spawn(move || {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("runtime");
		runtime.block_on(async {
			let request = moq_tokio::quic::web::proto::ConnectRequest::new(
				url::Url::parse(&format!("https://{address}/headers")).unwrap(),
			);
			let session = support::web_connect(request, Default::default())
				.await
				.expect("connect");
			let mut uni = Vec::new();
			let mut bi = Vec::new();
			for _ in 0..pending {
				let mut send = session.connection().open_uni(Default::default()).await.unwrap();
				send.write_all(&[0x40][..]).await.unwrap();
				uni.push(send);
				let (mut send, recv) = session.connection().open_bi(Default::default()).await.unwrap();
				send.write_all(&[0x40][..]).await.unwrap();
				bi.push((send, recv));
			}
			while let Some(done) = start_rx.recv().await {
				for sequence in 0..ready {
					let mut send = session.open_uni(Default::default()).await.unwrap();
					send.write_all(sequence.to_be_bytes().as_slice()).await.unwrap();
					drop(send.finish());
					let (mut send, recv) = session.open_bi(Default::default()).await.unwrap();
					send.write_all(sequence.to_be_bytes().as_slice()).await.unwrap();
					drop(send.finish());
					drop(recv);
				}
				done.send(()).unwrap();
			}
			// The receiver has delivered every ready stream while these were mid-header.
			// Complete a two-byte type and an eight-byte session id, preserving the payload.
			for mut send in uni {
				send.write_all(&[0x54, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0xa5][..])
					.await
					.unwrap();
				drop(send.finish());
			}
			for (mut send, recv) in bi {
				send.write_all(&[0x41, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0xa5][..])
					.await
					.unwrap();
				drop(send.finish());
				drop(recv);
			}
			// Wait for acknowledgement so the client stays alive until all payloads arrived.
			let mut recv = session.accept_uni().await.unwrap();
			assert_eq!(recv.read_to_end(1).await.unwrap(), [0xff]);
				acknowledgement.await.expect("server received the ACK");
		});
	});
	let elapsed = worker
		.block_on(async {
			let conn = endpoint.accept().await.unwrap();
			let session = quic::web::Request::accept(conn).await.unwrap().ok().await.unwrap();
			let start = std::time::Instant::now();
			for _ in 0..iterations {
				let (done, sent) = tokio::sync::oneshot::channel();
				start_tx.send(done).await.unwrap();
				for sequence in 0..ready {
					let mut recv = session.accept_uni().await.unwrap();
					assert_eq!(recv.read_to_end(8).await.unwrap(), sequence.to_be_bytes());
					let (send, mut recv) = session.accept_bi().await.unwrap();
					assert_eq!(recv.read_to_end(8).await.unwrap(), sequence.to_be_bytes());
					drop(send.finish());
				}
				sent.await.unwrap();
			}
			let elapsed = start.elapsed();
			drop(start_tx);
			for _ in 0..pending {
				let mut recv = session.accept_uni().await.unwrap();
				assert_eq!(recv.read_to_end(1).await.unwrap(), [0xa5]);
				let (send, mut recv) = session.accept_bi().await.unwrap();
				assert_eq!(recv.read_to_end(1).await.unwrap(), [0xa5]);
				drop(send.finish());
			}
			let mut send = session.open_uni(Default::default()).await.unwrap();
			send.write_all(&[0xff][..]).await.unwrap();
			assert_eq!(send.finish().await.unwrap(), None);
			acked.send(()).expect("release client");
			elapsed
		})
		.expect("worker");
	client.join().expect("client thread");
	elapsed
}
