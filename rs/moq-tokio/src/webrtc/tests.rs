use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use super::*;

/// Real sockets and real ICE, so these run on the wall clock.
const TIMEOUT: Duration = Duration::from_secs(10);

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A protocol both the default client and server speak.
fn protocol() -> &'static str {
	crate::connect::Config::default().versions().alpns()[0]
}

/// A one-frame broadcast named `test` and the handles that keep it alive.
fn broadcast(payload: &[u8]) -> (moq_net::origin::Producer, impl Sized) {
	let origin = crate::origin::spawn();
	let broadcast = origin.create_broadcast("test").unwrap();
	broadcast.announce(Default::default()).unwrap();
	let track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	let mut group = track.append_group().unwrap();
	group.write_frame(moq_net::Timestamp::ZERO, payload.to_vec()).unwrap();
	group.finish().unwrap();
	(origin, (broadcast, track))
}

/// Serve a one-frame broadcast from the answering peer, subscribe from the
/// offering one, and read the frame back across the data channel.
async fn transfer(offerer: &Offerer, answerer: &Answerer, payload: &[u8]) {
	let (origin, _keep) = broadcast(payload);
	let listener = crate::listen::Config::default()
		.init_streams()
		.unwrap()
		.listen()
		.await
		.unwrap();
	let server = async {
		let request = listener.accept_webrtc(answerer).await.unwrap();
		assert_eq!(request.transport(), crate::Transport::WebRtc);
		request.with_publisher(&origin).ok().await.unwrap()
	};

	let subscribed = crate::origin::spawn();
	let consumer = subscribed.consume();
	let mut announced = consumer.announced();
	let client = crate::client::Config::default()
		.init()
		.unwrap()
		.with_subscriber(subscribed);
	let (_server, _client) = moq_clock::timeout(TIMEOUT, async {
		tokio::join!(server, async { client.connect_webrtc(offerer).await.unwrap() })
	})
	.await
	.expect("handshake timed out");

	let read = async {
		let Some(moq_net::announce::Event::Start(route)) = announced.next().await else {
			panic!("expected the broadcast to be announced");
		};
		assert_eq!(route.prefix.as_str(), "test");
		let broadcast = consumer.request("test", None).await.unwrap();
		let mut track = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.recv_group().await.unwrap().unwrap();
		group.read_frame().await.unwrap().unwrap()
	};
	let frame = moq_clock::timeout(TIMEOUT, read).await.expect("transfer timed out");
	assert!(frame.payload[..] == *payload, "the frame arrived corrupted");
}

#[tokio::test]
async fn session_over_loopback() {
	let config = Config::default();
	let (offerer, offer) = Offerer::with_hosts(&config, protocol(), vec![LOOPBACK]).await.unwrap();
	let (answerer, answer) = Answerer::with_hosts(&config, &offer, protocol(), vec![LOOPBACK])
		.await
		.unwrap();
	offerer.accept(&answer).await.unwrap();

	transfer(&offerer, &answerer, b"hello").await;
}

/// A frame many records long fills the SCTP send window, so the writer has to
/// wait for the peer's acknowledgements and resume.
#[tokio::test]
async fn large_frame_over_loopback() {
	let config = Config::default();
	let (offerer, offer) = Offerer::with_hosts(&config, protocol(), vec![LOOPBACK]).await.unwrap();
	let (answerer, answer) = Answerer::with_hosts(&config, &offer, protocol(), vec![LOOPBACK])
		.await
		.unwrap();
	offerer.accept(&answer).await.unwrap();

	let payload: Vec<u8> = (0..1024 * 1024).map(|i| i as u8).collect();
	transfer(&offerer, &answerer, &payload).await;
}

