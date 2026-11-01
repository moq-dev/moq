//! The reference coordinator behind `moq auth serve --mesh-url`.
//!
//! [`Coordinator::router`] answers the device-facing contract; [`Coordinator::admin_router`]
//! is the owner's API, mounted on the auth server's own listener, which has no
//! authentication and stays on loopback. Enrollment keys and devices persist to a JSON
//! state file. Challenges are sealed with a secret held in memory, so a restart only makes
//! a device fetch a new challenge.
//!
//! A challenge is single-use per endpoint: each carries a sequence number, and an endpoint's
//! proof is accepted only over a challenge newer than the last one it was granted with. The
//! replay state is one number per enrolled device, so no caller can spend another's room.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use super::{CHALLENGE_LEN, Challenge, EndpointId, EnrollRequest, Enrollment, ErrorCode, ErrorReply, Member, Proof};
use crate::{Claims, Key, Pattern};

/// How long a challenge stays valid.
const CHALLENGE_TTL: Duration = Duration::from_secs(60);

/// A challenge is its expiry (unix seconds), its sequence number, then a truncated HMAC
/// over both. The HMAC makes it unpredictable; the sequence makes it single-use.
const TAG_LEN: usize = CHALLENGE_LEN - 16;

/// The longest a device name may be: one DNS label.
const MAX_NAME: usize = 63;

/// What the coordinator issues, and where it keeps its state.
///
/// `#[non_exhaustive]`, so build it with [`Config::new`] and set the fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The public base URL devices reach the contract at, which every proof is bound to.
	pub url: url::Url,
	/// The key that signs mesh tokens; the relays must verify it.
	pub key: Key,
	/// The relays an [`Enrollment`] tells a device to dial.
	pub relays: Vec<url::Url>,
	/// The JSON file enrollment keys and devices persist to; `None` keeps them in memory.
	pub state: Option<PathBuf>,
	/// How long an issued token lasts.
	pub token_ttl: Duration,
}

impl Config {
	/// A coordinator at `url` signing with `key`, with hour-long tokens, no relays, and no
	/// state file.
	pub fn new(url: url::Url, key: Key) -> Self {
		Self {
			url,
			key,
			relays: Vec::new(),
			state: None,
			token_ttl: Duration::from_secs(3600),
		}
	}
}

/// The state that survives a restart.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Store {
	#[serde(default)]
	keys: Vec<StoredKey>,
	#[serde(default)]
	devices: Vec<Device>,
}

/// An enrollment key, holding only the SHA-256 of its secret.
#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
struct StoredKey {
	id: String,
	/// Hex, so the file stays readable.
	hash: String,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	tags: Vec<String>,
	#[serde_as(as = "Option<moq_clock::serde::UnixSeconds>")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	expires: Option<moq_clock::Wall>,
	reusable: bool,
	#[serde(default)]
	used: bool,
	#[serde(default)]
	revoked: bool,
}

/// An enrolled device. A revoked one stays recorded so a reusable key cannot re-enroll it.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Device {
	/// Its endpoint ID.
	pub endpoint: EndpointId,
	/// Its unique, DNS-safe name.
	pub name: String,
	/// The tags of the enrollment key it joined with.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
	/// When it first enrolled, unix seconds.
	#[serde_as(as = "moq_clock::serde::UnixSeconds")]
	pub enrolled: moq_clock::Wall,
	/// Whether the owner revoked it; its next refresh fails.
	#[serde(default)]
	pub revoked: bool,
}

/// An enrollment key as the owner sees it, without its secret.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeyInfo {
	/// The ID the owner revokes it by.
	pub id: String,
	/// The tags every device it enrolls carries.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
	/// When it stops enrolling, unix seconds.
	#[serde_as(as = "Option<moq_clock::serde::UnixSeconds>")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub expires: Option<moq_clock::Wall>,
	/// Whether it enrolls more than one device.
	pub reusable: bool,
	/// Whether a single-use key has enrolled its device.
	pub used: bool,
	/// Whether the owner revoked it.
	pub revoked: bool,
	/// The key itself, returned only when it is created.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub key: Option<String>,
}

