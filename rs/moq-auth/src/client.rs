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

/// The largest reply read from the auth server. Each one is held whole before it is
/// parsed, once per admission in flight, so the size bounds that memory; a grant
/// with thousands of patterns still fits.
const MAX_REPLY: usize = 1 << 20;

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
		// No client-wide timeout: `connect` runs under its caller's deadline, and the
		// requests that run after the handshake are bounded one by one. Every request carries
		// a credential in its body, which a followed 307 or 308 would resend to wherever it
		// points, so a redirect is refused rather than followed.
		let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());

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
	///
	/// Waits on the server for as long as the caller does: the connection's handshake
	/// deadline is the bound, and dropping the future drops the request. Every later
	/// request for the session ([`attach`](Self::attach), revalidate, end) is bounded
	/// on its own, since nothing else bounds it.
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
	/// The owner's driver stopped, so its `end` was posted or never will be.
	reported: bool,
}

/// End must follow every request an attached credential has in flight for this
/// connection id: its Connect, and each Revalidate until its driver stops.
struct Admission(Arc<Connection>);

/// Held by the owner's driver; dropping it, posted or not, releases
/// [`Context::reported`] so a task aborted mid-post cannot park a waiter forever.
struct Reporting(Context);

impl Drop for Reporting {
	fn drop(&mut self) {
		if self.0.owner {
			self.0.connection.lifecycle.lock().reported = true;
		}
	}
}

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

	/// Resolve once the connection's `end` was posted, or its driver stopped without
	/// posting it. At once for an attached credential, which never posts one.
	pub(crate) async fn reported(&self) {
		if !self.owner {
			return;
		}
		drop(
			self.connection
				.lifecycle
				.wait(|lifecycle| match lifecycle.reported {
					true => std::task::Poll::Ready(()),
					false => std::task::Poll::Pending,
				})
				.await,
		);
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
	/// A 2xx carries the grant, a 401 or 403 refuses, and everything else, a redirect
	/// included, is an outage the caller decides about. A JSON refusal carries its reason and
	/// verified attribution; legacy bodies refuse without attribution.
	async fn post(&self, request: &Request) -> crate::Result<Grant> {
		let response = self.http.post(self.url.clone()).json(request).send().await?;
		let status = response.status();
		if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
			let refusal = reply(response)
				.await
				.ok()
				.and_then(|body| serde_json::from_slice(&body).ok());
			return Err(Error::Refused(refusal.unwrap_or_default()));
		}
		check(status)?;
		serde_json::from_slice(&reply(response).await?)
			.map_err(|err| Error::Unavailable(format!("auth server answered an invalid grant: {err}")))
	}

	async fn post_end(&self, request: &Request) -> crate::Result<()> {
		let response = self.http.post(self.url.clone()).json(request).send().await?;
		check(response.status())
	}
}

/// Refuse anything but a 2xx, naming a redirect as one, since it is not followed.
fn check(status: reqwest::StatusCode) -> crate::Result<()> {
	if status.is_redirection() {
		return Err(Error::Unavailable(format!(
			"auth server answered {status}; redirects are not followed"
		)));
	}
	if !status.is_success() {
		return Err(Error::Unavailable(format!("auth server answered {status}")));
	}
	Ok(())
}

/// Read a reply body of at most [`MAX_REPLY`] bytes, refusing a longer one as it arrives.
async fn reply(mut response: reqwest::Response) -> crate::Result<Vec<u8>> {
	let mut body = Vec::new();
	while let Some(chunk) = response.chunk().await? {
		if body.len() + chunk.len() > MAX_REPLY {
			return Err(Error::Unavailable(format!(
				"auth server reply exceeds {MAX_REPLY} bytes"
			)));
		}
		body.extend_from_slice(&chunk);
	}
	Ok(body)
}

