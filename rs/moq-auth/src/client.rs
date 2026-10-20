use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use moq_clock::Instant;
use url::Url;

use crate::lease::{self, Due, Reason};
use crate::{Bytes, Error, Event, Grant, Request};

/// Every request is bounded so a hung server refuses rather than parks the session.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The HTTP side of the contract: one JSON POST per event to an auth server.
///
/// `connect` admits a session and hands back the [`lease::Consumer`] it holds; a task
/// behind it re-POSTs `revalidate` on the grant's cadence, applies each reply, revokes
/// when the server refuses, answers an invalid grant, or the grant expires, and POSTs
/// `end` when the session closes.
#[derive(Clone)]
pub struct Client {
	identity: Arc<()>,
	http: reqwest::Client,
	url: Url,
}

impl Client {
	/// A client for the server at `url`.
	///
	/// `http://` is accepted for a loopback host only; `https://` presents `tls`, the
	/// caller's client identity and roots; `unix://` speaks HTTP over the socket at
	/// the URL's path. Anything else is refused here rather than at the first session.
	pub fn new(url: Url, tls: Option<rustls::ClientConfig>) -> crate::Result<Self> {
		let builder = reqwest::Client::builder().timeout(TIMEOUT);

		let (builder, url) = match url.scheme() {
			"http" => {
				let loopback = match url.host() {
					Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
					Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
					Some(url::Host::Domain(host)) => host == "localhost",
					None => false,
				};
				if !loopback {
					return Err(Error::InsecureUrl(redact(&url)));
				}
				(builder, url)
			}
			"https" => match tls {
				Some(tls) => (builder.use_preconfigured_tls(tls), url),
				None => (builder, url),
			},
			#[cfg(unix)]
			"unix" => {
				let path = url.to_file_path().map_err(|()| Error::InvalidUrl(redact(&url)))?;
				// The socket is the transport; the request target is the server's root.
				let target = Url::parse("http://localhost/").expect("a constant URL parses");
				(builder.unix_socket(path), target)
			}
			_ => return Err(Error::InvalidUrl(redact(&url))),
		};

		Ok(Self {
			identity: Arc::new(()),
			http: builder.build()?,
			url,
		})
	}

	/// Admit a session: POST `connect`, validate the reply, and return the lease the
	/// session holds. The session reports its end and totals through [`lease::Consumer::close`]
	/// or drop; credential expiry alone never reports that the connection ended.
	pub async fn connect(&self, request: Request) -> crate::Result<lease::Consumer> {
		let context = Context::new(request.clone(), self.identity.clone());
		admit(self.clone(), request, context).await
	}

	/// Verify another credential for this connection without owning its end event.
	pub async fn attach(&self, connection: &lease::Consumer, request: Request) -> crate::Result<lease::Consumer> {
		let context = connection.context().ok_or(Error::Refused(Default::default()))?;
		let context = context.attach(&request, &self.identity)?;
		admit(self.clone(), request, context).await
	}
}

/// A credential's ownership of the connection it belongs to.
#[derive(Clone)]
pub(crate) struct Context {
	connection: Arc<Connection>,
	owner: bool,
}

struct Connection {
	request: Request,
	issuer: Arc<()>,
	lifecycle: kio::Shared<Lifecycle>,
}

#[derive(Default)]
struct Lifecycle {
	ended: Option<(Reason, Bytes, Instant)>,
	pending: usize,
}

/// End must follow every request an attached credential has in flight for this
/// connection id: its Connect, and each Revalidate until its driver stops.
struct Admission(Arc<Connection>);

impl Drop for Admission {
	fn drop(&mut self) {
		self.0.lifecycle.lock().pending -= 1;
	}
}

impl std::fmt::Debug for Context {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Context")
			.field("owner", &self.owner)
			.finish_non_exhaustive()
	}
}

impl Context {
	fn new(mut request: Request, issuer: Arc<()>) -> Self {
		request.event = Event::Connect;
		Self {
			connection: Arc::new(Connection {
				request,
				issuer,
				lifecycle: Default::default(),
			}),
			owner: true,
		}
	}

	fn attach(&self, request: &Request, issuer: &Arc<()>) -> crate::Result<Self> {
		if !self.owner || !Arc::ptr_eq(issuer, &self.connection.issuer) || self.connection.request.tls.is_some() {
			return Err(Error::Refused(Default::default()));
		}
		let mut expected = self.connection.request.clone();
		expected.query = request.query.clone();
		expected.token = request.token.clone();
		if &expected != request {
			return Err(Error::Refused(Default::default()));
		}
		if self.connection.lifecycle.read().ended.is_some() {
			return Err(Error::Refused(Default::default()));
		}
		Ok(Self {
			connection: self.connection.clone(),
			owner: false,
		})
	}

