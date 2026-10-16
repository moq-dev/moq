use std::collections::{BTreeMap, VecDeque};

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
	/// Tokens the decider is verifying, at most `Auth::verifications` counting
	/// `settling`. A sent check runs to completion, even once its token is withdrawn,
	/// so the connection's `end` stays its last request.
	pending: FuturesUnordered<BoxFuture<'static, (Option<Verified>, Option<Settling>)>>,
	/// Decisions that outlived the admission timeout: the token was already refused,
	/// but the decider is still working, so its slot stays held until it answers.
	settling: FuturesUnordered<Settling>,
	/// Tokens waiting for a slot, oldest first. The peer's credit bounds them: each
	/// holds its request until answered or withdrawn, and a withdrawn one never
	/// reaches the decider.
	waiting: VecDeque<Waiting>,
	/// How many waiting tokens survived the last prune: pruning again only once the
	/// queue doubles keeps a push amortized O(1).
	survivors: usize,
	next: u64,
}

type Verified = (auth::Request, Result<lease::Consumer, super::Error>);
type Settling = BoxFuture<'static, ()>;

/// A token waiting for a verification slot.
struct Waiting {
	request: auth::Request,
	facts: moq_auth::Request,
	/// When its admission times out, counted from its arrival.
	deadline: tokio::time::Instant,
}

