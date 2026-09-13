//! Process recipe: a `moq --cluster-lan` importer beside a `moq-relay` with
//! `[cluster.lan]`.
//!
//! The in-process mesh (node-advertising cluster vs fingerprint-advertising
//! cluster, broadcasts both ways) lives in `cluster::tests::lan_meshes_node_and_fingerprint_clusters`.
//! This file is the live-mDNS recipe, ignored because CI runners often block
//! multicast even after announce succeeds.
//!
//! ```text
//! moq-relay --listen 127.0.0.1:4443 --listen-tls-generate localhost \
//!     --cluster-lan --auth-public ""
//! ffmpeg -re -f lavfi -i testsrc2 -f mpegts - | moq --cluster-lan import ts
//! moq --connect http://127.0.0.1:4443 --connect-tls-insecure export ts > /dev/null
//! ```

use std::time::Duration;

use moq_relay::{Cluster, ClusterConfig, ClusterOptions, LanAdvertise, LanConfig};

/// Two clusters that can advertise (fingerprint + node) start without a secret.
/// Binding mDNS is the recipe's gate; skip when the host cannot announce.
#[tokio::test]
#[ignore = "needs multicast on the host network; run with --run-ignored"]
async fn moq_import_cluster_lan_beside_a_relay() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut listen = moq_tokio::listen::Config::default();
	listen.bind = Some("127.0.0.1:0".to_string());
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
	let client = connect.clone().init(Default::default()).expect("client");

	let mut lan = LanConfig::default();
	lan.enabled = true;
	let mut config = ClusterConfig::default();
	config.lan = lan;
	config.node = Some(format!("https://127.0.0.1:{port}/"));

	let cluster = Cluster::new(ClusterOptions::new(config))
		.expect("cluster")
		.with_client(client)
		.with_connect(connect, Default::default())
		.with_advertise(LanAdvertise::new(port).with_fingerprint(fingerprint));

	match tokio::time::timeout(Duration::from_secs(10), cluster.start()).await {
		Ok(Ok(_)) => {}
		Ok(Err(err)) if format!("{err:#}").contains("no network interface") => {
			eprintln!("skipping: {err:#}");
		}
		Ok(Err(err)) => panic!("cluster start: {err:#}"),
		Err(_) => panic!("timed out advertising on the LAN"),
	}
}