	fn admitting(&self) -> crate::Result<Option<Admission>> {
		if self.owner {
			return Ok(None);
		}
		let mut lifecycle = self.connection.lifecycle.lock();
		if lifecycle.ended.is_some() {
			return Err(Error::Refused(Default::default()));
		}
		lifecycle.pending += 1;
		Ok(Some(Admission(self.connection.clone())))
	}

	pub(crate) fn close(&self, reason: Reason, bytes: Bytes) {
		if self.owner {
			self.connection
				.lifecycle
				.lock()
				.ended
				.get_or_insert((reason, bytes, Instant::now()));
		}
	}

	/// Resolve with the connection's end reason, for an attached credential only.
	async fn connection_ended(&self) -> Reason {
		if self.owner {
			return std::future::pending().await;
		}
		kio::wait(|waiter| {
			let lifecycle = std::task::ready!(self.connection.lifecycle.poll(waiter, |lifecycle| {
				if lifecycle.ended.is_some() {
					std::task::Poll::Ready(())
				} else {
					std::task::Poll::Pending
				}
			}));
			std::task::Poll::Ready(lifecycle.ended.as_ref().expect("waited for connection end").0.clone())
		})
		.await
	}

	async fn end(&self) -> Option<(Reason, Bytes, Instant)> {
		if !self.owner {
			return None;
		}
		Some(
			kio::wait(|waiter| {
				let lifecycle = std::task::ready!(self.connection.lifecycle.poll(waiter, |ended| {
					if ended.ended.is_some() && ended.pending == 0 {
						std::task::Poll::Ready(())
					} else {
						std::task::Poll::Pending
					}
				}));
				std::task::Poll::Ready(lifecycle.ended.clone().expect("waited for connection end"))
			})
			.await,
		)
	}
}

/// Where a lease's requests go: the auth server over HTTP, or an in-process
/// answer in tests, so the lease's timers run on Tokio's paused clock with no
/// socket for them to outrun.
trait Post: Clone + Send + Sync + 'static {
	/// Send a `connect` or `revalidate`: the grant as answered, not yet validated,
	/// a refusal, or an outage.
	fn post(&self, request: &Request) -> impl Future<Output = crate::Result<Grant>> + Send;

	/// Send the `end`.
	fn post_end(&self, request: &Request) -> impl Future<Output = crate::Result<()>> + Send;
}

impl Post for Client {
	/// A 2xx carries the grant, a 401 or 403 refuses, and everything else is an
	/// outage the caller decides about. A JSON refusal carries its reason and verified attribution;
	/// legacy bodies refuse without attribution.
	async fn post(&self, request: &Request) -> crate::Result<Grant> {
		let response = self.http.post(self.url.clone()).json(request).send().await?;
		let status = response.status();
		if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
			return Err(Error::Refused(response.json().await.unwrap_or_default()));
		}
		if !status.is_success() {
			return Err(Error::Unavailable(format!("auth server answered {status}")));
		}
		Ok(response.json().await?)
	}

	async fn post_end(&self, request: &Request) -> crate::Result<()> {
		self.http
			.post(self.url.clone())
			.json(request)
			.send()
			.await?
			.error_for_status()?;
		Ok(())
	}
}

/// Admit a session through `server` and spawn the driver behind its lease.
async fn admit<S: Post>(server: S, mut request: Request, context: Context) -> crate::Result<lease::Consumer> {
	request.event = Event::Connect;
	let admission = context.admitting()?;

	let grant = ask(&server, &request).await?;
	if context.connection.lifecycle.read().ended.is_some() {
		return Err(Error::Refused(Default::default()));
	}
	let (producer, mut consumer) = lease::Producer::new(grant.clone());
	consumer.set_context(context.clone());

	let driver = Driver {
		context,
		_admission: admission,
		server,
		request,
		producer: Some(producer),
		started: Instant::now(),
	};
	tokio::spawn(driver.run());

	Ok(consumer)
}

/// One `connect` or `revalidate`: a valid grant admits, and one that fails
/// validation is that error.
async fn ask<S: Post>(server: &S, request: &Request) -> crate::Result<Grant> {
	let grant = server.post(request).await?;
	grant.validate().map_err(|err| match err {
		// An empty grant is a refusal, not a malformed answer.
		Error::UselessGrant => Error::Refused(Default::default()),
		other => other,
	})?;
	Ok(grant)
}

/// `url` without its userinfo, query, or fragment, any of which may carry a credential.
fn redact(url: &Url) -> String {
	let mut url = url.clone();
	let _ = url.set_username("");
	let _ = url.set_password(None);
	url.set_query(None);
	url.set_fragment(None);
	url.to_string()
}

/// The task behind a lease: re-checks on cadence and reports the end.
struct Driver<S> {
	context: Context,
	/// Held by an attached credential's driver, so the connection's End waits for it.
	_admission: Option<Admission>,
	server: S,
	request: Request,
	producer: Option<lease::Producer>,
	started: Instant,
}