impl From<&StoredKey> for KeyInfo {
	fn from(key: &StoredKey) -> Self {
		Self {
			id: key.id.clone(),
			tags: key.tags.clone(),
			expires: key.expires,
			reusable: key.reusable,
			used: key.used,
			revoked: key.revoked,
			key: None,
		}
	}
}

/// `POST /mesh/keys` on the admin API: what the new enrollment key allows.
#[serde_as]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct NewKey {
	/// Tags every device it enrolls carries.
	pub tags: Vec<String>,
	/// How long until it stops enrolling; `None` never expires. Whole seconds on the wire.
	#[serde_as(as = "Option<serde_with::DurationSeconds<u64>>")]
	pub expires_in: Option<Duration>,
	/// Whether it enrolls more than one device.
	pub reusable: bool,
}

#[derive(Default)]
struct Inner {
	store: Store,
	/// The sequence of the last challenge each endpoint was granted with. Only an
	/// enrolled device gets an entry, so it grows with the devices and nothing else.
	used: HashMap<EndpointId, u64>,
}

/// The mesh coordinator: issues challenges, enrolls devices, and refreshes their tokens.
#[derive(Clone)]
pub struct Coordinator {
	config: Arc<Config>,
	/// Seals challenges, so an outstanding one needs no state.
	secret: Arc<aws_lc_rs::hmac::Key>,
	/// The next challenge's sequence number. It restarts with the secret, which voids
	/// every earlier challenge.
	sequence: Arc<AtomicU64>,
	inner: Arc<Mutex<Inner>>,
}

type Reply<T> = Result<T, ErrorReply>;

