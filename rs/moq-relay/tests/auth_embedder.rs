//! Admission and lease behavior for a gateway embedding the relay.

use moq_auth::{Grant, Pattern, Patterns, Role, Transport, lease};
use moq_relay::{auth, cluster};

fn patterns(texts: &[&str]) -> Patterns {
	texts.iter().map(|text| text.parse::<Pattern>().unwrap()).collect()
}

#[tokio::test]
async fn gateway_admission_scopes_origins_and_revoke_stops_work() {
	let cluster = cluster::Cluster::new(cluster::Options::default()).unwrap();
	let (auth, mut admissions) = auth::Auth::embedded("edge");
	let mut grant = Grant::new(patterns(&["media/**"]), patterns(&["watch/**"]));
	grant.root = Some("canonical".into());
	grant.tier = Some("gateway".into());
	let decider = tokio::spawn(async move {
		let admission = admissions.next().await.expect("gateway asks to connect");
		assert_eq!(admission.request.transport, Transport::Rtmp);
		let (producer, consumer) = lease::Producer::new(grant);
		admission.grant(consumer);
		producer
	});

	let mut request = auth.request(Transport::Rtmp, "/legacy");
	request.role = Some(Role::Publisher);
	let admitted = cluster.admit(&auth, request).await.unwrap();
	assert_eq!(admitted.lease.token().root.as_str(), "canonical");
	assert_eq!(admitted.publisher.as_ref().unwrap().allowed(), patterns(&["media/**"]));
	assert!(
		admitted.subscriber.is_none(),
		"publisher-only gateway must not subscribe"
	);

	let producer = decider.await.unwrap();
	producer.revoke(lease::Reason::Refused);
	assert_eq!(
		auth::hold(admitted.lease, std::future::pending::<()>()).await,
		Err(lease::Reason::Refused)
	);
}

#[tokio::test]
async fn a_gateway_role_without_scope_is_forbidden() {
	let cluster = cluster::Cluster::new(cluster::Options::default()).unwrap();
	let (auth, mut admissions) = auth::Auth::embedded("edge");
	tokio::spawn(async move {
		let admission = admissions.next().await.unwrap();
		admission.grant(lease::Consumer::fixed(Grant::new(
			patterns(&["media/**"]),
			Patterns::new(),
		)));
	});
	let mut request = auth.request(Transport::WebRtc, "/room");
	request.role = Some(Role::Subscriber);
	assert!(matches!(
		cluster.admit(&auth, request).await,
		Err(auth::Error::Forbidden(_))
	));
}

#[tokio::test]
async fn completed_gateway_work_closes_its_lease() {
	let cluster = cluster::Cluster::new(cluster::Options::default()).unwrap();
	let (auth, mut admissions) = auth::Auth::embedded("edge");
	let decider = tokio::spawn(async move {
		let admission = admissions.next().await.unwrap();
		let (producer, consumer) = lease::Producer::new(Grant::new(patterns(&["media/**"]), Patterns::new()));
		admission.grant(consumer);
		producer
	});
	let admitted = cluster
		.admit(&auth, auth.request(Transport::Srt, "/room"))
		.await
		.unwrap();
	let producer = decider.await.unwrap();
	assert_eq!(auth::hold(admitted.lease, async { 7 }).await, Ok(7));
	assert_eq!(producer.closed().await.0, lease::Reason::Session("done".into()));
}

#[tokio::test]
async fn a_narrowed_gateway_grant_stops_work() {
	let cluster = cluster::Cluster::new(cluster::Options::default()).unwrap();
	let (auth, mut admissions) = auth::Auth::embedded("edge");
	let decider = tokio::spawn(async move {
		let admission = admissions.next().await.unwrap();
		let (producer, consumer) = lease::Producer::new(Grant::new(patterns(&["media/**"]), Patterns::new()));
		admission.grant(consumer);
		producer
	});
	let mut request = auth.request(Transport::Rtmp, "/room");
	request.role = Some(Role::Publisher);
	let admitted = cluster.admit(&auth, request).await.unwrap();
	let producer = decider.await.unwrap();
	producer.update(Grant::new(patterns(&["media/video/**"]), Patterns::new()));
	assert_eq!(
		auth::hold(admitted.lease, std::future::pending::<()>()).await,
		Err(lease::Reason::Narrowed)
	);
}

#[tokio::test]
async fn a_moved_gateway_tier_retags_its_stats() {
	use moq_net::stats;

	let mut cluster = cluster::Cluster::new(cluster::Options::default()).unwrap();
	cluster.stats = stats::Registry::new(stats::Config::new());
	let registry = cluster.stats.clone();
	let active = |tier: &'static str| {
		registry
			.snapshot()
			.sessions()
			.into_iter()
			.find(|(t, _)| *t == stats::Tier::new(tier))
			.map_or(0, |(_, presence)| presence.active())
	};

	let (auth, mut admissions) = auth::Auth::embedded("edge");
	let mut grant = Grant::new(patterns(&["media/**"]), Patterns::new());
	grant.tier = Some("gateway".into());
	let decider = tokio::spawn({
		let grant = grant.clone();
		async move {
			let admission = admissions.next().await.unwrap();
			let (producer, consumer) = lease::Producer::new(grant);
			admission.grant(consumer);
			producer
		}
	});
	let admitted = cluster
		.admit(&auth, auth.request(Transport::Srt, "/room"))
		.await
		.unwrap();
	let producer = decider.await.unwrap();
	assert_eq!(active("gateway"), 1);

	let work = tokio::spawn(auth::hold(admitted.lease, std::future::pending::<()>()));
	grant.tier = Some("moved".into());
	producer.update(grant);
	tokio::time::timeout(std::time::Duration::from_secs(5), async {
		while active("moved") != 1 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.expect("the held lease retags the admitted stats");
	assert_eq!(active("gateway"), 0);
	work.abort();
}