impl<S: Post> Driver<S> {
	async fn run(mut self) {
		self.drive().await;
		let Some((reason, bytes, ended)) = self.context.end().await else {
			return;
		};

		let mut request = self.request.clone();
		request.event = Event::End {
			reason,
			duration: ended.saturating_duration_since(self.started),
			bytes,
		};
		// The session is already gone; nothing to do with a failure but say so.
		if let Err(err) = self.server.post_end(&request).await {
			tracing::warn!(id = %request.id, %err, "failed to report the session end");
		}
	}

	/// Re-check until the lease ends, returning why it did and the totals the session reported.
	async fn drive(&mut self) -> (Reason, Bytes) {
		let producer = self
			.producer
			.take()
			.expect("the driver owns the producer until it ends");
		// Keep an in-flight request alive while expiry or a push is observed.
		let mut inflight: Option<Pin<Box<dyn Future<Output = crate::Result<Grant>> + Send>>> = None;
		let mut pending = false;

		let ended = loop {
			let reply = async {
				match inflight.as_mut() {
					Some(request) => request.await,
					None => std::future::pending().await,
				}
			};
			tokio::select! {
				biased;
				ended = producer.closed() => break ended,
				// An attached credential ends with its connection.
				reason = self.context.connection_ended() => break producer.finish(reason, Bytes::default()),
				due = producer.due() => match due {
					Due::Expired => break producer.finish(Reason::Expired, Bytes::default()),
					Due::Revalidate if inflight.is_some() => pending = true,
					Due::Revalidate => inflight = Some(self.post_revalidate()),
				},
				result = reply => {
					inflight = None;
					match result {
						Ok(fresh) => producer.update(fresh),
						Err(Error::Refused(refusal)) => {
							let reason = match refusal.reason {
								crate::refusal::Reason::Expired => Reason::Expired,
								_ => Reason::Refused,
							};
							break producer.finish(reason, Bytes::default());
						},
						Err(Error::GrantExpired | Error::UnboundedRevalidate | Error::ZeroRevalidate) => {
							break producer.finish(Reason::Invalid, Bytes::default());
						},
						Err(err) => {
							let delay = producer.failed();
							tracing::warn!(id = %self.request.id, %err, ?delay, "auth revalidation failed; retrying");
						},
					}
					if pending {
						pending = false;
						producer.immediate();
						inflight = Some(self.post_revalidate());
					}
				},
			}
		};
		// Land an in-flight Revalidate before releasing the connection's End, so
		// End is the last request for this id however the lease ended.
		if let Some(request) = inflight {
			let _ = request.await;
		}
		ended
	}

	fn post_revalidate(&self) -> Pin<Box<dyn Future<Output = crate::Result<Grant>> + Send>> {
		let server = self.server.clone();
		let mut request = self.request.clone();
		request.event = Event::Revalidate;
		Box::pin(async move { ask(&server, &request).await })
	}
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)]
mod tests {
	use super::*;
	use moq_pattern::Patterns;
	use std::task::Poll;
	use std::time::SystemTime;
	use wiremock::matchers::{method, path};
	use wiremock::{Mock, MockServer, Request as Received, ResponseTemplate};

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn request() -> Request {
		let mut request = Request::new("relay-1", crate::Transport::Quic, "/demo/room");
		request.id = "0123".into();
		request
	}

	/// Records every request body the server saw, in order.
	#[derive(Clone, Default)]
	struct Log(kio::Shared<Vec<Request>>);

	impl Log {
		fn events(&self) -> Vec<Event> {
			self.0.read().iter().map(|r| r.event.clone()).collect()
		}

		fn revalidates(&self) -> usize {
			self.events()
				.iter()
				.filter(|event| **event == Event::Revalidate)
				.count()
		}

		/// Wait until the requests seen so far satisfy `done`.
		async fn until(&self, mut done: impl FnMut(&[Request]) -> bool + Unpin) {
			self.0
				.wait(|log| if done(log) { Poll::Ready(()) } else { Poll::Pending })
				.await;
		}

		/// Wait for the background `end` POST and return it.
		async fn end(&self) -> Request {
			let is_end = |r: &Request| matches!(r.event, Event::End { .. });
			self.until(|log| log.iter().any(is_end)).await;
			self.0.read().iter().find(|r| is_end(r)).cloned().unwrap()
		}
	}

	impl wiremock::Match for Log {
		fn matches(&self, received: &Received) -> bool {
			self.0.lock().push(received.body_json().unwrap());
			true
		}
	}

	async fn server(log: Log, respond: impl Fn(&Request) -> ResponseTemplate + Send + Sync + 'static) -> MockServer {
		let server = MockServer::start().await;
		Mock::given(method("POST"))
			.and(path("/"))
			.and(log)
			.respond_with(move |received: &Received| respond(&received.body_json().unwrap()))
			.mount(&server)
			.await;
		server
	}