impl Coordinator {
	/// A coordinator for `config`, loading its state file if one exists.
	pub fn new(config: Config) -> crate::Result<Self> {
		let store = match &config.state {
			Some(path) => match std::fs::read_to_string(path) {
				Ok(json) => serde_json::from_str(&json).map_err(|err| crate::Error::MeshState(err.to_string()))?,
				Err(err) if err.kind() == std::io::ErrorKind::NotFound => Store::default(),
				Err(err) => return Err(crate::Error::MeshState(err.to_string())),
			},
			None => Store::default(),
		};
		// Sign once now, so a key that cannot sign these claims fails at startup rather
		// than after an enrollment key is spent.
		config
			.key
			.sign(&claims(EndpointId::from_bytes([0; 32]), moq_clock::wall(), None))?;
		Ok(Self {
			config: Arc::new(config),
			secret: Arc::new(aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, &random(32))),
			sequence: Default::default(),
			inner: Arc::new(Mutex::new(Inner {
				store,
				..Default::default()
			})),
		})
	}

	/// Issue a fresh challenge. It holds no state until a proof uses it, so no number of
	/// requests can crowd out one a device is still signing.
	pub fn challenge(&self) -> Challenge {
		let now = moq_clock::wall();
		let expires =
			now.checked_duration_since(moq_clock::Wall::UNIX_EPOCH)
				.unwrap_or_default()
				.as_secs() + CHALLENGE_TTL.as_secs();
		let mut challenge = expires.to_be_bytes().to_vec();
		challenge.extend_from_slice(&self.sequence.fetch_add(1, Ordering::Relaxed).to_be_bytes());
		let tag = self.tag(&challenge);
		challenge.extend_from_slice(&tag);
		Challenge {
			challenge,
			expires: moq_clock::Wall::UNIX_EPOCH.saturating_add(Duration::from_secs(expires)),
		}
	}

	/// Enroll with an enrollment key.
	pub fn enroll(&self, request: &EnrollRequest) -> Reply<Enrollment> {
		let mut inner = self.inner.lock().unwrap();
		let sequence = self.check(&inner, &request.proof)?;

		let (id, secret) = request
			.key
			.split_once('.')
			.ok_or_else(|| ErrorReply::new(ErrorCode::InvalidGrant, "unknown enrollment key"))?;
		let hash = hex(&sha256(secret.as_bytes()));
		let now = moq_clock::wall();
		let key = inner
			.store
			.keys
			.iter()
			.find(|key| key.id == id && constant_eq(key.hash.as_bytes(), hash.as_bytes()))
			.ok_or_else(|| ErrorReply::new(ErrorCode::InvalidGrant, "unknown enrollment key"))?;
		if key.revoked {
			return Err(ErrorReply::new(
				ErrorCode::InvalidGrant,
				"the enrollment key is revoked",
			));
		}
		if key.expires.is_some_and(|expires| expires <= now) {
			return Err(ErrorReply::new(
				ErrorCode::InvalidGrant,
				"the enrollment key has expired",
			));
		}
		if !key.reusable && key.used {
			return Err(ErrorReply::new(
				ErrorCode::InvalidGrant,
				"the enrollment key was already used",
			));
		}
		let tags = key.tags.clone();
		let reusable = key.reusable;
		let id = id.to_string();

		let endpoint = request.proof.endpoint;
		let enrollment = self.commit(&mut inner, |store| {
			self.admit(store, endpoint, request.name.as_deref(), tags)?;
			if !reusable && let Some(key) = store.keys.iter_mut().find(|key| key.id == id) {
				key.used = true;
			}
			self.issue(store, endpoint)
		})?;
		inner.used.insert(endpoint, sequence);
		Ok(enrollment)
	}

	/// Refresh an enrolled device's token, refusing a revoked or unknown one.
	pub fn refresh(&self, proof: &Proof) -> Reply<Enrollment> {
		let mut inner = self.inner.lock().unwrap();
		let sequence = self.check(&inner, proof)?;
		let enrollment = match inner
			.store
			.devices
			.iter()
			.find(|device| device.endpoint == proof.endpoint)
		{
			Some(device) if !device.revoked => self.issue(&inner.store, proof.endpoint)?,
			Some(_) => return Err(ErrorReply::new(ErrorCode::AccessDenied, "the device is revoked")),
			None => return Err(ErrorReply::new(ErrorCode::AccessDenied, "the device is not enrolled")),
		};
		inner.used.insert(proof.endpoint, sequence);
		Ok(enrollment)
	}

	/// Create an enrollment key, returning it with its secret, which is never shown again.
	pub fn create_key(&self, new: &NewKey) -> crate::Result<KeyInfo> {
		let mut inner = self.inner.lock().unwrap();
		let id = hex(&random(8));
		let secret = encode(&random(32));
		let stored = StoredKey {
			id: id.clone(),
			hash: hex(&sha256(secret.as_bytes())),
			tags: new.tags.clone(),
			expires: new.expires_in.map(|ttl| moq_clock::wall().saturating_add(ttl)),
			reusable: new.reusable,
			used: false,
			revoked: false,
		};
		let mut info = KeyInfo::from(&stored);
		info.key = Some(format!("{id}.{secret}"));
		self.commit(&mut inner, |store| {
			store.keys.push(stored);
			Ok(())
		})?;
		Ok(info)
	}

	/// Every enrollment key, without secrets.
	pub fn keys(&self) -> Vec<KeyInfo> {
		self.inner
			.lock()
			.unwrap()
			.store
			.keys
			.iter()
			.map(KeyInfo::from)
			.collect()
	}

	/// Revoke an enrollment key so it enrolls nothing new. Devices it enrolled stay.
	/// Returns whether the key exists.
	pub fn revoke_key(&self, id: &str) -> crate::Result<bool> {
		let mut inner = self.inner.lock().unwrap();
		let found = self.commit(&mut inner, |store| {
			let key = store.keys.iter_mut().find(|key| key.id == id);
			Ok(key.map(|key| key.revoked = true).is_some())
		})?;
		Ok(found)
	}

	/// Every device, revoked ones included.
	pub fn devices(&self) -> Vec<Device> {
		self.inner.lock().unwrap().store.devices.clone()
	}

	/// Revoke a device: its next refresh fails, and it cannot enroll again.
	/// Returns whether the device exists.
	pub fn revoke_device(&self, endpoint: EndpointId) -> crate::Result<bool> {
		let mut inner = self.inner.lock().unwrap();
		let found = self.commit(&mut inner, |store| {
			let device = store.devices.iter_mut().find(|device| device.endpoint == endpoint);
			Ok(device.map(|device| device.revoked = true).is_some())
		})?;
		Ok(found)
	}

	/// The device-facing contract, mounted at [`Config::url`]'s path.
	pub fn router(&self) -> Router {
		let router = Router::new()
			.route("/challenge", post(challenge_handler))
			.route("/enroll", post(enroll_handler))
			.route("/refresh", post(refresh_handler))
			.with_state(self.clone());
		match self.config.url.path().trim_end_matches('/') {
			"" => router,
			base => Router::new().nest(base, router),
		}
	}

	/// The owner's API under `/mesh`, with no authentication of its own: mount it only on
	/// a listener the owner alone reaches.
	pub fn admin_router(&self) -> Router {
		Router::new()
			.route("/mesh/keys", get(keys_handler).post(create_key_handler))
			.route("/mesh/keys/{id}", delete(revoke_key_handler))
			.route("/mesh/devices", get(devices_handler))
			.route("/mesh/devices/{endpoint}", delete(revoke_device_handler))
			.with_state(self.clone())
	}

	/// Serve the device-facing contract on a TCP listener until the task is dropped.
	pub async fn serve(&self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
		axum::serve(listener, self.router()).await
	}

	/// Verify the proof's challenge and signature, returning the challenge's sequence.
	///
	/// The caller records the sequence as the endpoint's last only once the request is
	/// granted, so a refused proof changes nothing.
	fn check(&self, inner: &Inner, proof: &Proof) -> Reply<u64> {
		let invalid = |reason: &str| ErrorReply::new(ErrorCode::InvalidChallenge, reason);
		let challenge = &proof.challenge;
		if challenge.len() != CHALLENGE_LEN
			|| !constant_eq(
				&self.tag(&challenge[..CHALLENGE_LEN - TAG_LEN]),
				&challenge[CHALLENGE_LEN - TAG_LEN..],
			) {
			return Err(invalid("unknown challenge"));
		}
		let expires = u64::from_be_bytes(challenge[..8].try_into().expect("8 bytes"));
		let expires = moq_clock::Wall::UNIX_EPOCH.saturating_add(Duration::from_secs(expires));
		if expires <= moq_clock::wall() {
			return Err(invalid("the challenge expired"));
		}
		if !proof.verify(&self.config.url) {
			return Err(ErrorReply::new(
				ErrorCode::InvalidSignature,
				"the signature does not verify against the endpoint ID",
			));
		}
		let sequence = u64::from_be_bytes(challenge[8..16].try_into().expect("8 bytes"));
		if inner.used.get(&proof.endpoint).is_some_and(|last| sequence <= *last) {
			return Err(invalid("the endpoint already used this challenge or a newer one"));
		}
		Ok(sequence)
	}

	/// The truncated HMAC sealing a challenge's expiry and nonce.
	fn tag(&self, body: &[u8]) -> Vec<u8> {
		aws_lc_rs::hmac::sign(&self.secret, body).as_ref()[..TAG_LEN].to_vec()
	}

	/// Apply `change` to a copy of the store and save it, adopting the copy only once it
	/// is on disk, so a failed save leaves no credential spent and no device admitted.
	fn commit<T>(&self, inner: &mut Inner, change: impl FnOnce(&mut Store) -> Reply<T>) -> Reply<T> {
		let mut store = inner.store.clone();
		let out = change(&mut store)?;
		self.save(&store)?;
		inner.store = store;
		Ok(out)
	}

	/// Record `endpoint` as enrolled, or update its name and tags if it already is.
	fn admit(&self, store: &mut Store, endpoint: EndpointId, name: Option<&str>, tags: Vec<String>) -> Reply<()> {
		refuse_revoked(store, endpoint)?;
		let devices = &mut store.devices;
		let current = devices.iter().position(|device| device.endpoint == endpoint);
		let name = match (name, current) {
			(None, Some(index)) => devices[index].name.clone(),
			(name, _) => unique_name(devices, endpoint, &sanitize(name.unwrap_or_default())),
		};
		match current {
			Some(index) => {
				devices[index].name = name;
				devices[index].tags = tags;
			}
			None => devices.push(Device {
				endpoint,
				name,
				tags,
				enrolled: moq_clock::wall(),
				revoked: false,
			}),
		}
		Ok(())
	}

	/// Sign a token for an enrolled `endpoint` and list the mesh.
	fn issue(&self, store: &Store, endpoint: EndpointId) -> Reply<Enrollment> {
		let internal = |err: crate::Error| ErrorReply::new(ErrorCode::ServerError, err.to_string());
		let now = moq_clock::wall();
		// `exp` is whole seconds on the wire; truncate so the reply matches the token.
		let secs = now
			.checked_duration_since(moq_clock::Wall::UNIX_EPOCH)
			.unwrap_or_default()
			.as_secs();
		let expires = moq_clock::Wall::UNIX_EPOCH.saturating_add(Duration::from_secs(secs) + self.config.token_ttl);
		let token = self
			.config
			.key
			.sign(&claims(endpoint, now, Some(expires)))
			.map_err(internal)?;
		let device = store
			.devices
			.iter()
			.find(|device| device.endpoint == endpoint)
			.expect("admitted before it is issued");
		Ok(Enrollment {
			token,
			expires,
			relays: self.config.relays.clone(),
			name: device.name.clone(),
			members: store
				.devices
				.iter()
				.filter(|device| !device.revoked)
				.map(|device| Member {
					endpoint: device.endpoint,
					name: device.name.clone(),
					tags: device.tags.clone(),
				})
				.collect(),
		})
	}

	/// Write the store atomically: a temporary file beside it, renamed over it.
	fn save(&self, store: &Store) -> Reply<()> {
		let Some(path) = &self.config.state else {
			return Ok(());
		};
		let failed = |err: std::io::Error| {
			tracing::error!(path = %path.display(), %err, "failed to save the mesh state");
			ErrorReply::new(ErrorCode::ServerError, "the coordinator could not save its state")
		};
		let json = serde_json::to_string_pretty(store).expect("the store serializes");
		let tmp = path.with_extension("tmp");
		crate::fs::write_private(&tmp, &json).map_err(failed)?;
		std::fs::rename(&tmp, path).map_err(failed)
	}
}