/// Admit a session through `server` and spawn the driver behind its lease.
///
/// The connection's own `connect` runs under the caller's deadline; an attached
/// credential arrives after the handshake, so its request is bounded here.
async fn admit<S: Post>(server: S, mut request: Request, context: Context) -> crate::Result<lease::Consumer> {
	request.event = Event::Connect;
	let admission = context.admitting()?;

	let grant = match context.owner {
		true => ask(&server, &request).await?,
		false => bounded(ask(&server, &request)).await?,
	};
	if context.connection.lifecycle.read().ended.is_some() {
		return Err(Error::Refused(Default::default()));
	}
	let (producer, mut consumer) = lease::Producer::new(grant.clone());
	consumer.set_context(context.clone());

	let driver = Driver {
		_reporting: Reporting(context.clone()),
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

/// Finish a request that nothing else bounds within [`TIMEOUT`], or report the
/// server as unavailable, so a stalled server cannot pile up requests.
async fn bounded<T>(request: impl Future<Output = crate::Result<T>>) -> crate::Result<T> {
	moq_clock::timeout(TIMEOUT, request).await.unwrap_or_else(|_| {
		Err(Error::Unavailable(format!(
			"the auth server did not answer within {TIMEOUT:?}"
		)))
	})
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
	/// Dropped after the `end` POST returns, or with the task.
	_reporting: Reporting,
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
		if let Err(err) = bounded(self.server.post_end(&request)).await {
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
		Box::pin(async move { bounded(ask(&server, &request)).await })
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_pattern::Patterns;
	use std::task::Poll;
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
		#[expect(
			clippy::type_complexity,
			reason = "a one-off test responder; an alias would only move the type"
		)]
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
		grant.expires = expires_in.map(|d| moq_clock::wall() + d);
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
		assert_eq!(consumer.grant().publish, patterns(&["**"]));
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

	/// A closed lease is reported only once its `end` POST returns, so a caller
	/// exiting on it never drops the decider's last word.
	#[tokio::test(start_paused = true)]
	async fn reported_waits_for_the_end_post() {
		#[derive(Clone)]
		struct HeldEnd {
			script: Script,
			release: Arc<tokio::sync::Semaphore>,
		}
		impl Post for HeldEnd {
			async fn post(&self, request: &Request) -> crate::Result<Grant> {
				self.script.post(request).await
			}
			async fn post_end(&self, request: &Request) -> crate::Result<()> {
				self.script.post_end(request).await?;
				drop(self.release.acquire().await);
				Ok(())
			}
		}
		let log = Log::default();
		let script = Script::new(log.clone(), |_| Some(Ok(grant(None, None))));
		let server = HeldEnd {
			script: script.clone(),
			release: Arc::new(tokio::sync::Semaphore::new(0)),
		};
		let owner = admit(
			server.clone(),
			request(),
			Context::new(request(), script.identity.clone()),
		)
		.await
		.unwrap();
		let mut reported = std::pin::pin!(owner.reported());
		tokio::task::yield_now().await;
		assert!(pending(reported.as_mut()), "reported while still open");

		owner.close(Reason::Shutdown, Bytes::default());
		log.end().await;
		assert!(pending(reported.as_mut()), "reported before the end POST returned");
		server.release.add_permits(1);
		reported.await;
	}

	/// An `end` POST that fails still releases a caller waiting on the report.
	#[tokio::test(start_paused = true)]
	async fn a_failed_end_post_is_still_reported() {
		#[derive(Clone)]
		struct FailedEnd(Script);
		impl Post for FailedEnd {
			async fn post(&self, request: &Request) -> crate::Result<Grant> {
				self.0.post(request).await
			}
			async fn post_end(&self, _: &Request) -> crate::Result<()> {
				Err(Error::Unavailable("down".into()))
			}
		}
		let script = Script::new(Log::default(), |_| Some(Ok(grant(None, None))));
		let owner = admit(
			FailedEnd(script.clone()),
			request(),
			Context::new(request(), script.identity.clone()),
		)
		.await
		.unwrap();
		let reported = owner.reported();
		owner.close("done", Bytes::default());
		reported.await;
	}

	/// A lease nobody drives has nobody to tell, so it is reported as it ends.
	#[tokio::test]
	async fn a_fixed_lease_is_reported_at_close() {
		let consumer = lease::Consumer::fixed(grant(None, None));
		let mut reported = std::pin::pin!(consumer.reported());
		assert!(pending(reported.as_mut()), "reported while still open");
		consumer.close("done", Bytes::default());
		assert!(!pending(reported), "a fixed lease waited on nobody");
	}

	/// Poll `future` once, reporting whether it is still pending.
	fn pending(future: Pin<&mut impl Future>) -> bool {
		future
			.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
			.is_pending()
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

	/// `connect` waits as long as its caller does, so a server answering past the
	/// fixed bound still admits the session. An attached credential arrives after the
	/// handshake, which nothing else bounds, so it keeps the bound.
	#[tokio::test(start_paused = true)]
	async fn connect_waits_for_a_slow_server_but_attach_does_not() {
		#[derive(Clone)]
		struct Slow(Script);
		impl Post for Slow {
			async fn post(&self, request: &Request) -> crate::Result<Grant> {
				moq_clock::sleep(TIMEOUT + Duration::from_secs(1)).await;
				self.0.post(request).await
			}
			async fn post_end(&self, request: &Request) -> crate::Result<()> {
				self.0.post_end(request).await
			}
		}
		let script = Script::new(Log::default(), |_| Some(Ok(grant(None, None))));
		let server = Slow(script.clone());

		let start = moq_clock::now();
		let owner = match admit(
			server.clone(),
			request(),
			Context::new(request(), script.identity.clone()),
		)
		.await
		{
			Ok(owner) => owner,
			Err(err) => panic!("connect gave up on a slow server: {err}"),
		};
		assert_eq!(start.elapsed(), TIMEOUT + Duration::from_secs(1));

		let mut attached = request();
		attached.query = Some("jwt=replacement".into());
		let context = owner.context().unwrap().attach(&attached, &script.identity).unwrap();
		let start = moq_clock::now();
		let Err(err) = admit(server, attached, context).await else {
			panic!("an attached credential waited past its bound");
		};
		assert!(matches!(err, Error::Unavailable(_)), "{err:?}");
		assert_eq!(start.elapsed(), TIMEOUT);
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

	/// A reply of exactly `MAX_REPLY` bytes admits; one byte more is an outage, or a
	/// plain refusal on a 403, never read whole.
	#[tokio::test]
	async fn replies_are_bounded() {
		let padded = |status: u16, body: &str, len: usize| {
			// JSON allows trailing whitespace, so the padded reply still parses.
			let body = format!("{body}{}", " ".repeat(len - body.len()));
			ResponseTemplate::new(status).set_body_raw(body, "application/json")
		};
		let grant = serde_json::to_string(&grant(None, None)).unwrap();
		let refusal = r#"{"reason":"expired","root":"tenant"}"#;

		let fits = server(Log::default(), {
			let grant = grant.clone();
			move |_| padded(200, &grant, MAX_REPLY)
		})
		.await;
		client(&fits)
			.connect(request())
			.await
			.expect("a reply at the bound admits");

		let over = server(Log::default(), move |_| padded(200, &grant, MAX_REPLY + 1)).await;
		let err = client(&over).connect(request()).await.unwrap_err();
		assert!(
			matches!(&err, Error::Unavailable(message) if message.contains("exceeds")),
			"{err}"
		);

		let refused = server(Log::default(), move |_| padded(403, refusal, MAX_REPLY + 1)).await;
		let Error::Refused(actual) = client(&refused).connect(request()).await.unwrap_err() else {
			panic!("expected refusal")
		};
		assert_eq!(actual, crate::Refusal::default());
	}

	#[tokio::test]
	async fn typed_refusal_preserves_reason_root_and_tier() {
		use crate::refusal::Reason;
		for reason in [Reason::Refused, Reason::Forbidden, Reason::Expired, Reason::Malformed] {
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

	/// A 307 or 308 from the auth server fails every credential-bearing request, and the body
	/// never reaches the destination the redirect names.
	#[tokio::test]
	async fn redirects_are_refused_without_resending_the_credential() {
		let foreign = MockServer::start().await;
		Mock::given(method("POST"))
			.respond_with(ResponseTemplate::new(200))
			.mount(&foreign)
			.await;
		for status in [307, 308] {
			let auth = MockServer::start().await;
			Mock::given(method("POST"))
				.respond_with(
					ResponseTemplate::new(status).insert_header("location", format!("{}/foreign", foreign.uri())),
				)
				.mount(&auth)
				.await;
			let client = Client::new(auth.uri().parse().unwrap(), None).unwrap();

			let refused = |err: Error| {
				assert!(
					matches!(&err, Error::Unavailable(message) if message.contains("redirects are not followed")),
					"{status}: {err}"
				);
			};
			refused(client.connect(request()).await.unwrap_err());
			refused(Post::post(&client, &request()).await.unwrap_err());
			refused(Post::post_end(&client, &request()).await.unwrap_err());
			assert_eq!(auth.received_requests().await.unwrap().len(), 3);
		}
		assert!(
			foreign.received_requests().await.unwrap().is_empty(),
			"a credential followed the redirect"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn revalidate_runs_on_cadence_and_applies_the_reply() {
		let log = Log::default();
		let script = Script::new(log.clone(), |request| {
			let mut grant = grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)));
			if request.event == Event::Revalidate {
				grant.tier = Some("moved".into());
			}
			Some(Ok(grant))
		});

		let mut consumer = script.connect().await.unwrap();
		let fresh = moq_clock::timeout(Duration::from_secs(3), consumer.changed())
			.await
			.expect("a re-check within the cadence")
			.unwrap();
		assert_eq!(fresh.tier.as_deref(), Some("moved"));
		assert_eq!(log.events()[..2], [Event::Connect, Event::Revalidate]);
	}

	/// A 401 or 403 is the same refusal (`refusals_and_outages_refuse_at_connect` maps
	/// them), and an empty grant is a no too.
	#[tokio::test(start_paused = true)]
	async fn a_refusal_on_recheck_revokes() {
		let refusals: [fn() -> crate::Result<Grant>; 2] =
			[|| Err(Error::Refused(Default::default())), || Ok(Grant::default())];
		for refuse in refusals {
			let script = Script::new(Log::default(), move |request| {
				Some(match request.event {
					Event::Connect => Ok(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)))),
					_ => refuse(),
				})
			});

			let consumer = script.connect().await.unwrap();
			let reason = moq_clock::timeout(Duration::from_secs(3), consumer.closed())
				.await
				.expect("revoked within the cadence");
			assert_eq!(reason, Reason::Refused);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn an_invalid_grant_on_recheck_revokes() {
		let script = Script::new(Log::default(), |request| {
			Some(Ok(match request.event {
				Event::Connect => grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1))),
				_ => grant(None, Some(Duration::from_secs(1))),
			}))
		});

		let consumer = script.connect().await.unwrap();
		let reason = moq_clock::timeout(Duration::from_secs(3), consumer.closed())
			.await
			.expect("revoked within the cadence");
		assert_eq!(reason, Reason::Invalid);
	}

	#[tokio::test]
	async fn an_expired_grant_is_refused() {
		tokio::time::pause();
		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		grant.expires = Some(moq_clock::wall() - Duration::from_secs(1));
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
		let reason = moq_clock::timeout(Duration::from_secs(4), consumer.closed())
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
		let expires = moq_clock::wall() + Duration::from_secs(3);
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
		let (wall, tick) = (moq_clock::wall(), moq_clock::now());
		let consumer = script.connect().await.unwrap();
		let earliest = tick + expires.checked_duration_since(moq_clock::wall()).unwrap();
		let latest = moq_clock::now() + expires.checked_duration_since(wall).unwrap();

		// A millisecond either side for Tokio's timer resolution.
		let tolerance = Duration::from_millis(1);
		assert!(
			moq_clock::timeout_at(earliest - tolerance, consumer.closed())
				.await
				.is_err(),
			"an outage must not close before expires"
		);
		assert!(log.revalidates() >= 1, "the outage was answered before expires");
		assert_eq!(
			consumer.grant().publish,
			patterns(&["**"]),
			"the grant stands through the outage"
		);
		let reason = moq_clock::timeout_at(latest + tolerance, consumer.closed())
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

		let reason = moq_clock::timeout(Duration::from_secs(5), consumer.closed())
			.await
			.expect("expired while the re-check was in flight");
		assert_eq!(reason, Reason::Expired);
	}

	#[tokio::test(start_paused = true)]
	async fn a_close_is_reported_while_a_recheck_is_stalled() {
		let log = Log::default();
		let script = Script::new(log.clone(), |request| match request.event {
			Event::Connect => Some(Ok(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1))))),
			_ => None,
		});

		let consumer = script.connect().await.unwrap();
		log.until(|log| log.iter().any(|r| r.event == Event::Revalidate)).await;
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
		moq_clock::timeout(
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
		moq_clock::timeout(
			Duration::from_secs(2),
			log.until(|log| log.iter().any(|r| r.event == Event::Revalidate)),
		)
		.await
		.expect("the first re-check is in flight");

		consumer.revalidate();
		consumer.revalidate();
		// `changed` jumps to the latest grant epoch, so two replies can arrive as one
		// observation. The request log does not coalesce.
		moq_clock::timeout(
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