/// Open the data channel between two loopback peers, without a MoQ session over it.
async fn channels() -> (Offerer, Answerer, Opened, Opened) {
	let config = Config::default();
	let (offerer, offer) = Offerer::with_hosts(&config, protocol(), vec![LOOPBACK]).await.unwrap();
	let (answerer, answer) = Answerer::with_hosts(&config, &offer, protocol(), vec![LOOPBACK])
		.await
		.unwrap();
	offerer.accept(&answer).await.unwrap();
	let (sent, received) = moq_clock::timeout(TIMEOUT, async { tokio::join!(offerer.channel(), answerer.channel()) })
		.await
		.expect("the channel did not open");
	(offerer, answerer, sent.unwrap(), received.unwrap())
}

/// A full record, numbered so its order can be checked.
fn record(index: usize) -> Bytes {
	Bytes::from(vec![index as u8; RECORD_SIZE as usize])
}

/// A session that is not reading must not stop the peer connection: the agent
/// keeps acknowledging, so the remote writer gets well past the send window.
#[tokio::test]
async fn unread_records_do_not_stall_the_writer() {
	use qmux::transport::{Reader as _, Writer as _};

	let (_offerer, _answerer, sent, received) = channels().await;
	let (mut writer, mut reader) = (sent.channel.writer, received.channel.reader);
	let count = 8 * SEND_WINDOW / RECORD_SIZE as usize;
	moq_clock::timeout(TIMEOUT, async {
		for index in 0..count {
			writer.send(record(index)).await.unwrap();
		}
	})
	.await
	.expect("the writer stalled while the reader was idle");

	for index in 0..count {
		assert_eq!(reader.recv().await.unwrap(), record(index));
	}
}

/// A peer that sends past qmux's receive window while the session is not reading
/// has the channel closed, after the records within the budget.
#[tokio::test]
async fn ignoring_flow_control_closes_the_channel() {
	use qmux::transport::{Reader as _, Writer as _};

	let (_offerer, _answerer, sent, received) = channels().await;
	let budget = RECEIVE_WINDOW + READ_SLACK;
	let within = (budget / RECORD_SIZE) as usize;
	let (mut writer, mut reader) = (sent.channel.writer, received.channel.reader);
	moq_clock::timeout(TIMEOUT, async {
		for index in 0..within + 8 {
			if writer.send(record(index)).await.is_err() {
				break;
			}
		}
	})
	.await
	.expect("the writer stalled");

	for index in 0..within {
		assert_eq!(reader.recv().await.unwrap(), record(index));
	}
	assert!(matches!(reader.recv().await, Err(qmux::Error::FlowControlError)));
}

/// Answer STUN Binding requests with the source address, as a public server would.
async fn stun_server() -> SocketAddr {
	use stun_types::attribute::XorMappedAddress;
	use stun_types::message::{Message, MessageWriteVec};
	use stun_types::prelude::*;

	let socket = UdpSocket::bind((LOOPBACK, 0)).await.unwrap();
	let addr = socket.local_addr().unwrap();
	tokio::spawn(async move {
		let mut buf = [0; 1500];
		while let Ok((len, src)) = socket.recv_from(&mut buf).await {
			let request = Message::from_bytes(&buf[..len]).unwrap();
			let mut reply = Message::builder_success(&request, MessageWriteVec::new());
			reply
				.add_attribute(&XorMappedAddress::new(src, request.transaction_id()))
				.unwrap();
			socket.send_to(&reply.finish(), src).await.unwrap();
		}
	});
	addr
}

/// Drop the candidates a description carries, so only trickled ones reach the peer.
fn strip_candidates(sdp: &str) -> String {
	sdp.split_inclusive('\n')
		.filter(|line| !line.starts_with("a=candidate:"))
		.collect()
}