fn refuse_revoked(store: &Store, endpoint: EndpointId) -> Reply<()> {
	if store
		.devices
		.iter()
		.any(|device| device.endpoint == endpoint && device.revoked)
	{
		return Err(ErrorReply::new(ErrorCode::AccessDenied, "the device is revoked"));
	}
	Ok(())
}

/// The grant every mesh token carries: publish under its own ID, subscribe to the mesh.
fn claims(endpoint: EndpointId, issued: moq_clock::Wall, expires: Option<moq_clock::Wall>) -> Claims {
	Claims::default()
		.with_publish([endpoint.publish()])
		.with_subscribe([Pattern::subtree("mesh").expect("a valid path")])
		.with_issued(issued)
		.with_expires(expires)
}

/// `name` as one DNS label: lowercase ASCII letters, digits, and inner hyphens, at most
/// 63 bytes, `device` when nothing is left.
fn sanitize(name: &str) -> String {
	let mut out = String::with_capacity(name.len().min(MAX_NAME));
	for c in name.chars() {
		let c = c.to_ascii_lowercase();
		let c = if c.is_ascii_alphanumeric() { c } else { '-' };
		if c == '-' && (out.is_empty() || out.ends_with('-')) {
			continue;
		}
		out.push(c);
	}
	out.truncate(MAX_NAME);
	let out = out.trim_end_matches('-');
	if out.is_empty() { "device".into() } else { out.into() }
}

