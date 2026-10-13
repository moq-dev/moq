use std::collections::BTreeMap;

use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use moq_net::{Role, SessionError, auth};

use super::{Grant, Source, Token, lease};

/// A connection owns its lifecycle while each credential owns only its grant.
pub(super) struct Refresh {
	handle: auth::Handle,
	requests: auth::Requests,
	requests_open: bool,
	owner: lease::Consumer,
	source: Option<Source>,
	admitted: Token,
	stats: moq_net::stats::Session,
	role: Option<Role>,
	credentials: BTreeMap<u64, Credential>,
	/// At most 128 tokens being verified; more are refused with `TooManyRequests`.
	/// A withdrawn token keeps its slot until the auth server answers.
	pending: FuturesUnordered<BoxFuture<'static, Option<Verified>>>,
	next: u64,
}

type Verified = (auth::Request, Result<lease::Consumer, super::Error>);

struct Credential {
	consumer: lease::Consumer,
	grant: Grant,
	expires: Option<tokio::time::Instant>,
	issued: Option<auth::Issued>,
	initial: bool,
}

enum Change {
	Grant(Grant),
	Ended(lease::Reason),
	Withdrawn,
}

impl Credential {
	fn new(consumer: lease::Consumer) -> Self {
		let grant = consumer.grant();
		Self {
			expires: grant.deadline(),
			consumer,
			grant,
			issued: None,
			initial: false,
		}
	}

	async fn changed(&mut self) -> Change {
		let expire = async {
			match self.expires {
				Some(at) => tokio::time::sleep_until(at).await,
				None => std::future::pending().await,
			}
		};
		let withdrawn = async {
			match &self.issued {
				Some(issued) => {
					issued.closed().await;
				}
				None => std::future::pending().await,
			}
		};
		tokio::select! {
			biased;
			() = withdrawn => Change::Withdrawn,
			() = expire => Change::Ended(lease::Reason::Expired),
			changed = self.consumer.changed() => match changed {
				Ok(grant) => Change::Grant(grant),
				Err(reason) => Change::Ended(reason),
			},
		}
	}

	fn wire(&self, role: Option<Role>) -> auth::Grant {
		auth::Grant {
			publish: if role == Some(Role::Subscriber) {
				Default::default()
			} else {
				self.grant.publish.clone()
			},
			subscribe: if role == Some(Role::Publisher) {
				Default::default()
			} else {
				self.grant.subscribe.clone()
			},
			expires: self.expires.map(tokio::time::Instant::into_std),
		}
	}
}

impl Refresh {
	pub(super) fn new(
		lease: &mut super::Lease,
		handle: auth::Handle,
		role: Option<Role>,
	) -> Result<Self, moq_net::Error> {
		let requests = handle.requests()?;
		let mut initial = Credential::new(lease.consumer.clone());
		initial.initial = true;
		let this = Self {
			handle,
			requests,
			requests_open: true,
			owner: lease.consumer.clone(),
			source: lease.source.take(),
			admitted: lease.token.clone(),
			stats: lease.stats.clone(),
			role,
			credentials: [(0, initial)].into(),
			pending: FuturesUnordered::new(),
			next: 1,
		};
		this.authorize();
		Ok(this)
	}

	fn valid(&self, grant: &Grant) -> bool {
		let fresh = self.admitted.recheck(grant);
		fresh.root == self.admitted.root
			&& fresh.mounts == self.admitted.mounts
			&& fresh.peer == self.admitted.peer
			&& fresh.upstream == self.admitted.upstream
			&& match self.role {
				Some(Role::Publisher) => !grant.publish.is_empty(),
				Some(Role::Subscriber) => !grant.subscribe.is_empty(),
				Some(_) => false,
				None => !grant.publish.is_empty() || !grant.subscribe.is_empty(),
			}
	}

	fn authorize(&self) {
		let mut union = auth::Grant::default();
		for credential in self.credentials.values() {
			let grant = credential.wire(self.role);
			union.publish.extend(grant.publish.iter().cloned());
			union.subscribe.extend(grant.subscribe.iter().cloned());
		}
		// Credential timers remove one grant at a time; no single deadline may
		// expire a union that another credential still covers.
		self.handle.authorize(&union);
	}