/// The offerer sits behind a pretend NAT: it advertises 127.0.0.2, which the
/// answerer never learns, and reaches the world as 127.0.0.1. With no candidates
/// in either description and none trickled back, the answerer can only reach the
/// offerer through the server-reflexive candidate STUN found.
#[tokio::test]
async fn server_reflexive_candidate_is_offered_and_selected() {
	let stun = stun_server().await;
	let config = Config {
		stun: vec![format!("stun:{stun}").parse().unwrap()],
	};
	let private = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
	let (mut offerer, offer) = Offerer::with_hosts(&config, protocol(), vec![private]).await.unwrap();
	let offer = Offer::new(strip_candidates(offer.as_str()));
	let (answerer, answer) = Answerer::with_hosts(&Config::default(), &offer, protocol(), vec![LOOPBACK])
		.await
		.unwrap();
	offerer
		.accept(&Answer::new(strip_candidates(answer.as_str())))
		.await
		.unwrap();

	let candidate = moq_clock::timeout(TIMEOUT, offerer.candidate())
		.await
		.expect("gathering timed out")
		.expect("no server-reflexive candidate");
	let parsed = str0m::Candidate::from_sdp_string(candidate.as_str()).unwrap();
	assert_eq!(parsed.kind(), str0m::CandidateKind::ServerReflexive);
	assert_eq!(parsed.addr().ip(), LOOPBACK);
	answerer.add_candidate(&candidate).await.unwrap();
	assert_eq!(
		offerer.candidate().await,
		None,
		"gathering should end after the only server answers"
	);

	transfer(&offerer, &answerer, b"hello").await;
}

#[tokio::test]
async fn unsupported_protocol_is_refused() {
	let (offerer, _) = Offerer::with_hosts(&Config::default(), "not-moq", vec![LOOPBACK])
		.await
		.unwrap();
	let client = crate::client::Config::default().init().unwrap();
	assert!(matches!(
		client.connect_webrtc(&offerer).await,
		Err(crate::Error::WebRtc(err)) if matches!(*err, Error::Protocol(_))
	));
}

/// A channel carrying another supported ALPN than the one signaling agreed is refused.
#[tokio::test]
async fn unsignaled_protocol_is_refused() {
	let versions = crate::connect::Config::default().versions();
	let [agreed, other, ..] = versions.alpns()[..] else {
		panic!("expected two supported ALPNs");
	};
	let config = Config::default();
	let (offerer, offer) = Offerer::with_hosts(&config, other, vec![LOOPBACK]).await.unwrap();
	let (answerer, answer) = Answerer::with_hosts(&config, &offer, agreed, vec![LOOPBACK])
		.await
		.unwrap();
	offerer.accept(&answer).await.unwrap();

	let listener = crate::listen::Config::default()
		.init_streams()
		.unwrap()
		.listen()
		.await
		.unwrap();
	let accepted = moq_clock::timeout(TIMEOUT, listener.accept_webrtc(&answerer))
		.await
		.expect("the channel did not open");
	assert!(matches!(
		accepted,
		Err(crate::Error::WebRtc(err)) if matches!(&*err, Error::Protocol(protocol) if protocol == other)
	));
}

#[test]
fn stun_urls() {
	let parse = |url: &str| url.parse::<Stun>().map(|stun| stun.to_string()).ok();
	assert_eq!(
		parse("stun:stun.example.com").as_deref(),
		Some("stun:stun.example.com:3478")
	);
	assert_eq!(parse("STUN:1.2.3.4:19302").as_deref(), Some("stun:1.2.3.4:19302"));
	assert_eq!(parse("stun:[::1]").as_deref(), Some("stun:[::1]:3478"));
	assert_eq!(parse("stun:[::1]:5349").as_deref(), Some("stun:[::1]:5349"));
	for bad in [
		"turn:turn.example.com",
		"stuns:stun.example.com",
		"stun:",
		"stun:host:port",
		"stun:host:3478?transport=udp",
		"stun:[::1]x",
		"stun:[nope]",
		"stun:server.example#fragment",
		"stun:server example",
		"stun:server\texample",
		"stun.example.com",
	] {
		assert_eq!(parse(bad), None, "{bad}");
	}
}

#[test]
fn max_message_sizes() {
	assert_eq!(max_message_size("v=0\r\n").unwrap(), DEFAULT_MAX_MESSAGE_SIZE);
	assert_eq!(max_message_size("a=max-message-size:262144\r\n").unwrap(), 262_144);
	assert_eq!(max_message_size("a=max-message-size:0\r\n").unwrap(), u64::MAX);
	assert!(max_message_size("a=max-message-size:big\r\n").is_err());
}

