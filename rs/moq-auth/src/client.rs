use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

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
					return Err(Error::InsecureUrl(url.to_string()));
				}
				(builder, url)
			}
			"https" => match tls {
				Some(tls) => (builder.use_preconfigured_tls(tls), url),
				None => (builder, url),
			},
			#[cfg(unix)]
			"unix" => {
				let path = url.to_file_path().map_err(|()| Error::InvalidUrl(url.to_string()))?;
				// The socket is the transport; the request target is the server's root.
				let target = Url::parse("http://localhost/").expect("a constant URL parses");
				(builder.unix_socket(path), target)
			}
			_ => return Err(Error::InvalidUrl(url.to_string())),
		};

		Ok(Self {
			http: builder.build()?,
			url,
		})
	}

	/// Admit a session: POST `connect`, validate the reply, and return the lease the
	/// session holds. The session reports totals through [`lease::Consumer::close`].
	pub async fn connect(&self, request: Request) -> crate::Result<lease::Consumer> {
		let mut request = request;
		request.event = Event::Connect;

		let grant = self.post(&request).await?;
		let (producer, consumer) = lease::Producer::new(grant.clone());

		let driver = Driver {
			client: self.clone(),
			request,
			producer: Some(producer),
			started: Instant::now(),
		};
		tokio::spawn(driver.run());

		Ok(consumer)
	}

	/// One POST: a 2xx with a valid grant admits, a 401 or 403 refuses, a 2xx
	/// whose grant fails validation is that error, and everything else is an
	/// outage the caller decides about.
	async fn post(&self, request: &Request) -> crate::Result<Grant> {
		let response = self.http.post(self.url.clone()).json(request).send().await?;
		let status = response.status();
		if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
			return Err(Error::Refused);
		}
		if !status.is_success() {
			return Err(Error::Unavailable(format!("auth server answered {status}")));
		}
		let grant: Grant = response.json().await?;
		grant.validate().map_err(|err| match err {
			// An empty grant is a refusal, not a malformed answer.
			Error::UselessGrant => Error::Refused,
			other => other,
		})?;
		Ok(grant)
	}
}

/// The task behind a lease: re-checks on cadence and reports the end.
struct Driver {
	client: Client,
	request: Request,
	producer: Option<lease::Producer>,
	started: Instant,
}

impl Driver {
	async fn run(mut self) {
		let (reason, bytes) = self.drive().await;

		let mut request = self.request.clone();
		request.event = Event::End {
			reason,
			duration: self.started.elapsed(),
			bytes,
		};
		// The session is already gone; nothing to do with a failure but say so.
		if let Err(err) = self.client.post_end(&request).await {
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

		loop {
			let reply = async {
				match inflight.as_mut() {
					Some(request) => request.await,
					None => std::future::pending().await,
				}
			};
			tokio::select! {
				biased;
				ended = producer.closed() => return ended,
				due = producer.due() => match due {
					Due::Expired => return producer.finish(Reason::Expired, Bytes::default()),
					Due::Revalidate if inflight.is_some() => pending = true,
					Due::Revalidate => inflight = Some(self.post_revalidate()),
				},
				result = reply => {
					inflight = None;
					match result {
						Ok(fresh) => producer.update(fresh),
						Err(Error::Refused) => return producer.finish(Reason::Refused, Bytes::default()),
						Err(Error::GrantExpired | Error::UnboundedRevalidate | Error::ZeroRevalidate) => {
							return producer.finish(Reason::Invalid, Bytes::default());
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
		}
	}

	fn post_revalidate(&self) -> Pin<Box<dyn Future<Output = crate::Result<Grant>> + Send>> {
		let client = self.client.clone();
		let mut request = self.request.clone();
		request.event = Event::Revalidate;
		Box::pin(async move { client.post(&request).await })
	}
}

impl Client {
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

#[cfg(test)]
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

	/// Keep HTTP handling on the paused test clock instead of wiremock's separate runtime.
	async fn clock_server(log: Log, grant: Grant, stall: bool) -> Client {
		use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
		let router = Router::new()
			.route(
				"/",
				post(
					|State((log, grant, stall)): State<(Log, Grant, bool)>, Json(request): Json<Request>| async move {
						let event = request.event.clone();
						log.0.lock().push(request);
						match event {
							Event::Connect => Json(grant).into_response(),
							Event::Revalidate if stall => std::future::pending().await,
							Event::Revalidate => StatusCode::SERVICE_UNAVAILABLE.into_response(),
							Event::End { .. } => StatusCode::OK.into_response(),
						}
					},
				),
			)
			.with_state((log, grant, stall));
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("http://{}/", listener.local_addr().unwrap());
		tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
		// Only the lease clock is under test; an HTTP timeout can auto-advance
		// Tokio's paused clock before loopback I/O gets its first reactor turn.
		Client {
			http: reqwest::Client::builder().no_proxy().build().unwrap(),
			url: url.parse().unwrap(),
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
				401 | 403 => assert!(matches!(err, Error::Refused), "{status}: {err}"),
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
		assert!(matches!(err, Error::Refused), "{err}");
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
	async fn a_grant_within_clock_skew_stays_live() {
		tokio::time::pause();
		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		grant.expires = Some(SystemTime::now() - Duration::from_secs(1));
		let client = clock_server(Log::default(), grant, false).await;
		let consumer = client.connect(request()).await.unwrap();

		tokio::time::sleep(Duration::from_millis(500)).await;
		assert!(
			tokio::time::timeout(Duration::from_millis(100), consumer.closed())
				.await
				.is_err(),
			"still live inside the skew window"
		);

		let reason = tokio::time::timeout(crate::grant::CLOCK_SKEW + Duration::from_secs(1), consumer.closed())
			.await
			.expect("expired once the skew window ended");
		assert_eq!(reason, Reason::Expired);
	}

	#[tokio::test]
	async fn an_outage_keeps_the_grant_until_expires() {
		tokio::time::pause();
		let log = Log::default();
		let client = clock_server(
			log.clone(),
			grant(Some(Duration::from_secs(3)), Some(Duration::from_secs(1))),
			false,
		)
		.await;
		let consumer = client.connect(request()).await.unwrap();

		tokio::time::sleep(Duration::from_millis(1500)).await;
		assert!(log.revalidates() >= 1, "re-checks happened");
		assert_eq!(
			consumer.grant().publish,
			patterns(&["**"]),
			"the grant stands through the outage"
		);

		let reason = tokio::time::timeout(Duration::from_secs(5), consumer.closed())
			.await
			.expect("expired");
		assert_eq!(reason, Reason::Expired);
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
		let client = clock_server(
			Log::default(),
			grant(Some(Duration::from_secs(3)), Some(Duration::from_secs(1))),
			true,
		)
		.await;
		let consumer = client.connect(request()).await.unwrap();

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