impl Waiting {
	fn withdrawn(&self) -> bool {
		self.request.closed().now_or_never().is_some()
	}
}

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
			settling: FuturesUnordered::new(),
			waiting: VecDeque::new(),
			survivors: 0,
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

	/// The connection credential's grant. moq-net adds every grant issued to an AUTH token
	/// itself, and keeps a request token's grant to that request alone.
	fn authorize(&self) {
		let mut union = auth::Grant::default();
		for credential in self.credentials.values().filter(|credential| credential.initial) {
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
			if let Some(reason) = self.owner.closed().now_or_never() {
				let code = match reason {
					lease::Reason::Expired => SessionError::Expired,
					_ => SessionError::Unauthorized,
				};
				request.reject(code, "connection credential ended");
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
			// The lease may have revalidated to a grant this session can no longer
			// use while another credential keeps the session alive. Validate the
			// exact snapshot that would be issued.
			let mut credential = Credential::new(self.owner.clone());
			if !self.valid(&credential.grant) {
				request.reject(SessionError::Unauthorized, "connection credential no longer valid");
				return;
			}
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
		let Ok(token) = std::str::from_utf8(request.token()) else {
			request.reject(SessionError::Unauthorized, "token is not UTF-8");
			return;
		};
		let mut facts = source.request.clone();
		facts.query = Some(with_jwt(facts.query.as_deref(), token));
		facts.token = None;
		if self.waiting.len() >= 2 * self.survivors.max(1) {
			self.waiting.retain(|waiting| !waiting.withdrawn());
			self.survivors = self.waiting.len();
		}
		self.waiting.push_back(Waiting {
			request,
			facts,
			deadline: tokio::time::Instant::now() + super::ADMIT_TIMEOUT,
		});
		self.dispatch();
	}

	/// Send waiting tokens to the decider while slots are free, dropping any the peer
	/// withdrew or whose admission timed out while it waited.
	fn dispatch(&mut self) {
		let Some(source) = self.source.as_ref() else {
			return;
		};
		while self.pending.len() + self.settling.len() < source.auth.verifications {
			let Some(waiting) = self.waiting.pop_front() else {
				return;
			};
			if waiting.withdrawn() {
				continue;
			}
			let Waiting {
				request,
				facts,
				deadline,
			} = waiting;
			if deadline <= tokio::time::Instant::now() {
				refuse(request, super::timed_out());
				continue;
			}
			let mut answer = match source.auth.submit(facts, Some(self.owner.clone())) {
				Ok(answer) => answer,
				Err(err) => {
					refuse(request, err);
					continue;
				}
			};
			self.pending.push(Box::pin(async move {
				// Withdrawing the stream cannot cancel the decider's work. Keep its slot
				// until the decider answers so churn cannot bypass the bound.
				let (result, settling) = match tokio::time::timeout_at(deadline, &mut answer).await {
					Ok(answered) => (super::answered_with(answered), None),
					Err(_) => {
						let settling: Settling = Box::pin(async move {
							let _ = answer.await;
						});
						(Err(super::timed_out()), Some(settling))
					}
				};
				let verdict = request.closed().now_or_never().is_none().then_some((request, result));
				(verdict, settling)
			}));
		}
	}

	/// Refuse the waiting tokens whose admission timed out before a slot freed.
	fn expire(&mut self) {
		let now = tokio::time::Instant::now();
		while let Some(waiting) = self.waiting.front()
			&& (waiting.deadline <= now || waiting.withdrawn())
		{
			let waiting = self.waiting.pop_front().expect("peeked");
			if !waiting.withdrawn() {
				refuse(waiting.request, super::timed_out());
			}
		}
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
				Verified(Option<Verified>, Option<Settling>),
				Settled,
				Expired,
				Changed(u64, Change),
			}
			let event = {
				// The oldest waiting token's deadline is the earliest, as they all wait the
				// same timeout from their arrival.
				let expiry = self.waiting.front().map(|waiting| waiting.deadline);
				let expired = async {
					match expiry {
						Some(at) => tokio::time::sleep_until(at).await,
						None => std::future::pending().await,
					}
				};
				let mut changes: FuturesUnordered<_> = self
					.credentials
					.iter_mut()
					.map(|(id, credential)| async move { (*id, credential.changed().await) })
					.collect();
				tokio::select! {
					biased;
					Some((id, change)) = changes.next(), if !changes.is_empty() => Event::Changed(id, change),
					Some((verdict, settling)) = self.pending.next(), if !self.pending.is_empty() => Event::Verified(verdict, settling),
					Some(()) = self.settling.next(), if !self.settling.is_empty() => Event::Settled,
					() = expired => Event::Expired,
					request = self.requests.next(), if self.requests_open => Event::Request(request),
				}
			};
			match event {
				Event::Request(Some(request)) => self.request(request),
				Event::Request(None) => self.requests_open = false,
				Event::Settled => self.dispatch(),
				Event::Expired => self.expire(),
				Event::Verified(verdict, settling) => {
					self.settling.extend(settling);
					self.dispatch();
					let Some((request, result)) = verdict else {
						continue;
					};
					// Validate the exact snapshot that is issued, not a later read.
					let credential = result.map(Credential::new);
					match credential {
						Ok(mut credential) if self.valid(&credential.grant) => {
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
						Err(err) => refuse(request, err),
					}
				}
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
						let code = match &reason {
							lease::Reason::Expired => SessionError::Expired,
							_ => SessionError::Unauthorized,
						};
						issued.revoke(code, reason.to_string());
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

/// Refuse a token the decider did not admit. An auth server that could not decide
/// is no verdict on the token, so the presenter may present it again.
fn refuse(request: auth::Request, err: super::Error) {
	match err {
		super::Error::Unavailable(reason) => {
			tracing::debug!(%reason, "in-band token not verified; auth server unavailable");
			request.reject(SessionError::Internal, "auth server unavailable");
		}
		_ => request.reject(SessionError::Unauthorized, "token refused"),
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
				.connect(
					now,
					moq_tokio::transport::Session::new(client, moq_net::transport::Binding::InBand),
				)
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
				.accept_request(
					now,
					moq_tokio::transport::Session::new(server, moq_net::transport::Binding::InBand),
				)
				.await
				.unwrap();
			let mut lease = lease.preparing(request.auth(), request.role()).unwrap();
			let (session, driver) = request.ok().await.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			(tokio::spawn(async move {
				let reason = lease.ended().await;
				session.abort(super::super::error(&reason));
				lease.close(reason.clone(), Default::default());
				reason
			}),)
		};
		let (client, (server,)) = tokio::join!(client, server);
		(client, server)
	}

	async fn admission(initial: Grant) -> (super::super::Lease, super::super::Admissions) {
		admission_with(initial, super::super::VERIFICATIONS).await
	}

	/// A session admitted on `initial` whose in-band tokens verify `verifications` at a time.
	async fn admission_with(initial: Grant, verifications: usize) -> (super::super::Lease, super::super::Admissions) {
		let (auth, mut admissions) = super::super::Auth::embedded("test");
		let auth = auth.with_verifications(verifications);
		let request = auth.request(moq_auth::Transport::Quic, "/room");
		let (lease, ()) = tokio::join!(auth.admit(request), async {
			let admission = admissions.next().await.unwrap();
			assert!(!admission.attached(), "a new session arrived as an attachment");
			admission.grant(lease::Consumer::fixed(initial));
		});
		(lease.unwrap(), admissions)
	}

	#[tokio::test(start_paused = true)]
	async fn an_in_band_token_arrives_as_an_attachment() {
		let (lease, mut admissions) = admission(grant("a/**", 3000)).await;
		let (client, supervising) = sessions(lease).await;
		let auth = client.auth();
		let (token, ()) = tokio::join!(auth.add("replacement"), async {
			let admission = admissions.next().await.unwrap();
			assert!(admission.attached(), "an in-band token arrived as a new session");
			admission.grant(lease::Consumer::fixed(grant("a/**", 3000)));
		});
		token.unwrap();
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}

	#[tokio::test(start_paused = true)]
	async fn an_outage_refuses_transiently_and_never_extends_the_expiry() {
		let (lease, mut admissions) = admission(grant("a/**", 30)).await;
		let (client, supervising) = sessions(lease).await;
		let start = tokio::time::Instant::now();
		for _ in 0..3 {
			let auth = client.auth();
			let (result, ()) = tokio::join!(auth.add("replacement"), async {
				admissions
					.next()
					.await
					.unwrap()
					.refuse(super::super::Error::Unavailable("auth server down".into()));
			});
			assert!(matches!(result, Err(moq_net::Error::Session(SessionError::Internal))));
			tokio::time::advance(Duration::from_secs(5)).await;
		}
		assert!(matches!(
			client.closed().await,
			moq_net::Error::Session(SessionError::Expired)
		));
		assert_eq!(supervising.await.unwrap(), lease::Reason::Expired);
		let lived = start.elapsed();
		assert!(
			lived >= Duration::from_secs(29) && lived <= Duration::from_secs(31),
			"the session outlived its only accepted grant: {lived:?}"
		);
		client.abort(moq_net::Error::Cancel);
	}

	/// Present `token` in the background, as a peer whose credit allows it.
	fn present(client: &moq_net::Session, token: &'static str) -> tokio::task::JoinHandle<Result<(), moq_net::Error>> {
		let auth = client.auth();
		tokio::spawn(async move { auth.add(token).await.map(drop) })
	}

	/// A token past the verification limit waits for a slot rather than being refused,
	/// and reaches the decider once one frees.
	#[tokio::test(start_paused = true)]
	async fn a_token_over_the_limit_waits_for_a_slot() {
		let (lease, mut admissions) = admission_with(grant("a/**", 3000), 2).await;
		let (client, supervising) = sessions(lease).await;
		let first = present(&client, "first");
		let mut held = vec![admissions.next().await.unwrap()];
		let _second = present(&client, "second");
		held.push(admissions.next().await.unwrap());

		let third = present(&client, "third");
		assert!(
			tokio::time::timeout(Duration::from_secs(5), admissions.next())
				.await
				.is_err(),
			"a token past the limit reached the decider"
		);
		assert!(
			!third.is_finished(),
			"a token past the limit was answered without a slot"
		);

		held.remove(0).grant(lease::Consumer::fixed(grant("a/**", 3000)));
		first.await.unwrap().unwrap();
		let admission = admissions.next().await.unwrap();
		assert_eq!(admission.request.query.as_deref(), Some("jwt=third"));
		admission.grant(lease::Consumer::fixed(grant("a/**", 3000)));
		third.await.unwrap().unwrap();
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}

	/// A waiting token whose admission times out is refused without reaching the
	/// decider, while the check it waited behind keeps its slot past the timeout.
	#[tokio::test(start_paused = true)]
	async fn a_waiting_token_times_out_without_reaching_the_decider() {
		let (lease, mut admissions) = admission_with(grant("a/**", 3000), 1).await;
		let (client, supervising) = sessions(lease).await;
		let slow = present(&client, "slow");
		let held = admissions.next().await.unwrap();
		let waiting = present(&client, "waiting");
		assert!(matches!(
			waiting.await.unwrap(),
			Err(moq_net::Error::Session(SessionError::Internal))
		));
		assert!(matches!(
			slow.await.unwrap(),
			Err(moq_net::Error::Session(SessionError::Internal))
		));
		assert!(
			admissions.next().now_or_never().is_none(),
			"a timed-out check released its slot"
		);

		// The decider giving up frees the slot for the next token, not the expired one.
		drop(held);
		let _next = present(&client, "next");
		let admission = admissions.next().await.unwrap();
		assert_eq!(admission.request.query.as_deref(), Some("jwt=next"));
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
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
	async fn an_empty_token_is_refused_once_the_connection_credential_changes_root() {
		let (auth, mut admissions) = super::super::Auth::embedded("test");
		let request = auth.request(moq_auth::Transport::Quic, "/room");
		let (owner, consumer) = lease::Producer::new(grant("a/**", 3000));
		let (lease, ()) = tokio::join!(auth.admit(request), async {
			admissions.next().await.unwrap().grant(consumer);
		});
		let (client, supervising) = sessions(lease.unwrap()).await;
		let auth = client.auth();
		let (token, ()) = tokio::join!(auth.add("replacement"), async {
			admissions
				.next()
				.await
				.unwrap()
				.grant(lease::Consumer::fixed(grant("a/**", 3000)));
		});
		let _token = token.unwrap();
		let mut moved = grant("a/**", 3000);
		moved.root = Some("other".into());
		owner.update(moved);
		tokio::task::yield_now().await;
		let result = auth.add("").await;
		assert!(
			matches!(result, Err(moq_net::Error::Session(SessionError::Unauthorized))),
			"an empty token reissued a credential the relay removed: {:?}",
			result.map(|token| token.grant().peek())
		);
		assert!(!supervising.is_finished());
		client.abort(moq_net::Error::Cancel);
		supervising.abort();
		let _ = supervising.await;
	}

	/// A token the peer withdraws while it waits for a slot never reaches the decider,
	/// while a sent check keeps its slot after its token is withdrawn.
	#[tokio::test(start_paused = true)]
	async fn a_withdrawn_waiting_token_never_reaches_the_decider() {
		let (lease, mut admissions) = admission_with(grant("a/**", 3000), 1).await;
		let (client, supervising) = sessions(lease).await;
		let sent = present(&client, "sent");
		let held = admissions.next().await.unwrap();
		sent.abort();
		let _ = sent.await;

		let withdrawn = present(&client, "withdrawn");
		tokio::time::sleep(Duration::from_secs(1)).await;
		withdrawn.abort();
		let _ = withdrawn.await;
		let next = present(&client, "next");
		assert!(
			tokio::time::timeout(Duration::from_secs(1), admissions.next())
				.await
				.is_err(),
			"withdrawing a sent check released its slot"
		);

		held.grant(lease::Consumer::fixed(grant("a/**", 3000)));
		let admission = admissions.next().await.unwrap();
		assert_eq!(admission.request.query.as_deref(), Some("jwt=next"));
		admission.grant(lease::Consumer::fixed(grant("a/**", 3000)));
		next.await.unwrap().unwrap();
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