	fn request(&mut self, request: auth::Request) {
		if request.token().is_empty() {
			// The connection's own lease answers an empty token while it lives, even
			// after an earlier empty-token stream was withdrawn.
			if self.owner.closed().now_or_never().is_some() {
				request.reject(SessionError::Unauthorized, "connection credential ended");
				return;
			}
			let unissued = self
				.credentials
				.values_mut()
				.find(|credential| credential.initial && credential.issued.is_none());
			if let Some(initial) = unissued {
				initial.issued = Some(request.accept(initial.wire(self.role)));
				return;
			}
			let mut credential = Credential::new(self.owner.clone());
			credential.initial = true;
			credential.issued = Some(request.accept(credential.wire(self.role)));
			self.credentials.insert(self.next, credential);
			self.next += 1;
			self.authorize();
			return;
		}
		let Some(source) = self.source.as_ref().filter(|source| source.auth.in_band) else {
			request.reject(SessionError::Unsupported, "tokens are not verified in band");
			return;
		};
		if source.request.tls.is_some() {
			request.reject(SessionError::Unsupported, "a certificate cannot be renewed in band");
			return;
		}
		if self.pending.len() >= 128 {
			request.reject(SessionError::TooManyRequests, "too many tokens being verified");
			return;
		}
		let Ok(token) = std::str::from_utf8(request.token()) else {
			request.reject(SessionError::Unauthorized, "token is not UTF-8");
			return;
		};
		let mut facts = source.request.clone();
		facts.query = Some(with_jwt(facts.query.as_deref(), token));
		facts.token = None;
		let auth = source.auth.clone();
		let owner = self.owner.clone();
		self.pending.push(Box::pin(async move {
			// Withdrawing the stream cannot cancel the auth server's work. Keep
			// its slot until verification finishes so churn cannot bypass the bound.
			let result = auth.decide(facts, Some(owner)).await;
			if request.closed().now_or_never().is_some() {
				return None;
			}
			Some((request, result))
		}));
	}

	pub(super) fn revalidate(&self) {
		for credential in self.credentials.values() {
			credential.consumer.revalidate();
		}
	}

	pub(super) async fn ended(&mut self) -> lease::Reason {
		loop {
			enum Event {
				Request(Option<auth::Request>),
				Verified(Option<Verified>),
				Changed(u64, Change),
			}
			let event = {
				let mut changes: FuturesUnordered<_> = self
					.credentials
					.iter_mut()
					.map(|(id, credential)| async move { (*id, credential.changed().await) })
					.collect();
				tokio::select! {
					biased;
					Some((id, change)) = changes.next(), if !changes.is_empty() => Event::Changed(id, change),
					Some(result) = self.pending.next(), if !self.pending.is_empty() => Event::Verified(result),
					request = self.requests.next(), if self.requests_open => Event::Request(request),
				}
			};
			match event {
				Event::Request(Some(request)) => self.request(request),
				Event::Request(None) => self.requests_open = false,
				Event::Verified(None) => {}
				Event::Verified(Some((request, result))) => match result {
					Ok(consumer) if self.valid(&consumer.grant()) => {
						let mut credential = Credential::new(consumer);
						credential.issued = Some(request.accept(credential.wire(self.role)));
						self.credentials.insert(self.next, credential);
						self.next += 1;
						self.authorize();
					}
					Ok(_) => request.reject(
						SessionError::Unauthorized,
						format!(
							"token must retain root {}, connection routing, and usable SETUP role permissions",
							self.admitted.root
						),
					),
					Err(_) => request.reject(SessionError::Unauthorized, "token refused"),
				},
				Event::Changed(id, Change::Grant(grant)) if self.valid(&grant) => {
					let credential = self.credentials.get_mut(&id).expect("observed credential");
					credential.expires = grant.deadline();
					if credential.initial {
						self.stats.set_tier(crate::configured_tier(grant.tier.clone()));
					}
					credential.grant = grant;
					if let Some(issued) = &credential.issued {
						issued.update(credential.wire(self.role));
					}
					self.authorize();
				}
				Event::Changed(id, change) => {
					let credential = self.credentials.remove(&id).expect("observed credential");
					let withdrawn = matches!(change, Change::Withdrawn);
					let reason = match change {
						Change::Ended(reason) => reason,
						Change::Withdrawn => lease::Reason::Dropped,
						Change::Grant(_) => lease::Reason::Narrowed,
					};
					if !withdrawn && let Some(issued) = credential.issued {
						issued.revoke(SessionError::Unauthorized, reason.to_string());
					}
					self.authorize();
					if self.credentials.is_empty() {
						return reason;
					}
				}
			}
		}
	}
}

/// Replace the `jwt` pair, leaving every other byte of the dialed query as it was,
/// since the auth server may interpret or authenticate its raw form.
fn with_jwt(query: Option<&str>, token: &str) -> String {
	let jwt = format!(
		"jwt={}",
		url::form_urlencoded::byte_serialize(token.as_bytes()).collect::<String>()
	);
	let mut pairs = Vec::new();
	let mut replaced = false;
	let query = query.unwrap_or_default();
	let segments = query.split('&').filter(|_| !query.is_empty());
	for pair in segments {
		let is_jwt = url::form_urlencoded::parse(pair.as_bytes())
			.next()
			.is_some_and(|(key, _)| key == "jwt");
		if !is_jwt {
			pairs.push(pair);
		} else if !replaced {
			pairs.push(&jwt);
			replaced = true;
		}
	}
	if !replaced {
		pairs.push(&jwt);
	}
	pairs.join("&")
}

