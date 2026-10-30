//! The contract end to end: a [`Client`] against a [`Coordinator`] over HTTP.

use std::sync::Arc;
use std::time::Duration;

use aws_lc_rs::signature::Ed25519KeyPair;

use super::*;
use crate::{Algorithm, Error, Key, KeyId};

struct Mesh {
	coordinator: Coordinator,
	base: url::Url,
	key: Key,
}

impl Mesh {
	async fn new() -> Self {
		Self::with_ttl(Duration::from_secs(3600)).await
	}

	async fn with_ttl(ttl: Duration) -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let base: url::Url = format!("http://{}/mesh", listener.local_addr().unwrap())
			.parse()
			.unwrap();
		let key = Key::generate(Algorithm::HS256, Some(KeyId::decode("mesh").unwrap())).unwrap();
		let mut config = Config::new(base.clone(), key.clone());
		config.relays = vec!["https://relay.example".parse().unwrap()];
		config.device_interval = Duration::ZERO;
		config.token_ttl = ttl;
		let coordinator = Coordinator::new(config).unwrap();
		let router = coordinator.router();
		tokio::spawn(async move { axum::serve(listener, router).await });
		Self { coordinator, base, key }
	}

	fn device(&self) -> Client {
		let signer = Arc::new(Ed25519KeyPair::generate().unwrap());
		Client::new(self.base.clone(), signer).unwrap()
	}

	fn key(&self, reusable: bool) -> String {
		let new = NewKey {
			reusable,
			tags: vec!["robot".into()],
			..Default::default()
		};
		self.coordinator.create_key(&new).unwrap().key.unwrap()
	}

	/// The code a refusal carries.
	fn refused(err: Error) -> ErrorCode {
		match err {
			Error::Mesh(reply) => reply.error,
			other => panic!("expected a refusal, got {other}"),
		}
	}
}

#[tokio::test]
async fn an_enrollment_key_grants_the_endpoints_subtree() {
	let mesh = Mesh::new().await;
	let device = mesh.device();
	let enrollment = device.enroll(&mesh.key(false), Some("Robot 1")).await.unwrap();

	assert_eq!(enrollment.name, "robot-1");
	assert_eq!(
		enrollment.relays,
		vec!["https://relay.example".parse::<url::Url>().unwrap()]
	);
	assert_eq!(enrollment.members.len(), 1);
	assert_eq!(enrollment.members[0].tags, vec!["robot".to_string()]);

	let id = enrollment.members[0].endpoint;
	let claims = mesh.key.verify(&enrollment.token).unwrap();
	assert_eq!(claims.expires, Some(enrollment.expires));
	let own = claims.authorize(&format!("mesh/{id}/ssh")).unwrap();
	assert!(!own.publish.is_empty());
	let other = claims.authorize("mesh/someone").unwrap();
	assert!(other.publish.is_empty());
	assert!(!other.subscribe.is_empty());
	assert!(claims.authorize("elsewhere").is_err());
}

#[tokio::test]
async fn a_single_use_key_enrolls_once() {
	let mesh = Mesh::new().await;
	let key = mesh.key(false);
	mesh.device().enroll(&key, None).await.unwrap();
	let err = mesh.device().enroll(&key, None).await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::InvalidGrant);

	let reusable = mesh.key(true);
	let first = mesh.device().enroll(&reusable, Some("bot")).await.unwrap();
	let second = mesh.device().enroll(&reusable, Some("bot")).await.unwrap();
	assert_eq!(first.name, "bot");
	assert_eq!(second.name, "bot-2");
	assert_eq!(second.members.len(), 3);
}

#[tokio::test]
async fn a_revoked_or_unknown_key_enrolls_nothing() {
	let mesh = Mesh::new().await;
	let key = mesh.key(true);
	let id = key.split_once('.').unwrap().0;
	assert!(mesh.coordinator.revoke_key(id).unwrap());
	let err = mesh.device().enroll(&key, None).await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::InvalidGrant);

	let err = mesh.device().enroll(&format!("{id}.wrong"), None).await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::InvalidGrant);
}

#[tokio::test]
async fn a_proof_from_the_wrong_key_is_refused() {
	let mesh = Mesh::new().await;
	let challenge = mesh.coordinator.challenge();
	let holder = Ed25519KeyPair::generate().unwrap();
	let victim = Ed25519KeyPair::generate().unwrap();
	let mut proof = Proof::sign(&holder, &mesh.base, challenge.challenge);
	proof.endpoint = victim.endpoint();
	let request = EnrollRequest {
		proof,
		key: mesh.key(true),
		name: None,
	};
	assert_eq!(
		mesh.coordinator.enroll(&request).unwrap_err().error,
		ErrorCode::InvalidSignature
	);

	// The challenge is spent, so even the honest retry needs a new one.
	let mut request = request;
	request.proof = Proof::sign(&holder, &mesh.base, request.proof.challenge);
	assert_eq!(
		mesh.coordinator.enroll(&request).unwrap_err().error,
		ErrorCode::InvalidChallenge
	);

	// A proof made for another coordinator does not verify here.
	let challenge = mesh.coordinator.challenge();
	request.proof = Proof::sign(
		&holder,
		&"https://other.example/mesh".parse().unwrap(),
		challenge.challenge,
	);
	assert_eq!(
		mesh.coordinator.enroll(&request).unwrap_err().error,
		ErrorCode::InvalidSignature
	);
}