	/// An in-process auth server for tests on Tokio's paused clock: it logs each
	/// request and answers with `respond`, never when that returns `None`. With no
	/// socket, the clock only advances once every answer has landed.
	#[derive(Clone)]
	struct Script {
		identity: Arc<()>,
		log: Log,
		#[allow(clippy::type_complexity)]
		respond: std::sync::Arc<dyn Fn(&Request) -> Option<crate::Result<Grant>> + Send + Sync>,
	}

	impl Script {
		fn new(log: Log, respond: impl Fn(&Request) -> Option<crate::Result<Grant>> + Send + Sync + 'static) -> Self {
			Self {
				identity: Arc::new(()),
				log,
				respond: std::sync::Arc::new(respond),
			}
		}

		async fn connect(&self) -> crate::Result<lease::Consumer> {
			admit(self.clone(), request(), Context::new(request(), self.identity.clone())).await
		}
		async fn attach(&self, owner: &lease::Consumer, request: Request) -> crate::Result<lease::Consumer> {
			let context = owner
				.context()
				.ok_or(Error::Refused(Default::default()))?
				.attach(&request, &self.identity)?;
			admit(self.clone(), request, context).await
		}
	}

	impl Post for Script {
		async fn post(&self, request: &Request) -> crate::Result<Grant> {
			self.log.0.lock().push(request.clone());
			let answer = (self.respond)(request);
			match answer {
				Some(answer) => answer,
				None => std::future::pending().await,
			}
		}

		async fn post_end(&self, request: &Request) -> crate::Result<()> {
			self.log.0.lock().push(request.clone());
			Ok(())
		}
	}

	fn client(server: &MockServer) -> Client {
		Client::new(server.uri().parse().unwrap(), None).unwrap()
	}

	/// The wire carries whole seconds, so a cadence under one second would serialize
	/// as zero and be refused; these tests run on a one-second cadence.
	fn grant(expires_in: Option<Duration>, revalidate: Option<Duration>) -> Grant {
		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		grant.expires = expires_in.map(|d| SystemTime::now() + d);
		grant.revalidate = revalidate;
		grant
	}

	#[tokio::test]
	async fn connect_admits_and_end_follows_the_close() {
		let log = Log::default();
		let server = server(log.clone(), |_| {
			ResponseTemplate::new(200).set_body_json(grant(None, None))
		})
		.await;

		let consumer = client(&server).connect(request()).await.unwrap();
		assert_eq!(consumer.grant().publish, patterns(&["**"]).into());
		assert_eq!(log.events(), [Event::Connect]);

		consumer.close("disconnected", Bytes { sent: 7, received: 11 });

		let end = log.end().await;
		assert_eq!(end.id, "0123");
		match end.event {
			Event::End { reason, bytes, .. } => {
				assert_eq!(reason, Reason::Session("disconnected".into()));
				assert_eq!(bytes, Bytes { sent: 7, received: 11 });
			}
			other => panic!("expected an end, got {other:?}"),
		}
	}

	#[tokio::test(start_paused = true)]
	async fn session_duration_uses_the_lease_clock() {
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let consumer = script.connect().await.unwrap();
		tokio::time::advance(Duration::from_secs(60)).await;
		consumer.close("done", Bytes::default());
		match log.end().await.event {
			Event::End { duration, .. } => assert_eq!(duration, Duration::from_secs(60)),
			other => panic!("expected connection end, got {other:?}"),
		}
	}

	#[tokio::test]
	async fn an_attached_lease_never_reports_the_connection_end() {
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let owner = script.connect().await.unwrap();
		let retained = owner.clone();
		let mut request = request();
		request.query = Some("jwt=replacement".into());
		let attached = script.attach(&owner, request).await.unwrap();
		drop(attached);
		tokio::task::yield_now().await;
		assert_eq!(log.events(), [Event::Connect, Event::Connect]);
		owner.close("done", Bytes { sent: 7, received: 11 });
		match log.end().await.event {
			Event::End { reason, bytes, .. } => {
				assert_eq!(reason, Reason::Session("done".into()));
				assert_eq!(bytes, Bytes { sent: 7, received: 11 });
			}
			other => panic!("expected connection end, got {other:?}"),
		}
		drop(retained);
	}

	#[tokio::test]
	async fn an_attached_lease_ends_with_its_connection() {
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let owner = script.connect().await.unwrap();
		let mut request = request();
		request.query = Some("jwt=replacement".into());
		let attached = script.attach(&owner, request).await.unwrap();
		owner.close(Reason::Shutdown, Bytes::default());
		assert_eq!(attached.closed().await, Reason::Shutdown);
		log.end().await;
		attached.revalidate();
		tokio::task::yield_now().await;
		assert!(
			matches!(
				log.events().as_slice(),
				[Event::Connect, Event::Connect, Event::End { .. }]
			),
			"an attached lease outlived its connection's End"
		);
	}

