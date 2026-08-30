//! In-process WHIP/WHEP coverage over real HTTP, ICE, and UDP.

use std::time::Duration;

use axum::Router;
use bytes::Bytes;

use crate::codec::{Bridge, Frame, Track};
use crate::{Client, Server, client, server};

const TIMEOUT: Duration = Duration::from_secs(10);
const OPUS_PACKET: &[u8] = &[0xfc, 0xff, 0xfe];

#[tokio::test]
async fn whip_and_whep_round_trip_opus() {
	let source_origin = moq_net::Origin::random().produce();
	let source_consumer = source_origin.consume();
	let mut announcements = source_consumer.announced();
	let mut source = source_origin
		.create_broadcast("source", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create source broadcast");
	let catalog = moq_mux::catalog::Producer::new(&mut source).expect("create source catalog");
	let mut opus = crate::codec::opus::Bridge::new(source, catalog, 48_000, 2).expect("create Opus bridge");
	Bridge::push(
		&mut opus,
		Frame {
			timestamp_us: 20_000,
			payload: Bytes::from_static(OPUS_PACKET),
		},
	)
	.expect("publish source packet");
	let announcement = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("source announcement timed out")
		.expect("source origin closed");
	assert_eq!(announcement.path.as_str(), "source");
	assert!(announcement.broadcast.is_some(), "source was unannounced");
	drop(announcements);

	let server_origin = moq_net::Origin::random().produce();
	let server = Server::new(
		server::Config::default(),
		server_origin.clone(),
		server_origin.consume(),
	);
	let app = Router::new()
		.nest("/whip", server.publish_router())
		.nest("/whep", server.subscribe_router());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind HTTP listener");
	let address = listener.local_addr().expect("HTTP listener address");
	let http = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve HTTP") });

	let client = Client::new(client::Config::default());
	let whip = format!("http://{address}/whip/ingested").parse().expect("WHIP URL");
	tokio::time::timeout(TIMEOUT, client.publish(whip, source_consumer, "source"))
		.await
		.expect("WHIP negotiation timed out")
		.expect("WHIP negotiation failed");

	let output_origin = moq_net::Origin::random().produce();
	let output = output_origin
		.create_broadcast("output", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create output broadcast");
	let output_consumer = output.consume();
	let whep = format!("http://{address}/whep/ingested").parse().expect("WHEP URL");
	tokio::time::timeout(TIMEOUT, client.subscribe(whep, output))
		.await
		.expect("WHEP negotiation timed out")
		.expect("WHEP negotiation failed");

	let catalog_track = output_consumer
		.track(hang::Catalog::DEFAULT_NAME)
		.expect("output catalog track")
		.subscribe(hang::Catalog::default_subscription())
		.await
		.expect("subscribe to output catalog");
	let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(catalog_track);
	let catalog = tokio::time::timeout(TIMEOUT, catalogs.next())
		.await
		.expect("output catalog timed out")
		.expect("read output catalog")
		.expect("output catalog ended");
	let track_name = catalog.audio.renditions.keys().next().expect("output Opus rendition");
	let track = output_consumer
		.track(track_name)
		.expect("output Opus track")
		.subscribe(None)
		.await
		.expect("subscribe to output Opus track");
	let mut opus = Track::opus(track);
	let frame = tokio::time::timeout(TIMEOUT, opus.next())
		.await
		.expect("output Opus packet timed out")
		.expect("read output Opus packet")
		.expect("output Opus track ended");
	assert_eq!(frame.payload.as_ref(), OPUS_PACKET);

	http.abort();
}