#[tokio::test]
async fn a_device_grant_waits_for_approval() {
	let mesh = Mesh::new().await;
	let device = mesh.device();
	let authorization = device.device(Some("laptop")).await.unwrap();
	assert_eq!(authorization.verification_uri.path(), "/mesh/device");

	let pending = mesh.coordinator.pending();
	assert_eq!(pending.len(), 1);
	assert_eq!(pending[0].user_code, authorization.user_code);

	let poll = tokio::spawn({
		let device = device.clone();
		let authorization = authorization.clone();
		async move { device.poll(&authorization).await }
	});
	moq_clock::sleep(Duration::from_millis(50)).await;
	assert!(!poll.is_finished());

	assert!(mesh.coordinator.decide(&authorization.user_code.to_lowercase(), true));
	let enrollment = poll.await.unwrap().unwrap();
	assert_eq!(enrollment.name, "laptop");
	assert!(mesh.coordinator.pending().is_empty());

	// The device code is spent once it is exchanged.
	let err = device.poll(&authorization).await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::ExpiredToken);
}

#[tokio::test]
async fn a_denied_device_grant_enrolls_nothing() {
	let mesh = Mesh::new().await;
	let device = mesh.device();
	let authorization = device.device(None).await.unwrap();
	assert!(mesh.coordinator.decide(&authorization.user_code, false));
	assert!(!mesh.coordinator.decide(&authorization.user_code, true));
	let err = device.poll(&authorization).await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::AccessDenied);
	assert!(mesh.coordinator.devices().is_empty());
}

#[tokio::test]
async fn refresh_issues_a_new_token_until_revoked() {
	let mesh = Mesh::new().await;
	let device = mesh.device();

	// Refreshing before enrolling proves a key the mesh does not know.
	let err = device.refresh().await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::AccessDenied);

	let enrollment = device.enroll(&mesh.key(true), Some("pi")).await.unwrap();
	let refreshed = device.refresh().await.unwrap();
	assert_eq!(refreshed.name, "pi");
	assert!(mesh.key.verify(&refreshed.token).is_ok());

	let id = enrollment.members[0].endpoint;
	assert!(mesh.coordinator.revoke_device(id).unwrap());
	let err = device.refresh().await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::AccessDenied);

	// A reusable key cannot bring a revoked device back.
	let err = device.enroll(&mesh.key(true), None).await.unwrap_err();
	assert_eq!(Mesh::refused(err), ErrorCode::AccessDenied);
}

#[tokio::test]
async fn renew_refreshes_before_expiry() {
	let mesh = Mesh::with_ttl(Duration::from_secs(3)).await;
	let device = mesh.device();
	let enrollment = device.enroll(&mesh.key(true), None).await.unwrap();
	let renewed = device.renew(&enrollment).await.unwrap();
	assert!(moq_clock::wall() < enrollment.expires);
	assert!(renewed.expires > enrollment.expires);
}

#[test]
fn a_key_that_cannot_sign_refuses_to_start() {
	let key = Key::generate(Algorithm::ES256, None).unwrap().to_public().unwrap();
	let config = Config::new("https://mesh.example".parse().unwrap(), key);
	assert!(Coordinator::new(config).is_err());
}

#[test]
fn a_failed_save_spends_nothing() {
	let dir = tempfile::tempdir().unwrap();
	let key = Key::generate(Algorithm::HS256, None).unwrap();
	let mut config = Config::new("https://mesh.example".parse().unwrap(), key);
	config.state = Some(dir.path().join("mesh.json"));
	let coordinator = Coordinator::new(config).unwrap();
	let secret = coordinator.create_key(&NewKey::default()).unwrap().key.unwrap();

	// A directory where the temporary file goes makes every save fail.
	std::fs::create_dir(dir.path().join("mesh.tmp")).unwrap();
	let signer = Ed25519KeyPair::generate().unwrap();
	let base: url::Url = "https://mesh.example".parse().unwrap();
	let enroll = |coordinator: &Coordinator| EnrollRequest {
		proof: Proof::sign(&signer, &base, coordinator.challenge().challenge),
		key: secret.clone(),
		name: None,
	};
	let err = coordinator.enroll(&enroll(&coordinator)).unwrap_err();
	assert_eq!(err.error, ErrorCode::ServerError);
	assert!(coordinator.devices().is_empty());
	assert!(!coordinator.keys()[0].used);

	std::fs::remove_dir(dir.path().join("mesh.tmp")).unwrap();
	coordinator.enroll(&enroll(&coordinator)).unwrap();
	assert!(coordinator.keys()[0].used);
}

#[tokio::test]
async fn a_malformed_body_is_an_oauth_error() {
	let mesh = Mesh::new().await;
	let url = format!("{}/enroll", mesh.base);
	let response = reqwest::Client::new()
		.post(url)
		.header("content-type", "application/json")
		.body("{}")
		.send()
		.await
		.unwrap();
	assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
	let reply: ErrorReply = response.json().await.unwrap();
	assert_eq!(reply.error, ErrorCode::InvalidRequest);
}
