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
fn broadcast() -> (moq_net::origin::Producer, impl Sized) {
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
	group.write_frame(moq_net::Timestamp::ZERO, &b"hello"[..]).unwrap();
	group.finish().unwrap();
	(origin, (broadcast, track))
}

/// Serve the broadcast from the answering peer, subscribe from the offering one,
/// and read the frame back across the data channel.
async fn transfer(offerer: &Offerer, answerer: &Answerer) {
	let (origin, _keep) = broadcast();
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
	let ((_server, _driver), _client) = tokio::time::timeout(TIMEOUT, async {
		tokio::join!(server, async { client.connect_webrtc(offerer).await.unwrap() })
	})
	.await
	.expect("handshake timed out");

	let read = async {
		let Some(moq_net::announce::Event::Start(route)) = announced.next().await else {
			panic!("expected the broadcast to be announced");
		};
		assert_eq!(route.prefix.as_str(), "test");
		let broadcast = consumer.request_broadcast("test", None).await.unwrap();
		let mut track = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.recv_group().await.unwrap().unwrap();
		group.read_frame().await.unwrap().unwrap()
	};
	let frame = tokio::time::timeout(TIMEOUT, read).await.expect("transfer timed out");
	assert_eq!(&frame.payload[..], b"hello");
}

#[tokio::test]
async fn session_over_loopback() {
	let config = Config::default();
	let (offerer, offer) = Offerer::with_hosts(&config, protocol(), vec![LOOPBACK]).await.unwrap();
	let (answerer, answer) = Answerer::with_hosts(&config, &offer, vec![LOOPBACK]).await.unwrap();
	offerer.accept(&answer).await.unwrap();

	transfer(&offerer, &answerer).await;
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
	let (answerer, answer) = Answerer::with_hosts(&Config::default(), &offer, vec![LOOPBACK])
		.await
		.unwrap();
	offerer
		.accept(&Answer::new(strip_candidates(answer.as_str())))
		.await
		.unwrap();

	let candidate = tokio::time::timeout(TIMEOUT, offerer.candidate())
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

	transfer(&offerer, &answerer).await;
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
	let (kept, mdns) = split_mdns(sdp);
	assert_eq!(
		kept,
		"v=0\r\na=candidate:2 1 udp 2113937151 192.168.1.2 54321 typ host\r\na=end-of-candidates\r\n"
	);
	assert_eq!(mdns, ["candidate:1 1 udp 2113937151 0a1b2c3d.local 54321 typ host"]);
}

#[tokio::test]
async fn ip_candidates_parse_without_mdns() {
	let candidate = parse_candidate("candidate:2 1 udp 2113937151 192.168.1.2 54321 typ host")
		.await
		.unwrap();
	assert_eq!(candidate.addr(), "192.168.1.2:54321".parse().unwrap());
	assert!(parse_candidate("candidate:2 1 udp nope").await.is_err());
}
