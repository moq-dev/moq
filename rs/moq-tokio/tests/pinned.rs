//! Fixed destinations must survive the client dispatch and never resolve a redirect.
#![cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]

#[tokio::test]
async fn fixed_target_preserves_request_and_refuses_redirect() {
	let mut listen = moq_tokio::listen::Config::default();
	listen.bind = Some("127.0.0.1:0".into());
	listen.tls.generate = vec!["relay.invalid".into()];
	let server = listen.init(Default::default()).unwrap();
	let mut server = server.listen().await.unwrap();
	let peer = server.local_addr().unwrap();
	let origin = moq_tokio::origin::spawn(moq_tokio::moq_net::Hop::random());
	let server_origin = origin.clone();
	let accepted = tokio::spawn(async move {
		let request = server.accept().await.unwrap();
		assert_eq!(request.path(), "/room");
		assert_eq!(request.query(), Some("jwt=secret"));
		let session = request.with_publisher(&server_origin).ok().await.unwrap();
		(server, session)
	});
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	let client = config.init(Default::default()).unwrap().with_publisher(&origin);
	let url: url::Url = format!("https://relay.invalid:{}/room?jwt=secret", peer.port())
		.parse()
		.unwrap();
	let target = moq_tokio::connect::Addr::resolved(url.clone(), [peer]).unwrap();
	let connection = client.connect(target);
	let connection = connection.established().await.unwrap();
	let (_server, server_session) = accepted.await.unwrap();
	server_session
		.drain()
		.send(moq_tokio::moq_net::goaway::Goaway::redirect(url.to_string()))
		.unwrap();
	assert!(matches!(
		connection.closed().await,
		Err(moq_tokio::Error::PinnedRedirect)
	));
}
