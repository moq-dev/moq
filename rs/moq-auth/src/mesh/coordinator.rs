//! The reference coordinator behind `moq auth serve --mesh-url`.
//!
//! [`Coordinator::router`] answers the device-facing contract; [`Coordinator::admin_router`]
//! is the owner's API, mounted on the auth server's own listener, which has no
//! authentication and stays on loopback. Enrollment keys and devices persist to a JSON
//! state file; challenges and pending device grants live in memory, so a restart only
//! makes a device fetch a new challenge or start its grant again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use moq_clock::Instant;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use super::{
	CHALLENGE_LEN, Challenge, DeviceAuthorization, DeviceRequest, DeviceTokenRequest, EndpointId, EnrollRequest,
	Enrollment, ErrorCode, ErrorReply, Member, Proof,
};
use crate::{Claims, Key, Pattern};

/// How long a challenge stays valid.
const CHALLENGE_TTL: Duration = Duration::from_secs(60);

/// The most challenges outstanding at once, so an unauthenticated caller cannot grow the
/// table without bound; the oldest is dropped first.
const MAX_CHALLENGES: usize = 4096;

/// The most device grants pending at once, for the same reason.
const MAX_PENDING: usize = 256;

/// The alphabet of a user code: no vowels, so a code spells no word, and no
/// look-alikes (RFC 8628 §6.1).
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

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
	/// How long a device grant waits for a human.
	pub device_ttl: Duration,
	/// The seconds a device waits between device grant polls.
	pub device_interval: Duration,
}

impl Config {
	/// A coordinator at `url` signing with `key`, with hour-long tokens, ten-minute device
	/// grants polled every five seconds, no relays, and no state file.
	pub fn new(url: url::Url, key: Key) -> Self {
		Self {
			url,
			key,
			relays: Vec::new(),
			state: None,
			token_ttl: Duration::from_secs(3600),
			device_ttl: Duration::from_secs(600),
			device_interval: Duration::from_secs(5),
		}
	}
}

/// The state that survives a restart.
#[derive(Default, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct NewKey {
	/// Tags every device it enrolls carries.
	pub tags: Vec<String>,
	/// Seconds until it stops enrolling; `None` never expires.
	pub expires_in: Option<u64>,
	/// Whether it enrolls more than one device.
	pub reusable: bool,
}

/// A device grant waiting for a human, as the owner sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PendingDevice {
	/// The code the device shows its human.
	pub user_code: String,
	/// The endpoint the grant would enroll.
	pub endpoint: EndpointId,
	/// The name the device asked for.
	pub name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
	Waiting,
	Approved,
	Denied,
}

struct Pending {
	user_code: String,
	endpoint: EndpointId,
	name: Option<String>,
	expires: Instant,
	polled: Option<Instant>,
	decision: Decision,
}

#[derive(Default)]
struct Inner {
	store: Store,
	/// Outstanding challenges and when each was issued.
	challenges: HashMap<Vec<u8>, Instant>,
	/// Pending device grants, by the SHA-256 of their device code.
	pending: HashMap<Vec<u8>, Pending>,
}