	#[tokio::test]
	async fn a_withdrawn_attachment_lands_its_revalidate_before_end() {
		#[derive(Clone)]
		struct HeldRevalidate {
			script: Script,
			arrived: Arc<tokio::sync::Notify>,
			release: Arc<tokio::sync::Notify>,
		}
		impl Post for HeldRevalidate {
			async fn post(&self, request: &Request) -> crate::Result<Grant> {
				if request.event == Event::Revalidate {
					self.arrived.notify_one();
					self.release.notified().await;
				}
				self.script.post(request).await
			}
			async fn post_end(&self, request: &Request) -> crate::Result<()> {
				self.script.post_end(request).await
			}
		}
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let owner = script.connect().await.unwrap();
		let server = HeldRevalidate {
			script: script.clone(),
			arrived: Arc::default(),
			release: Arc::default(),
		};
		let mut request = request();
		request.query = Some("jwt=replacement".into());
		let context = owner.context().unwrap().attach(&request, &script.identity).unwrap();
		let attached = admit(server.clone(), request, context).await.unwrap();
		attached.revalidate();
		server.arrived.notified().await;
		drop(attached);
		tokio::task::yield_now().await;
		owner.close(Reason::Shutdown, Bytes::default());
		tokio::task::yield_now().await;
		assert_eq!(
			log.events(),
			[Event::Connect, Event::Connect],
			"end overtook a withdrawn attachment's in-flight revalidate"
		);
		server.release.notify_one();
		log.end().await;
		assert!(matches!(
			log.events().as_slice(),
			[Event::Connect, Event::Connect, Event::Revalidate, Event::End { .. }]
		));
	}

	#[tokio::test]
	async fn initial_credential_expiry_does_not_end_a_renewed_connection() {
		tokio::time::pause();
		let log = Log::default();
		let script = Script::new(log.clone(), |request| {
			let seconds = if request.query.as_deref() == Some("jwt=replacement") {
				5
			} else {
				1
			};
			Some(Ok(grant(Some(Duration::from_secs(seconds)), None)))
		});
		let owner = script.connect().await.unwrap();
		let mut request = request();
		request.query = Some("jwt=replacement".into());
		let attached = script.attach(&owner, request).await.unwrap();
		assert_eq!(owner.closed().await, Reason::Expired);
		assert_eq!(log.events(), [Event::Connect, Event::Connect]);
		assert!(matches!(attached.poll_closed(&kio::Waiter::noop()), Poll::Pending));
		owner.close(Reason::Shutdown, Bytes::default());
		assert!(matches!(
			log.end().await.event,
			Event::End {
				reason: Reason::Shutdown,
				..
			}
		));
		drop(attached);
	}

	#[tokio::test]
	async fn connection_end_waits_for_an_attachment_already_being_verified() {
		#[derive(Clone)]
		struct HeldPost {
			script: Script,
			arrived: Arc<tokio::sync::Notify>,
			release: Arc<tokio::sync::Notify>,
		}
		impl Post for HeldPost {
			async fn post(&self, request: &Request) -> crate::Result<Grant> {
				self.arrived.notify_one();
				self.release.notified().await;
				self.script.post(request).await
			}
			async fn post_end(&self, request: &Request) -> crate::Result<()> {
				self.script.post_end(request).await
			}
		}
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let owner = script.connect().await.unwrap();
		let server = HeldPost {
			script: script.clone(),
			arrived: Arc::default(),
			release: Arc::default(),
		};
		let context = owner.context().unwrap().attach(&request(), &script.identity).unwrap();
		let pending = tokio::spawn(admit(server.clone(), request(), context));
		server.arrived.notified().await;
		owner.close(Reason::Shutdown, Bytes::default());
		tokio::task::yield_now().await;
		assert_eq!(
			log.events(),
			[Event::Connect],
			"end overtook an in-flight connect for the same id"
		);
		server.release.notify_one();
		assert!(matches!(pending.await.unwrap(), Err(Error::Refused(_))));
		assert!(matches!(
			log.end().await.event,
			Event::End {
				reason: Reason::Shutdown,
				..
			}
		));
		assert!(matches!(
			log.events().as_slice(),
			[Event::Connect, Event::Connect, Event::End { .. }]
		));
	}

	#[tokio::test]
	async fn an_attachment_cannot_change_connection_identity() {
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let owner = script.connect().await.unwrap();
		for change in [0, 1, 2, 3] {
			let mut request = request();
			match change {
				0 => request.id = "another".into(),
				1 => request.path = "/another".into(),
				2 => request.role = Some(crate::Role::Publisher),
				_ => request.node = "another".into(),
			}
			assert!(matches!(script.attach(&owner, request).await, Err(Error::Refused(_))));
		}
		let other = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		assert!(matches!(other.attach(&owner, request()).await, Err(Error::Refused(_))));
		assert_eq!(log.events(), [Event::Connect]);
		owner.clone().close("done", Bytes::default());
		assert!(matches!(script.attach(&owner, request()).await, Err(Error::Refused(_))));
	}