#[cfg(test)]
mod tests {
	use super::*;
	use futures::FutureExt;
	use std::time::{Duration, SystemTime};

	fn grant(prefix: &str, seconds: u64) -> Grant {
		let patterns = [prefix.parse().unwrap()].into_iter().collect();
		let mut grant = Grant::new(patterns, Default::default());
		grant.expires = Some(SystemTime::now() + Duration::from_secs(seconds));
		grant
	}

	async fn sessions(lease: super::super::Lease) -> (moq_net::Session, tokio::task::JoinHandle<lease::Reason>) {
		let config = || {
			let mut config = qmux::Config::new(qmux::Version::QMux01);
			config.protocol = qmux::Protocol::Negotiate(
				moq_net::Versions::from("moq-lite-07-wip".parse::<moq_net::Version>().unwrap())
					.alpns()
					.iter()
					.map(|alpn| alpn.to_string())
					.collect(),
			);
			config
		};
		let stream = |io| qmux::transport::Stream::new(io, qmux::Version::QMux01, config().max_record_size);
		let (client, server) = tokio::io::duplex(64 * 1024);
		let (client, server) = tokio::try_join!(
			qmux::Session::connect(stream(client), config()),
			qmux::Session::accept(stream(server), config())
		)
		.unwrap();
		let now = tokio::time::Instant::now().into_std();
		let client = async {
			let origin = moq_tokio::origin::spawn();
			let (session, driver) = moq_net::Client::new()
				.with_versions("moq-lite-07-wip".parse::<moq_net::Version>().unwrap().into())
				.with_publisher(origin.consume())
				.connect(now, moq_tokio::transport::Session::new(client))
				.await
				.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		};
		let server = async {
			let origin = moq_tokio::origin::spawn();
			let request = moq_net::Server::new()
				.with_versions("moq-lite-07-wip".parse::<moq_net::Version>().unwrap().into())
				.with_subscriber(origin)
				.accept_request(now, moq_tokio::transport::Session::new(server))
				.await
				.unwrap();
			let mut lease = lease.preparing(request.auth(), request.role()).unwrap();
			let (session, driver) = request.ok().await.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			(tokio::spawn(async move {
				let reason = lease.ended().await;
				session.abort(moq_net::Error::Unauthorized);
				lease.close(reason.clone(), Default::default());
				reason
			}),)
		};
		let (client, (server,)) = tokio::join!(client, server);
		(client, server)
	}

	async fn admission(initial: Grant) -> (super::super::Lease, super::super::Admissions) {
		let (auth, mut admissions) = super::super::Auth::embedded("test");
		let request = auth.request(moq_auth::Transport::Quic, "/room");
		let (lease, ()) = tokio::join!(auth.admit(request), async {
			admissions.next().await.unwrap().grant(lease::Consumer::fixed(initial));
		});
		(lease.unwrap(), admissions)
	}

	#[tokio::test(start_paused = true)]
	async fn renewal_outlives_the_initial_credential_and_withdrawal_ends_only_its_grant() {
		let (lease, mut admissions) = admission(grant("a/**", 3)).await;
		let (client, mut supervising) = sessions(lease).await;
		let auth = client.auth();
		let (token, ()) = tokio::join!(auth.add("replacement"), async {
			let admission = admissions.next().await.unwrap();
			assert_eq!(admission.request.query.as_deref(), Some("jwt=replacement"));
			admission.grant(lease::Consumer::fixed(grant("a/**", 20)));
		});
		let token = token.unwrap();
		tokio::time::advance(Duration::from_secs(4)).await;
		tokio::task::yield_now().await;
		assert!(
			(&mut supervising).now_or_never().is_none(),
			"renewed connection ended with its first credential"
		);
		assert!(token.closed().now_or_never().is_none());
		drop(token);
		assert_eq!(supervising.await.unwrap(), lease::Reason::Dropped);
	}