/// The mesh coordinator: issues challenges, enrolls devices, and refreshes their tokens.
#[derive(Clone)]
pub struct Coordinator {
	config: Arc<Config>,
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
		Ok(Self {
			config: Arc::new(config),
			inner: Arc::new(Mutex::new(Inner {
				store,
				..Default::default()
			})),
		})
	}

	/// Issue a fresh challenge.
	pub fn challenge(&self) -> Challenge {
		let challenge = random(CHALLENGE_LEN);
		let mut inner = self.inner.lock().unwrap();
		let now = Instant::now();
		inner
			.challenges
			.retain(|_, issued| now.saturating_duration_since(*issued) < CHALLENGE_TTL);
		if inner.challenges.len() >= MAX_CHALLENGES
			&& let Some(oldest) = inner
				.challenges
				.iter()
				.min_by_key(|(_, issued)| **issued)
				.map(|(challenge, _)| challenge.clone())
		{
			inner.challenges.remove(&oldest);
		}
		inner.challenges.insert(challenge.clone(), now);
		Challenge {
			challenge,
			expires: moq_clock::wall() + CHALLENGE_TTL,
		}
	}

	/// Enroll with an enrollment key.
	pub fn enroll(&self, request: &EnrollRequest) -> Reply<Enrollment> {
		let mut inner = self.inner.lock().unwrap();
		self.check(&mut inner, &request.proof)?;

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
		self.admit(&mut inner, endpoint, request.name.as_deref(), tags)?;
		if !reusable && let Some(key) = inner.store.keys.iter_mut().find(|key| key.id == id) {
			key.used = true;
		}
		self.save(&inner)?;
		self.issue(&inner, endpoint)
	}

	/// Start a device grant for a human to approve.
	pub fn device(&self, request: &DeviceRequest) -> Reply<DeviceAuthorization> {
		let mut inner = self.inner.lock().unwrap();
		self.check(&mut inner, &request.proof)?;
		self.refuse_revoked(&inner, request.proof.endpoint)?;

		let now = Instant::now();
		inner.pending.retain(|_, pending| pending.expires > now);
		if inner.pending.len() >= MAX_PENDING {
			return Err(ErrorReply::new(
				ErrorCode::SlowDown,
				"too many device grants are pending; try again later",
			));
		}

		let device_code = encode(&random(32));
		let user_code = loop {
			let code = user_code();
			if !inner.pending.values().any(|pending| pending.user_code == code) {
				break code;
			}
		};
		inner.pending.insert(
			sha256(device_code.as_bytes()),
			Pending {
				user_code: user_code.clone(),
				endpoint: request.proof.endpoint,
				name: request.name.clone(),
				expires: now + self.config.device_ttl,
				polled: None,
				decision: Decision::Waiting,
			},
		);

		Ok(DeviceAuthorization {
			device_code,
			user_code,
			verification_uri: self.verification_uri(),
			expires_in: self.config.device_ttl.as_secs(),
			interval: self.config.device_interval.as_secs(),
		})
	}

	/// Poll a device grant: the enrollment once a human approves, a refusal until then.
	pub fn device_token(&self, request: &DeviceTokenRequest) -> Reply<Enrollment> {
		let mut inner = self.inner.lock().unwrap();
		let hash = sha256(request.device_code.as_bytes());
		let now = Instant::now();
		let pending = inner
			.pending
			.get_mut(&hash)
			.ok_or_else(|| ErrorReply::new(ErrorCode::ExpiredToken, "unknown or expired device code"))?;
		if pending.expires <= now {
			inner.pending.remove(&hash);
			return Err(ErrorReply::new(ErrorCode::ExpiredToken, "the device grant expired"));
		}
		let fast = pending
			.polled
			.is_some_and(|polled| now.saturating_duration_since(polled) < self.config.device_interval);
		pending.polled = Some(now);
		match pending.decision {
			Decision::Waiting if fast => Err(ErrorReply::new(ErrorCode::SlowDown, "polling faster than the interval")),
			Decision::Waiting => Err(ErrorReply::new(ErrorCode::AuthorizationPending, "waiting for approval")),
			Decision::Denied => {
				inner.pending.remove(&hash);
				Err(ErrorReply::new(ErrorCode::AccessDenied, "the device grant was denied"))
			}
			Decision::Approved => {
				let pending = inner.pending.remove(&hash).expect("found above");
				self.admit(&mut inner, pending.endpoint, pending.name.as_deref(), Vec::new())?;
				self.save(&inner)?;
				self.issue(&inner, pending.endpoint)
			}
		}
	}

	/// Refresh an enrolled device's token, refusing a revoked or unknown one.
	pub fn refresh(&self, proof: &Proof) -> Reply<Enrollment> {
		let mut inner = self.inner.lock().unwrap();
		self.check(&mut inner, proof)?;
		match inner
			.store
			.devices
			.iter()
			.find(|device| device.endpoint == proof.endpoint)
		{
			Some(device) if !device.revoked => self.issue(&inner, proof.endpoint),
			Some(_) => Err(ErrorReply::new(ErrorCode::AccessDenied, "the device is revoked")),
			None => Err(ErrorReply::new(ErrorCode::AccessDenied, "the device is not enrolled")),
		}
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
			expires: new
				.expires_in
				.map(|secs| moq_clock::wall().saturating_add(Duration::from_secs(secs))),
			reusable: new.reusable,
			used: false,
			revoked: false,
		};
		let mut info = KeyInfo::from(&stored);
		info.key = Some(format!("{id}.{secret}"));
		inner.store.keys.push(stored);
		self.save(&inner)
			.map_err(|err| crate::Error::MeshState(err.to_string()))?;
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
		let Some(key) = inner.store.keys.iter_mut().find(|key| key.id == id) else {
			return Ok(false);
		};
		key.revoked = true;
		self.save(&inner)
			.map_err(|err| crate::Error::MeshState(err.to_string()))?;
		Ok(true)
	}

	/// Every device, revoked ones included.
	pub fn devices(&self) -> Vec<Device> {
		self.inner.lock().unwrap().store.devices.clone()
	}

	/// Revoke a device: its next refresh fails, and it cannot enroll again.
	/// Returns whether the device exists.
	pub fn revoke_device(&self, endpoint: EndpointId) -> crate::Result<bool> {
		let mut inner = self.inner.lock().unwrap();
		let Some(device) = inner
			.store
			.devices
			.iter_mut()
			.find(|device| device.endpoint == endpoint)
		else {
			return Ok(false);
		};
		device.revoked = true;
		self.save(&inner)
			.map_err(|err| crate::Error::MeshState(err.to_string()))?;
		Ok(true)
	}

	/// The device grants waiting for a human.
	pub fn pending(&self) -> Vec<PendingDevice> {
		let mut inner = self.inner.lock().unwrap();
		let now = Instant::now();
		inner.pending.retain(|_, pending| pending.expires > now);
		inner
			.pending
			.values()
			.filter(|pending| pending.decision == Decision::Waiting)
			.map(|pending| PendingDevice {
				user_code: pending.user_code.clone(),
				endpoint: pending.endpoint,
				name: pending.name.clone(),
			})
			.collect()
	}

	/// Approve or deny the waiting device grant showing `user_code`. Returns whether one
	/// was waiting.
	pub fn decide(&self, user_code: &str, approve: bool) -> bool {
		let user_code = normalize_user_code(user_code);
		let mut inner = self.inner.lock().unwrap();
		let now = Instant::now();
		let Some(pending) = inner.pending.values_mut().find(|pending| {
			pending.user_code == user_code && pending.decision == Decision::Waiting && pending.expires > now
		}) else {
			return false;
		};
		pending.decision = if approve { Decision::Approved } else { Decision::Denied };
		true
	}

	/// The device-facing contract, to serve at [`Config::url`].
	pub fn router(&self) -> Router {
		Router::new()
			.route("/challenge", post(challenge_handler))
			.route("/enroll", post(enroll_handler))
			.route("/device", post(device_handler).get(verification_handler))
			.route("/device/token", post(device_token_handler))
			.route("/refresh", post(refresh_handler))
			.with_state(self.clone())
	}

	/// The owner's API under `/mesh`, with no authentication of its own: mount it only on
	/// a listener the owner alone reaches.
	pub fn admin_router(&self) -> Router {
		Router::new()
			.route("/mesh/keys", get(keys_handler).post(create_key_handler))
			.route("/mesh/keys/{id}", delete(revoke_key_handler))
			.route("/mesh/devices", get(devices_handler))
			.route("/mesh/devices/{endpoint}", delete(revoke_device_handler))
			.route("/mesh/pending", get(pending_handler))
			.route("/mesh/pending/{code}/approve", post(approve_handler))
			.route("/mesh/pending/{code}/deny", post(deny_handler))
			.with_state(self.clone())
	}

	/// Serve the device-facing contract on a TCP listener until the task is dropped.
	pub async fn serve(&self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
		axum::serve(listener, self.router()).await
	}

	fn verification_uri(&self) -> url::Url {
		let mut url = self.config.url.clone();
		let path = format!("{}/device", url.path().trim_end_matches('/'));
		url.set_path(&path);
		url
	}

	/// Consume the proof's challenge and verify its signature.
	fn check(&self, inner: &mut Inner, proof: &Proof) -> Reply<()> {
		// Consumed even when the signature fails, so one challenge is one attempt.
		let issued = inner
			.challenges
			.remove(&proof.challenge)
			.ok_or_else(|| ErrorReply::new(ErrorCode::InvalidChallenge, "unknown or used challenge"))?;
		if Instant::now().saturating_duration_since(issued) >= CHALLENGE_TTL {
			return Err(ErrorReply::new(ErrorCode::InvalidChallenge, "the challenge expired"));
		}
		if !proof.verify(&self.config.url) {
			return Err(ErrorReply::new(
				ErrorCode::InvalidSignature,
				"the signature does not verify against the endpoint ID",
			));
		}
		Ok(())
	}

	fn refuse_revoked(&self, inner: &Inner, endpoint: EndpointId) -> Reply<()> {
		if inner
			.store
			.devices
			.iter()
			.any(|device| device.endpoint == endpoint && device.revoked)
		{
			return Err(ErrorReply::new(ErrorCode::AccessDenied, "the device is revoked"));
		}
		Ok(())
	}

	/// Record `endpoint` as enrolled, or update its name and tags if it already is.
	fn admit(&self, inner: &mut Inner, endpoint: EndpointId, name: Option<&str>, tags: Vec<String>) -> Reply<()> {
		self.refuse_revoked(inner, endpoint)?;
		let devices = &mut inner.store.devices;
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
	fn issue(&self, inner: &Inner, endpoint: EndpointId) -> Reply<Enrollment> {
		let internal = |err: crate::Error| ErrorReply::new(ErrorCode::ServerError, err.to_string());
		let now = moq_clock::wall();
		// `exp` is whole seconds on the wire; truncate so the reply matches the token.
		let secs = now
			.checked_duration_since(moq_clock::Wall::UNIX_EPOCH)
			.unwrap_or_default()
			.as_secs();
		let expires = moq_clock::Wall::UNIX_EPOCH.saturating_add(Duration::from_secs(secs) + self.config.token_ttl);
		let claims = Claims::default()
			.with_publish([endpoint.publish()])
			.with_subscribe([Pattern::subtree("mesh").expect("a valid path")])
			.with_issued(now)
			.with_expires(expires);
		let token = self.config.key.sign(&claims).map_err(internal)?;
		let device = inner
			.store
			.devices
			.iter()
			.find(|device| device.endpoint == endpoint)
			.expect("admitted before it is issued");
		Ok(Enrollment {
			token,
			expires,
			relays: self.config.relays.clone(),
			name: device.name.clone(),
			members: inner
				.store
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
	fn save(&self, inner: &Inner) -> Reply<()> {
		let Some(path) = &self.config.state else {
			return Ok(());
		};
		let failed = |err: std::io::Error| {
			tracing::error!(path = %path.display(), %err, "failed to save the mesh state");
			ErrorReply::new(ErrorCode::ServerError, "the coordinator could not save its state")
		};
		let json = serde_json::to_string_pretty(&inner.store).expect("the store serializes");
		let tmp = path.with_extension("tmp");
		crate::fs::write_private(&tmp, &json).map_err(failed)?;
		std::fs::rename(&tmp, path).map_err(failed)
	}
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

/// Eight characters from [`USER_CODE_ALPHABET`] as `XXXX-XXXX`.
fn user_code() -> String {
	let bytes = random(8);
	let mut code = String::with_capacity(9);
	for (i, byte) in bytes.iter().enumerate() {
		if i == 4 {
			code.push('-');
		}
		// 256 is not a multiple of 20; the slight bias costs a fraction of a bit.
		code.push(USER_CODE_ALPHABET[*byte as usize % USER_CODE_ALPHABET.len()] as char);
	}
	code
}

/// A user code as typed: case and separators ignored, as RFC 8628 §6.1 advises.
fn normalize_user_code(code: &str) -> String {
	let letters: String = code
		.chars()
		.filter(char::is_ascii_alphanumeric)
		.map(|c| c.to_ascii_uppercase())
		.collect();
	if letters.len() == 8 {
		format!("{}-{}", &letters[..4], &letters[4..])
	} else {
		letters
	}
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

async fn device_handler(
	State(coordinator): State<Coordinator>,
	Body(request): Body<DeviceRequest>,
) -> Reply<Json<DeviceAuthorization>> {
	coordinator.device(&request).map(Json)
}

async fn verification_handler() -> &'static str {
	"Ask the mesh owner to approve your device: moq auth mesh approve <user code>\n"
}

async fn device_token_handler(
	State(coordinator): State<Coordinator>,
	Body(request): Body<DeviceTokenRequest>,
) -> Reply<Json<Enrollment>> {
	coordinator.device_token(&request).map(Json)
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

async fn pending_handler(State(coordinator): State<Coordinator>) -> Json<Vec<PendingDevice>> {
	Json(coordinator.pending())
}

async fn approve_handler(State(coordinator): State<Coordinator>, Path(code): Path<String>) -> Response {
	decide(&coordinator, &code, true)
}

async fn deny_handler(State(coordinator): State<Coordinator>, Path(code): Path<String>) -> Response {
	decide(&coordinator, &code, false)
}

/// A known route's miss carries a body, so the CLI can tell it from a missing route.
fn decide(coordinator: &Coordinator, code: &str, approve: bool) -> Response {
	if coordinator.decide(code, approve) {
		StatusCode::NO_CONTENT.into_response()
	} else {
		(StatusCode::NOT_FOUND, "not found").into_response()
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
	fn user_codes_read_back_however_typed() {
		let code = user_code();
		assert_eq!(code.len(), 9);
		assert_eq!(normalize_user_code(&code.to_lowercase().replace('-', " ")), code);
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