	#[tokio::test]
	async fn a_bare_drop_ends_as_dropped() {
		let log = Log::default();
		let server = server(log.clone(), |_| {
			ResponseTemplate::new(200).set_body_json(grant(None, None))
		})
		.await;

		let consumer = client(&server).connect(request()).await.unwrap();
		drop(consumer);

		match log.end().await.event {
			Event::End {
				reason: Reason::Dropped,
				bytes,
				..
			} => assert_eq!(bytes, Bytes::default()),
			other => panic!("expected a dropped end, got {other:?}"),
		}
	}

	#[tokio::test]
	async fn refusals_and_outages_refuse_at_connect() {
		for status in [401, 403, 400, 404, 408, 429, 500, 503] {
			let server = server(Log::default(), move |_| ResponseTemplate::new(status)).await;
			let err = client(&server).connect(request()).await.unwrap_err();
			match status {
				401 | 403 => assert!(matches!(err, Error::Refused(_)), "{status}: {err}"),
				_ => assert!(matches!(err, Error::Unavailable(_)), "{status}: {err}"),
			}
		}

		let garbage = server(Log::default(), |_| ResponseTemplate::new(200).set_body_string("nope")).await;
		let err = client(&garbage).connect(request()).await.unwrap_err();
		assert!(matches!(err, Error::Unavailable(_)), "{err}");

		let nothing = server(Log::default(), |_| {
			ResponseTemplate::new(200).set_body_json(Grant::default())
		})
		.await;
		let err = client(&nothing).connect(request()).await.unwrap_err();
		assert!(matches!(err, Error::Refused(_)), "{err}");
	}

	#[tokio::test]
	async fn typed_refusal_preserves_reason_root_and_tier() {
		use crate::refusal::Reason;
		for reason in [Reason::Refused, Reason::Forbidden, Reason::Expired] {
			for root in [None, Some("tenant")] {
				for tier in [None, Some("edge")] {
					let expected = crate::Refusal {
						reason,
						root: root.map(str::to_owned),
						tier: tier.map(str::to_owned),
					};
					let body = expected.clone();
					let server = server(Log::default(), move |_| ResponseTemplate::new(403).set_body_json(&body)).await;
					let Error::Refused(actual) = client(&server).connect(request()).await.unwrap_err() else {
						panic!("expected typed refusal")
					};
					assert_eq!(actual, expected);
				}
			}
		}
	}

	#[tokio::test]
	async fn legacy_and_future_refusals_are_bounded() {
		for status in [401, 403] {
			let server = server(Log::default(), move |_| {
				ResponseTemplate::new(status).set_body_string("legacy refusal")
			})
			.await;
			let Error::Refused(actual) = client(&server).connect(request()).await.unwrap_err() else {
				panic!("expected refusal")
			};
			assert_eq!(actual, crate::Refusal::default());
		}
		let server = server(Log::default(), |_| {
			ResponseTemplate::new(403)
				.set_body_json(serde_json::json!({"reason":"future","root":"tenant","tier":"edge"}))
		})
		.await;
		let Error::Refused(actual) = client(&server).connect(request()).await.unwrap_err() else {
			panic!("expected refusal")
		};
		assert_eq!(actual.reason, crate::refusal::Reason::Refused);
		assert_eq!(actual.root.as_deref(), Some("tenant"));
		assert_eq!(actual.tier.as_deref(), Some("edge"));
	}