	#[tokio::test(start_paused = true)]
	async fn refused_and_different_root_tokens_leave_the_session_grant_unchanged() {
		let (lease, mut admissions) = admission(grant("a/**", 30)).await;
		let (client, supervising) = sessions(lease).await;
		let auth = client.auth();
		for refusal in [0, 1, 2] {
			let (result, ()) = tokio::join!(auth.add("bad"), async {
				let admission = admissions.next().await.unwrap();
				if refusal == 1 {
					let mut other = grant("b/**", 30);
					other.root = Some("other".into());
					admission.grant(lease::Consumer::fixed(other));
				} else if refusal == 2 {
					let mut other = grant("b/**", 30);
					other.subscribe = other.publish.clone();
					other.publish = Default::default();
					admission.grant(lease::Consumer::fixed(other));
				} else {
					admission.refuse(super::super::Error::Refused);
				}
			});
			assert!(result.is_err());
		}
		assert!(!supervising.is_finished());
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}
	#[tokio::test(start_paused = true)]
	async fn attached_grants_update_union_and_withdraw_without_widening_setup_role() {
		let (lease, mut admissions) = admission(grant("a/**", 30)).await;
		let (client, supervising) = sessions(lease).await;
		let auth = client.auth();
		let mut added = grant("b/**", 30);
		added.subscribe = ["**".parse().unwrap()].into_iter().collect();
		let (producer, consumer) = lease::Producer::new(added);
		let (token, ()) = tokio::join!(auth.add("replacement"), async {
			admissions.next().await.unwrap().grant(consumer);
		});
		let token = token.unwrap();
		assert!(
			token.grant().peek().unwrap().subscribe.is_empty(),
			"publisher SETUP role widened"
		);
		let mut union = auth.grant();
		let current = union.peek().unwrap();
		assert!(current.publish.matches("a/cam"));
		assert!(current.publish.matches("b/cam"));
		producer.update(grant("c/**", 30));
		let updated = union.changed().await.unwrap().unwrap();
		assert!(updated.publish.matches("c/cam"));
		assert!(!updated.publish.matches("b/cam"));
		drop(token);
		let narrowed = union.changed().await.unwrap().unwrap();
		assert!(narrowed.publish.matches("a/cam"));
		assert!(!narrowed.publish.matches("c/cam"));
		assert!(!supervising.is_finished());
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}

	#[tokio::test(start_paused = true)]
	async fn withdrawn_tokens_keep_their_pending_verification_slots() {
		let (lease, mut admissions) = admission(grant("a/**", 3000)).await;
		let (client, supervising) = sessions(lease).await;
		let mut held = Vec::new();
		for _ in 0..128 {
			let auth = client.auth();
			let adding = tokio::spawn(async move { auth.add("withdrawn").await });
			held.push(admissions.next().await.unwrap());
			adding.abort();
			let _ = adding.await;
		}
		let auth = client.auth();
		tokio::select! {
			biased;
			_ = admissions.next() => panic!("withdrawal released an in-flight verification slot"),
			result = auth.add("overflow") => assert!(matches!(
				result,
				Err(moq_net::Error::Session(SessionError::TooManyRequests))
			)),
		}
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}

	#[test]
	fn renewal_replaces_only_the_jwt_pair() {
		assert_eq!(
			with_jwt(Some("flag&a=%2f&jwt=old&b=x+y&j%77t=dup"), "new token"),
			"flag&a=%2f&jwt=new+token&b=x+y"
		);
		assert_eq!(with_jwt(Some("flag&&jwt=old&"), "t"), "flag&&jwt=t&");
		assert_eq!(with_jwt(Some("a=1"), "t"), "a=1&jwt=t");
		assert_eq!(with_jwt(Some(""), "t"), "jwt=t");
		assert_eq!(with_jwt(None, "t"), "jwt=t");
	}

	#[tokio::test(start_paused = true)]
	async fn public_rule_sessions_refuse_tokens_as_unsupported() {
		let auth = super::super::Config {
			public_publish: vec!["**".parse().unwrap()],
			..Default::default()
		}
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.unwrap();
		let lease = auth
			.admit(auth.request(moq_auth::Transport::Quic, "/room"))
			.await
			.unwrap();
		let (client, supervising) = sessions(lease).await;
		assert!(matches!(
			client.auth().add("token").await,
			Err(moq_net::Error::Unsupported)
		));
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}

	#[tokio::test(start_paused = true)]
	async fn certificate_sessions_refuse_tokens_as_unsupported_before_admission() {
		let (auth, mut admissions) = super::super::Auth::embedded("test");
		let mut request = auth.request(moq_auth::Transport::Quic, "/room");
		request.tls = Some(moq_auth::Peer {
			name: "edge".into(),
			fingerprint: "ab".repeat(32),
			expires: None,
			issuer: "test".into(),
		});
		let (lease, ()) = tokio::join!(auth.admit(request), async {
			admissions
				.next()
				.await
				.unwrap()
				.grant(lease::Consumer::fixed(grant("a/**", 30)));
		});
		let (client, supervising) = sessions(lease.unwrap()).await;
		assert!(matches!(
			client.auth().add("replacement").await,
			Err(moq_net::Error::Unsupported)
		));
		assert!(admissions.next().now_or_never().is_none());
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}
}
