//! The device side of the contract: enroll, then keep the token fresh.

use std::sync::Arc;
use std::time::Duration;

use url::Url;

use super::{Challenge, EnrollRequest, Enrollment, ErrorCode, ErrorReply, Proof, Signer};
use crate::Error;

/// Every request is bounded so a hung coordinator fails rather than parks the device.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The largest reply read from the coordinator; a mesh's member list fits easily.
const MAX_REPLY: usize = 4 << 20;

/// The longest wait between refresh attempts while the coordinator is unreachable.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A device's client for one coordinator, proving `signer`'s key on every request that
/// needs it.
#[derive(Clone)]
pub struct Client {
	http: reqwest::Client,
	base: Url,
	signer: Arc<dyn Signer>,
}

impl Client {
	/// A client for the coordinator at `base`, the URL the contract's paths hang off.
	///
	/// `https://` is required, except `http://` to a loopback host, since the token rides
	/// the reply.
	pub fn new(base: Url, signer: Arc<dyn Signer>) -> crate::Result<Self> {
		let loopback = match base.host() {
			Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
			Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
			Some(url::Host::Domain(host)) => host == "localhost",
			None => false,
		};
		match base.scheme() {
			"https" => {}
			"http" if loopback => {}
			"http" => return Err(Error::InsecureUrl(base.to_string())),
			_ => return Err(Error::InvalidUrl(base.to_string())),
		}
		// A redirect would resend a proof or enrollment key to wherever it points.
		let http = reqwest::Client::builder()
			.redirect(reqwest::redirect::Policy::none())
			.timeout(TIMEOUT)
			.build()?;
		Ok(Self { http, base, signer })
	}

	/// Join with a pre-authorized enrollment key, asking for `name`.
	///
	/// When the coordinator refuses the key but this endpoint is already enrolled, such as
	/// a retry after a single-use key's reply was lost, this refreshes instead, keeping the
	/// name the device already has.
	pub async fn enroll(&self, key: &str, name: Option<&str>) -> crate::Result<Enrollment> {
		let proof = self.proof().await?;
		let request = EnrollRequest {
			proof,
			key: key.into(),
			name: name.map(Into::into),
		};
		match self.post("enroll", &request).await {
			Err(Error::Mesh(refused)) if refused.error == ErrorCode::InvalidGrant => match self.refresh().await {
				// Not enrolled after all, so the key's refusal is the answer.
				Err(Error::Mesh(reply)) if reply.error == ErrorCode::AccessDenied => Err(Error::Mesh(refused)),
				res => res,
			},
			res => res,
		}
	}

	/// Prove the key again for a fresh token. Fails with [`ErrorCode::AccessDenied`] once
	/// the device is revoked.
	pub async fn refresh(&self) -> crate::Result<Enrollment> {
		let proof = self.proof().await?;
		self.post("refresh", &proof).await
	}

	/// Wait until `current` is two thirds through its remaining life, then refresh it,
	/// retrying through an outage until it expires. A refusal is returned at once, so a
	/// revoked device stops rather than retrying, except a transient one: a server error,
	/// `slow_down`, or a challenge that expired or outlived a coordinator restart.
	pub async fn renew(&self, current: &Enrollment) -> crate::Result<Enrollment> {
		let remaining = || {
			current
				.expires
				.checked_duration_since(moq_clock::wall())
				.unwrap_or_default()
		};
		moq_clock::sleep(remaining() * 2 / 3).await;

		let mut backoff = Duration::from_secs(1);
		loop {
			match self.refresh().await {
				Err(Error::Mesh(reply))
					if !matches!(
						reply.error,
						ErrorCode::ServerError | ErrorCode::SlowDown | ErrorCode::InvalidChallenge
					) =>
				{
					return Err(Error::Mesh(reply));
				}
				Ok(enrollment) => return Ok(enrollment),
				Err(err) if remaining().is_zero() => return Err(err),
				Err(err) => {
					tracing::warn!(%err, "mesh refresh failed; retrying");
					moq_clock::sleep(backoff.min(remaining())).await;
					backoff = (backoff * 2).min(MAX_BACKOFF);
				}
			}
		}
	}

	async fn proof(&self) -> crate::Result<Proof> {
		let challenge: Challenge = self.post("challenge", &serde_json::Value::Null).await?;
		Ok(Proof::sign(self.signer.as_ref(), &self.base, challenge.challenge))
	}

	async fn post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
		&self,
		path: &str,
		body: &T,
	) -> crate::Result<R> {
		let mut url = self.base.clone();
		let joined = format!("{}/{path}", url.path().trim_end_matches('/'));
		url.set_path(&joined);

		let request = self.http.post(url);
		let request = match serde_json::to_value(body)? {
			serde_json::Value::Null => request,
			body => request.json(&body),
		};
		let mut response = request.send().await?;
		let status = response.status();

		let mut bytes = Vec::new();
		while let Some(chunk) = response.chunk().await? {
			if bytes.len() + chunk.len() > MAX_REPLY {
				return Err(Error::Unavailable("the coordinator's reply is too large".into()));
			}
			bytes.extend_from_slice(&chunk);
		}

		if status.is_success() {
			return Ok(serde_json::from_slice(&bytes)?);
		}
		match serde_json::from_slice::<ErrorReply>(&bytes) {
			Ok(reply) => Err(Error::Mesh(reply)),
			Err(_) => Err(Error::Unavailable(format!("the coordinator answered {status}"))),
		}
	}
}
