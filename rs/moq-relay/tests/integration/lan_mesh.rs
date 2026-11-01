//! Process recipe: a `moq --cluster-lan` importer beside a `moq-relay` with
//! `[cluster.lan]`.
//!
//! The in-process mesh (two origins, broadcasts both ways over a
//! fingerprint-pinned `/.cluster` session) lives in
//! `cluster::tests::lan_cluster_path_carries_broadcasts_both_ways`.
//! This file is the live-mDNS recipe, ignored because it multicasts on the
//! host network; nightly runs it with `just rs multicast`.
//!
//! ```text
//! moq-relay --listen 127.0.0.1:4443 --listen-tls-generate localhost \
//!     --cluster-lan --auth-public '**'
//! ffmpeg -re -f lavfi -i testsrc2 -f mpegts - | moq --cluster-lan import ts
//! moq --connect http://127.0.0.1:4443 --connect-tls-insecure export ts > /dev/null
//! ```

#![cfg(feature = "cluster-lan")]

use std::time::Duration;

use moq_relay::cluster;

/// Two clusters that can advertise (fingerprint + node) start without a secret.
/// Binding mDNS is the recipe's gate; skip when the host cannot announce,
/// unless CI_STRICT makes that a failure.
#[tokio::test]
#[ignore = "needs multicast on the host network; run by just rs multicast"]
async fn moq_import_cluster_lan_beside_a_relay() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut listen = moq_tokio::listen::Config::default();
	listen.bind = Some("127.0.0.1:0".parse().unwrap());
	listen.tls.generate = vec!["localhost".into()];
	let server = listen.init(Default::default()).expect("bind");
	let port = server.local_addr().expect("addr").port();
	let fingerprint = server
		.certificates()
		.fingerprints()
		.into_iter()
		.next()
		.expect("fingerprint");

	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.fingerprint = vec![fingerprint.clone()];

	let mut lan = cluster::LanConfig::default();
	lan.enabled = true;
	let mut config = cluster::Config::default();
	config.lan = lan;
	config.node = Some(format!("https://127.0.0.1:{port}/"));

	let cluster = cluster::Cluster::new(cluster::Options::new(config))
		.expect("cluster")
		.with_connect(connect, Default::default())
		.with_advertise(cluster::LanAdvertise::new(port).with_fingerprint(fingerprint));

	match kio::clock::timeout(Duration::from_secs(10), cluster.start()).await {
		Ok(Ok(_)) => {}
		Ok(Err(err))
			if format!("{err:#}").contains("no network interface") && std::env::var_os("CI_STRICT").is_none() =>
		{
			eprintln!("skipping: {err:#}");
		}
		Ok(Err(err)) => panic!("cluster start: {err:#}"),
		Err(_) => panic!("timed out advertising on the LAN"),
	}
}