	#[tokio::test]
	async fn expired_refusal_ends_a_rechecked_lease_as_expired() {
		tokio::time::pause();
		let server = Script::new(Log::default(), |request| {
			Some(match request.event {
				Event::Connect => Ok(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)))),
				_ => Err(crate::Refusal {
					reason: crate::refusal::Reason::Expired,
					..Default::default()
				}
				.into()),
			})
		});
		let lease = server.connect().await.unwrap();
		assert_eq!(lease.closed().await, Reason::Expired);
	}

	#[tokio::test]
	async fn http_error_redacts_url() {
		// A freed port refuses the connection, so reqwest fails with the dialed URL attached.
		let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
		let address = listener.local_addr().expect("local address");
		drop(listener);

		let url = format!("http://user:pass@{address}/?jwt=secret").parse().unwrap();
		let err = Client::new(url, None).unwrap().connect(request()).await.unwrap_err();

		assert!(matches!(err, Error::Unavailable(_)), "unexpected error: {err}");
		let printed = format!("{err} {err:?}");
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!printed.contains(secret), "error leaked {secret}: {printed}");
		}
	}

	#[tokio::test]
	async fn revalidate_runs_on_cadence_and_applies_the_reply() {
		let log = Log::default();
		let server = server(log.clone(), |request| {
			let mut grant = grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)));
			if request.event == Event::Revalidate {
				grant.tier = Some("moved".into());
			}
			ResponseTemplate::new(200).set_body_json(grant)
		})
		.await;

		let mut consumer = client(&server).connect(request()).await.unwrap();
		let fresh = tokio::time::timeout(Duration::from_secs(3), consumer.changed())
			.await
			.expect("a re-check within the cadence")
			.unwrap();
		assert_eq!(fresh.tier.as_deref(), Some("moved"));
		assert_eq!(log.events()[..2], [Event::Connect, Event::Revalidate]);
	}

	#[tokio::test]
	async fn a_refusal_on_recheck_revokes() {
		for answer in [
			ResponseTemplate::new(401),
			ResponseTemplate::new(403),
			ResponseTemplate::new(200).set_body_json(Grant::default()),
		] {
			let server = server(Log::default(), {
				let answer = answer.clone();
				move |request| match request.event {
					Event::Connect => ResponseTemplate::new(200)
						.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)))),
					_ => answer.clone(),
				}
			})
			.await;

			let consumer = client(&server).connect(request()).await.unwrap();
			let reason = tokio::time::timeout(Duration::from_secs(3), consumer.closed())
				.await
				.expect("revoked within the cadence");
			assert_eq!(reason, Reason::Refused);
		}
	}

	#[tokio::test]
	async fn an_invalid_grant_on_recheck_revokes() {
		let unbounded = grant(None, Some(Duration::from_secs(1)));
		let server = server(Log::default(), move |request| match request.event {
			Event::Connect => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)))),
			_ => ResponseTemplate::new(200).set_body_json(unbounded.clone()),
		})
		.await;

		let consumer = client(&server).connect(request()).await.unwrap();
		let reason = tokio::time::timeout(Duration::from_secs(3), consumer.closed())
			.await
			.expect("revoked within the cadence");
		assert_eq!(reason, Reason::Invalid);
	}

	#[tokio::test]
	async fn an_expired_grant_is_refused() {
		tokio::time::pause();
		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		grant.expires = Some(SystemTime::now() - Duration::from_secs(1));
		let script = Script::new(Log::default(), move |_| Some(Ok(grant.clone())));
		assert!(matches!(script.connect().await, Err(Error::GrantExpired)));
	}

	/// Regression: a cadence past the clock's range overflowed `Instant` and panicked
	/// the driver, at connect and on each reply. It never fires; the grant expires.
	#[tokio::test]
	async fn a_cadence_past_the_clock_never_rechecks() {
		tokio::time::pause();
		let log = Log::default();
		let script = Script::new(log.clone(), |_| {
			Some(Ok(grant(
				Some(Duration::from_secs(3)),
				Some(Duration::from_secs(u64::MAX)),
			)))
		});

		let consumer = script.connect().await.unwrap();
		// A nudge's reply schedules the next re-check the same way.
		consumer.revalidate();
		let reason = tokio::time::timeout(Duration::from_secs(4), consumer.closed())
			.await
			.expect("closed at expires");
		assert_eq!(reason, Reason::Expired);
		assert_eq!(log.revalidates(), 1, "only the nudge re-checked");
	}

	/// An outage is evidence of nothing: the grant stands through failed re-checks
	/// until `expires`, and closes then, not later.
	#[tokio::test]
	async fn an_outage_keeps_the_grant_until_expires() {
		tokio::time::pause();
		let expires = SystemTime::now() + Duration::from_secs(3);
		let log = Log::default();
		let script = Script::new(log.clone(), move |request| {
			Some(match request.event {
				Event::Connect => {
					let mut grant = grant(None, Some(Duration::from_secs(1)));
					grant.expires = Some(expires);
					Ok(grant)
				}
				_ => Err(Error::Unavailable("auth server answered 503".into())),
			})
		});

		// `expires` is wall-clock time, which the client maps onto Tokio's clock
		// somewhere inside `connect`, so bracket it: the bounds hold however long it takes.
		let (wall, tick) = (SystemTime::now(), tokio::time::Instant::now());
		let consumer = script.connect().await.unwrap();
		let earliest = tick + expires.duration_since(SystemTime::now()).unwrap();
		let latest = tokio::time::Instant::now() + expires.duration_since(wall).unwrap();

		// A millisecond either side for Tokio's timer resolution.
		let tolerance = Duration::from_millis(1);
		assert!(
			tokio::time::timeout_at(earliest - tolerance, consumer.closed())
				.await
				.is_err(),
			"an outage must not close before expires"
		);
		assert!(log.revalidates() >= 1, "the outage was answered before expires");
		assert_eq!(
			consumer.grant().publish,
			patterns(&["**"]).into(),
			"the grant stands through the outage"
		);
		let reason = tokio::time::timeout_at(latest + tolerance, consumer.closed())
			.await
			.expect("closed at expires, not later");
		assert_eq!(reason, Reason::Expired);
		consumer.close(reason, Bytes::default());
		assert!(matches!(
			log.end().await.event,
			Event::End {
				reason: Reason::Expired,
				..
			}
		));
	}

	#[tokio::test]
	async fn expiry_fires_while_a_recheck_is_stalled() {
		tokio::time::pause();
		let script = Script::new(Log::default(), |request| match request.event {
			Event::Connect => Some(Ok(grant(Some(Duration::from_secs(3)), Some(Duration::from_secs(1))))),
			_ => None,
		});
		let consumer = script.connect().await.unwrap();

		let reason = tokio::time::timeout(Duration::from_secs(5), consumer.closed())
			.await
			.expect("expired while the re-check was in flight");
		assert_eq!(reason, Reason::Expired);
	}

	#[tokio::test]
	async fn a_close_is_reported_while_a_recheck_is_stalled() {
		let log = Log::default();
		let server = server(log.clone(), |request| match request.event {
			Event::Connect => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)))),
			Event::Revalidate => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(60))))
				.set_delay(Duration::from_secs(30)),
			Event::End { .. } => ResponseTemplate::new(200),
		})
		.await;

		let consumer = client(&server).connect(request()).await.unwrap();
		tokio::time::sleep(Duration::from_millis(1500)).await;
		assert!(log.events().contains(&Event::Revalidate), "the re-check is in flight");
		consumer.close("disconnected", Bytes::default());
		assert!(matches!(
			log.end().await.event,
			Event::End {
				reason: Reason::Session(_),
				..
			}
		));
	}

	#[test]
	fn url_schemes_are_checked_at_construction() {
		assert!(Client::new("http://127.0.0.1:4440/".parse().unwrap(), None).is_ok());
		assert!(Client::new("http://localhost:4440/".parse().unwrap(), None).is_ok());
		assert!(Client::new("http://[::1]:4440/".parse().unwrap(), None).is_ok());
		assert!(matches!(
			Client::new("http://auth.example/".parse().unwrap(), None),
			Err(Error::InsecureUrl(_))
		));
		assert!(Client::new("https://auth.example/".parse().unwrap(), None).is_ok());
		assert!(matches!(
			Client::new("ftp://auth.example/".parse().unwrap(), None),
			Err(Error::InvalidUrl(_))
		));
		#[cfg(unix)]
		assert!(Client::new("unix:///run/moq-auth.sock".parse().unwrap(), None).is_ok());

		// The refused URL is reported, minus anything that may carry a credential.
		for url in [
			"http://user:pass@auth.example/?jwt=secret#frag",
			"ftp://user:pass@auth.example/?jwt=secret#frag",
		] {
			let err = Client::new(url.parse().unwrap(), None).err().expect("refused");
			let printed = format!("{err} {err:?}");
			assert!(printed.contains("auth.example/"), "{printed}");
			for secret in ["jwt", "secret", "user", "pass", "frag"] {
				assert!(!printed.contains(secret), "error leaked {secret}: {printed}");
			}
		}
	}

	/// Backoff leaves the driver in this same state (nothing in flight, a timer armed),
	/// so this covers a nudge during backoff too.
	#[tokio::test]
	async fn a_nudge_while_idle_posts_at_once() {
		let log = Log::default();
		let server = server(log.clone(), |_| {
			ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(3600))))
		})
		.await;

		let consumer = client(&server).connect(request()).await.unwrap();
		assert_eq!(log.events(), [Event::Connect]);
		consumer.revalidate();
		tokio::time::timeout(
			Duration::from_secs(2),
			log.until(|log| log.iter().any(|r| r.event == Event::Revalidate)),
		)
		.await
		.expect("a nudge while idle POSTs at once");
	}

	#[tokio::test]
	async fn a_nudge_during_inflight_posts_once_more_when_the_reply_lands() {
		let log = Log::default();
		let server = server(log.clone(), |request| match request.event {
			Event::Connect => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(3600)))),
			Event::Revalidate => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(3600))))
				.set_delay(Duration::from_millis(400)),
			Event::End { .. } => ResponseTemplate::new(200),
		})
		.await;

		let consumer = client(&server).connect(request()).await.unwrap();
		consumer.revalidate();
		tokio::time::timeout(
			Duration::from_secs(2),
			log.until(|log| log.iter().any(|r| r.event == Event::Revalidate)),
		)
		.await
		.expect("the first re-check is in flight");

		consumer.revalidate();
		consumer.revalidate();
		// `changed` jumps to the latest grant epoch, so two replies can arrive as one
		// observation. The request log does not coalesce.
		tokio::time::timeout(
			Duration::from_secs(3),
			log.until(|log| log.iter().filter(|r| r.event == Event::Revalidate).count() >= 2),
		)
		.await
		.expect("the in-flight nudge POSTs once more when the reply lands");
		consumer.close("disconnected", Bytes::default());
		log.end().await;
		assert_eq!(
			log.revalidates(),
			2,
			"a burst during an in-flight re-check is one extra POST"
		);
	}
}