/// `name`, or `name-2`, `name-3`, ... when another endpoint holds it.
fn unique_name(devices: &[Device], endpoint: EndpointId, name: &str) -> String {
	let taken = |candidate: &str| {
		devices
			.iter()
			.any(|device| device.endpoint != endpoint && device.name == candidate)
	};
	if !taken(name) {
		return name.into();
	}
	(2..)
		.map(|n| {
			let suffix = format!("-{n}");
			let base = &name[..name.len().min(MAX_NAME - suffix.len())];
			format!("{}{suffix}", base.trim_end_matches('-'))
		})
		.find(|candidate| !taken(candidate))
		.expect("an unbounded search finds a free name")
}

/// base64url without padding, the encoding of every random secret in the contract.
fn encode(bytes: &[u8]) -> String {
	use base64::Engine;
	base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random(len: usize) -> Vec<u8> {
	let mut bytes = vec![0u8; len];
	aws_lc_rs::rand::fill(&mut bytes).expect("system randomness");
	bytes
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
	aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes)
		.as_ref()
		.to_vec()
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
	aws_lc_rs::constant_time::verify_slices_are_equal(a, b).is_ok()
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl IntoResponse for ErrorReply {
	fn into_response(self) -> Response {
		// RFC 6749 §5.2: 400 for every OAuth error except a failed client authentication.
		let status = match self.error {
			ErrorCode::ServerError => StatusCode::INTERNAL_SERVER_ERROR,
			_ => StatusCode::BAD_REQUEST,
		};
		(status, Json(self)).into_response()
	}
}

/// A JSON body that refuses with the contract's error shape rather than axum's text.
struct Body<T>(T);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> axum::extract::FromRequest<S> for Body<T> {
	type Rejection = ErrorReply;

	async fn from_request(request: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
		Json::<T>::from_request(request, state)
			.await
			.map(|Json(body)| Self(body))
			.map_err(|err| ErrorReply::new(ErrorCode::InvalidRequest, err.body_text()))
	}
}

async fn challenge_handler(State(coordinator): State<Coordinator>) -> Json<Challenge> {
	Json(coordinator.challenge())
}

async fn enroll_handler(
	State(coordinator): State<Coordinator>,
	Body(request): Body<EnrollRequest>,
) -> Reply<Json<Enrollment>> {
	coordinator.enroll(&request).map(Json)
}

async fn refresh_handler(State(coordinator): State<Coordinator>, Body(proof): Body<Proof>) -> Reply<Json<Enrollment>> {
	coordinator.refresh(&proof).map(Json)
}

fn admin_error(err: crate::Error) -> Response {
	(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
}

async fn keys_handler(State(coordinator): State<Coordinator>) -> Json<Vec<KeyInfo>> {
	Json(coordinator.keys())
}

async fn create_key_handler(State(coordinator): State<Coordinator>, Body(new): Body<NewKey>) -> Response {
	match coordinator.create_key(&new) {
		Ok(info) => (StatusCode::CREATED, Json(info)).into_response(),
		Err(err) => admin_error(err),
	}
}

async fn revoke_key_handler(State(coordinator): State<Coordinator>, Path(id): Path<String>) -> Response {
	match coordinator.revoke_key(&id) {
		Ok(true) => StatusCode::NO_CONTENT.into_response(),
		Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
		Err(err) => admin_error(err),
	}
}

async fn devices_handler(State(coordinator): State<Coordinator>) -> Json<Vec<Device>> {
	Json(coordinator.devices())
}

async fn revoke_device_handler(State(coordinator): State<Coordinator>, Path(endpoint): Path<String>) -> Response {
	let Ok(endpoint) = endpoint.parse() else {
		return (StatusCode::BAD_REQUEST, "invalid endpoint ID").into_response();
	};
	match coordinator.revoke_device(endpoint) {
		Ok(true) => StatusCode::NO_CONTENT.into_response(),
		Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
		Err(err) => admin_error(err),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Algorithm, KeyId};

	#[test]
	fn names_are_dns_labels() {
		assert_eq!(sanitize("Luke's MacBook Pro"), "luke-s-macbook-pro");
		assert_eq!(sanitize("--robot_01.local--"), "robot-01-local");
		assert_eq!(sanitize("日本"), "device");
		assert_eq!(sanitize(&"a".repeat(100)).len(), MAX_NAME);
	}

	#[test]
	fn names_stay_unique() {
		let device = |byte, name: &str| Device {
			endpoint: EndpointId::from_bytes([byte; 32]),
			name: name.into(),
			tags: Vec::new(),
			enrolled: moq_clock::wall(),
			revoked: false,
		};
		let devices = vec![device(1, "robot"), device(2, "robot-2")];
		assert_eq!(unique_name(&devices, EndpointId::from_bytes([1; 32]), "robot"), "robot");
		assert_eq!(
			unique_name(&devices, EndpointId::from_bytes([3; 32]), "robot"),
			"robot-3"
		);
		let long = "a".repeat(MAX_NAME);
		let devices = vec![device(1, &long)];
		let name = unique_name(&devices, EndpointId::from_bytes([3; 32]), &long);
		assert_eq!(name.len(), MAX_NAME);
		assert!(name.ends_with("-2"));
	}

	#[test]
	fn a_challenge_is_single_use_per_endpoint() {
		let key = Key::generate(Algorithm::HS256, None).unwrap();
		let base: url::Url = "https://mesh.example".parse().unwrap();
		let coordinator = Coordinator::new(Config::new(base.clone(), key)).unwrap();
		let secret = coordinator
			.create_key(&NewKey {
				reusable: true,
				..Default::default()
			})
			.unwrap()
			.key
			.unwrap();
		let a = aws_lc_rs::signature::Ed25519KeyPair::generate().unwrap();
		let b = aws_lc_rs::signature::Ed25519KeyPair::generate().unwrap();
		let enroll = |signer: &aws_lc_rs::signature::Ed25519KeyPair, challenge: &Challenge| EnrollRequest {
			proof: Proof::sign(signer, &base, challenge.challenge.clone()),
			key: secret.clone(),
			name: None,
		};
		let older = coordinator.challenge();
		let newer = coordinator.challenge();

		// A refused proof records nothing, so an unknown caller holds no state.
		assert_eq!(
			coordinator.refresh(&enroll(&a, &older).proof).unwrap_err().error,
			ErrorCode::AccessDenied
		);
		assert!(coordinator.inner.lock().unwrap().used.is_empty());

		// Once `a` uses the newer challenge, its own older one is spent too.
		coordinator.enroll(&enroll(&a, &newer)).unwrap();
		for challenge in [&newer, &older] {
			let err = coordinator.enroll(&enroll(&a, challenge)).unwrap_err();
			assert_eq!(err.error, ErrorCode::InvalidChallenge);
		}

		// Another endpoint's state is its own: `a` spending challenges costs `b` nothing.
		coordinator.enroll(&enroll(&b, &older)).unwrap();
		coordinator.enroll(&enroll(&b, &newer)).unwrap();
		assert_eq!(coordinator.inner.lock().unwrap().used.len(), 2);
	}

	#[test]
	fn state_survives_a_restart() {
		let dir = tempfile::tempdir().unwrap();
		let mut config = Config::new(
			"https://mesh.example".parse().unwrap(),
			Key::generate(Algorithm::HS256, Some(KeyId::decode("mesh").unwrap())).unwrap(),
		);
		config.state = Some(dir.path().join("mesh.json"));
		let coordinator = Coordinator::new(config.clone()).unwrap();
		let key = coordinator.create_key(&NewKey::default()).unwrap();
		assert!(coordinator.revoke_key(&key.id).unwrap());

		let restarted = Coordinator::new(config).unwrap();
		let keys = restarted.keys();
		assert_eq!(keys.len(), 1);
		assert!(keys[0].revoked);
		assert_eq!(keys[0].key, None);
	}
}
