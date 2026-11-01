use std::time::Duration;

use futures::StreamExt;

use super::*;

type WsResult = Result<Message, tungstenite::Error>;

/// A WebSocket whose peer is a channel. Holding the sender keeps it open but silent; dropping
/// it ends the stream. Sent messages are kept for inspection.
struct Mock {
	rx: futures::channel::mpsc::UnboundedReceiver<WsResult>,
	sent: Vec<Message>,
}

impl Stream for Mock {
	type Item = WsResult;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<WsResult>> {
		self.rx.poll_next_unpin(cx)
	}
}

impl Sink<Message> for Mock {
	type Error = tungstenite::Error;

	fn poll_ready(self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn start_send(mut self: Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
		self.sent.push(message);
		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}
}

fn transport(
	keep_alive: Option<KeepAlive>,
	max_record_size: u64,
) -> (futures::channel::mpsc::UnboundedSender<WsResult>, WsTransport<Mock>) {
	let (tx, rx) = futures::channel::mpsc::unbounded();
	let mut transport = WsTransport::new(Mock { rx, sent: Vec::new() }, moq_net::transport::Address::Uri);
	if let Some(keep_alive) = keep_alive {
		transport = transport.with_keep_alive(keep_alive).unwrap();
	}
	transport.bind(&Config {
		max_record_size,
		..Config::default()
	});
	(tx, transport)
}

async fn recv(transport: &mut WsTransport<Mock>) -> Result<Bytes, Error> {
	kio::wait(|waiter| transport.poll_recv(waiter)).await
}

fn try_recv(transport: &mut WsTransport<Mock>) -> Poll<Result<Bytes, Error>> {
	transport.poll_recv(&kio::Waiter::noop())
}

const KEEP_ALIVE: KeepAlive = KeepAlive {
	interval: Duration::from_millis(10),
	timeout: Duration::from_millis(50),
};

/// A silent peer is gone once the keep-alive timeout passes.
#[tokio::test(start_paused = true)]
async fn times_out_a_silent_peer() {
	let (_peer, mut transport) = transport(Some(KEEP_ALIVE), 16382);
	let result = moq_clock::timeout(Duration::from_secs(1), recv(&mut transport))
		.await
		.expect("the keep-alive deadline should fire on a silent peer");
	assert!(matches!(result, Err(Error::Closed)), "got {result:?}");
}

/// Every message the peer sends, control included, restarts its deadline.
#[tokio::test(start_paused = true)]
async fn any_message_keeps_the_peer_alive() {
	let (peer, mut transport) = transport(Some(KEEP_ALIVE), 16382);
	for _ in 0..4 {
		moq_clock::sleep(Duration::from_millis(40)).await;
		peer.unbounded_send(Ok(Message::Pong(Bytes::new()))).unwrap();
		let pending = moq_clock::timeout(Duration::from_millis(1), recv(&mut transport)).await;
		assert!(pending.is_err(), "a live peer timed out");
	}
}

/// The keep-alive pings once per interval, the first one interval after start.
#[tokio::test(start_paused = true)]
async fn pings_each_interval() {
	let (_peer, mut transport) = transport(Some(KEEP_ALIVE), 16382);
	let pings = |transport: &WsTransport<Mock>| {
		transport
			.ws
			.sent
			.iter()
			.filter(|message| matches!(message, Message::Ping(_)))
			.count()
	};
	assert!(try_recv(&mut transport).is_pending());
	assert_eq!(pings(&transport), 0, "pinged on connect");
	moq_clock::sleep(Duration::from_millis(25)).await;
	assert!(try_recv(&mut transport).is_pending());
	assert_eq!(pings(&transport), 1, "one Ping per missed tick, not a burst");
	moq_clock::sleep(Duration::from_millis(10)).await;
	assert!(try_recv(&mut transport).is_pending());
	assert_eq!(pings(&transport), 2);
}

/// Without a keep-alive, a silent but open peer just leaves `recv` pending.
#[tokio::test(start_paused = true)]
async fn no_keep_alive_never_times_out() {
	let (_peer, mut transport) = transport(None, 16382);
	let pending = moq_clock::timeout(Duration::from_secs(60), recv(&mut transport)).await;
	assert!(pending.is_err(), "recv must not resolve without activity");
}

#[tokio::test]
async fn skips_empty_messages() {
	let (peer, mut transport) = transport(None, 16382);
	peer.unbounded_send(Ok(Message::Binary(Bytes::new()))).unwrap();
	peer.unbounded_send(Ok(Message::Binary(Bytes::from_static(b"one"))))
		.unwrap();
	assert_eq!(recv(&mut transport).await.unwrap(), &b"one"[..]);
}

#[tokio::test]
async fn refuses_a_message_past_one_record() {
	let (peer, mut transport) = transport(None, 16382);
	peer.unbounded_send(Ok(Message::Binary(Bytes::from(vec![0; 16383]))))
		.unwrap();
	assert!(matches!(recv(&mut transport).await, Err(Error::FrameTooLarge)));
}

#[tokio::test]
async fn refuses_a_text_message() {
	let (peer, mut transport) = transport(None, 16382);
	peer.unbounded_send(Ok(Message::Text("hello".into()))).unwrap();
	assert!(matches!(recv(&mut transport).await, Err(Error::ProtocolViolation)));
}