#[test]
fn mdns_candidates_are_split_out() {
	let sdp = "v=0\r\n\
		a=candidate:1 1 udp 2113937151 0a1b2c3d.local 54321 typ host\r\n\
		a=candidate:2 1 udp 2113937151 192.168.1.2 54321 typ host\r\n\
		a=end-of-candidates\r\n";
	let (kept, mdns) = split_mdns(sdp).unwrap();
	assert_eq!(
		kept,
		"v=0\r\na=candidate:2 1 udp 2113937151 192.168.1.2 54321 typ host\r\na=end-of-candidates\r\n"
	);
	assert_eq!(mdns, ["candidate:1 1 udp 2113937151 0a1b2c3d.local 54321 typ host"]);
}

/// A malformed `.local` candidate is refused up front, not after costing a query.
#[test]
fn malformed_mdns_candidates_are_refused() {
	let sdp = "v=0\r\na=candidate:1 1 udp nope 0a1b2c3d.local 54321 typ host\r\n";
	assert!(matches!(split_mdns(sdp), Err(Error::Candidate(_))));
	assert!(mdns_host("candidate:1 1 udp 2113937151 0a1b2c3d.local 54321 typ nope").is_err());
	assert_eq!(
		mdns_host("candidate:1 1 udp 2113937151 0a1b2c3d.local 54321 typ host").unwrap(),
		Some("0a1b2c3d.local")
	);
}

/// Building a peer mutates its `Rtc` several times before the agent's first drain:
/// a host candidate per address, then the channel or the answer. str0m wants a
/// drain after each, which is a no-op while none of them queues output; pin that,
/// so a str0m upgrade that changes it fails here.
#[test]
fn construction_queues_no_output() {
	fn idle(rtc: &mut Rtc) {
		assert!(matches!(rtc.poll_output(), Ok(str0m::Output::Timeout(_))));
	}
	fn host(rtc: &mut Rtc, addr: &str) {
		let candidate = str0m::Candidate::host(addr.parse().unwrap(), "udp").unwrap();
		rtc.add_local_candidate(candidate);
		idle(rtc);
	}

	let mut offerer = Rtc::builder().build(moq_clock::now().into_std());
	host(&mut offerer, "127.0.0.1:5000");
	host(&mut offerer, "[::1]:5000");
	let mut api = offerer.sdp_api();
	api.add_channel(LABEL.to_string());
	let (offer, _pending) = api.apply().unwrap();
	idle(&mut offerer);

	let mut answerer = Rtc::builder().build(moq_clock::now().into_std());
	host(&mut answerer, "127.0.0.1:5001");
	host(&mut answerer, "[::1]:5001");
	answerer.sdp_api().accept_offer(offer).unwrap();
	idle(&mut answerer);
}

/// A trickled IP candidate waits for the driver to take the one before it, so a
/// peer cannot queue candidates faster than the connection applies them.
#[tokio::test(start_paused = true)]
async fn trickled_candidates_wait_for_the_driver() {
	let (commands, mut driver) = mpsc::channel(1);
	let (_candidates, candidates) = mpsc::unbounded_channel();
	let peer = Peer {
		resolver: Resolver::spawn(commands.clone(), resolver::Mdns),
		commands,
		candidates,
	};
	let candidate = Candidate::new("candidate:1 1 udp 2113937151 192.0.2.1 54321 typ host");

	peer.add_candidate(&candidate).await.unwrap();
	let mut second = std::pin::pin!(peer.add_candidate(&candidate));
	assert!(
		futures::poll!(&mut second).is_pending(),
		"the second candidate queued while the driver held the first"
	);

	assert!(matches!(driver.recv().await, Some(Command::Candidate(_))));
	second.await.unwrap();
	assert!(matches!(driver.recv().await, Some(Command::Candidate(_))));
}
