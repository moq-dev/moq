use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use url::Url;

use crate::lease::{self, Reason};
use crate::{Bytes, Error, Event, Grant, Request};

/// Every request is bounded so a hung server refuses rather than parks the session.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The longest a failed re-check waits before trying again.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Two shared byte totals a session adds to as it sends and receives.
///
/// Cheap to clone; every clone reads and writes the same counters. The client reads
/// them for the `end` event, so it never reaches into a session, and a caller with no
/// meter passes `Counters::default()`.
#[derive(Clone, Default, Debug)]
pub struct Counters(Arc<Totals>);

#[derive(Default, Debug)]
struct Totals {
	sent: AtomicU64,
	received: AtomicU64,
}

impl Counters {
	/// Count bytes sent to the peer.
	pub fn add_sent(&self, bytes: u64) {
		self.0.sent.fetch_add(bytes, Ordering::Relaxed);
	}

	/// Count bytes received from the peer.
	pub fn add_received(&self, bytes: u64) {
		self.0.received.fetch_add(bytes, Ordering::Relaxed);
	}

	/// The totals so far.
	pub fn bytes(&self) -> Bytes {
		Bytes {
			sent: self.0.sent.load(Ordering::Relaxed),
			received: self.0.received.load(Ordering::Relaxed),
		}
	}
}

/// The HTTP side of the contract: one JSON POST per event to an auth server.
///
/// `connect` admits a session and hands back the [`lease::Consumer`] it holds; a task
/// behind it re-POSTs `revalidate` on the grant's cadence, applies each reply, revokes
/// when the server refuses or the grant expires, and POSTs `end` when the session
/// closes.
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
	/// session holds. `bytes` is read for the `end` event; pass what the session meters.
	pub async fn connect(&self, request: Request, bytes: Counters) -> crate::Result<lease::Consumer> {
		let mut request = request;
		request.event = Event::Connect;

		let grant = self.post(&request).await?;
		let (producer, consumer) = lease::Producer::new(grant.clone());

		let driver = Driver {
			client: self.clone(),
			request,
			producer: Some(producer),
			bytes,
			started: Instant::now(),
		};
		tokio::spawn(driver.run(grant));

		Ok(consumer)
	}

	/// One POST: a 2xx with a valid grant admits, a 403 refuses, and everything else is
	/// an outage the caller decides about.
	async fn post(&self, request: &Request) -> crate::Result<Grant> {
		let response = self.http.post(self.url.clone()).json(request).send().await?;
		let status = response.status();
		if status == reqwest::StatusCode::FORBIDDEN {
			return Err(Error::Refused);
		}
		if !status.is_success() {
			return Err(Error::Unavailable(format!("auth server answered {status}")));
		}
		let grant: Grant = response.json().await?;
		grant.validate()?;
		Ok(grant)
	}
}

/// The task behind a lease: re-checks on cadence and reports the end.
struct Driver {
	client: Client,
	request: Request,
	producer: Option<lease::Producer>,
	bytes: Counters,
	started: Instant,
}

impl Driver {
	async fn run(mut self, grant: Grant) {
		let reason = self.drive(grant).await;

		let mut request = self.request.clone();
		request.event = Event::End {
			reason,
			duration: self.started.elapsed(),
			bytes: self.bytes.bytes(),
		};
		// The session is already gone; nothing to do with a failure but say so.
		if let Err(err) = self.client.post_end(&request).await {
			tracing::warn!(id = %request.id, %err, "failed to report the session end");
		}
	}

	/// Re-check until the lease ends, returning why it did.
	async fn drive(&mut self, mut grant: Grant) -> Reason {
		let producer = self
			.producer
			.take()
			.expect("the driver owns the producer until it ends");
		let mut failures = 0u32;
		let mut next = grant.revalidate.map(|cadence| Instant::now() + cadence);
		// The re-check in flight, kept out of the select so expiry and the session's
		// close are still polled while a stalled server holds the reply.
		let mut inflight: Option<Pin<Box<dyn Future<Output = crate::Result<Grant>> + Send>>> = None;

		loop {
			let expires = grant.expires.map(until);
			let revalidate = async {
				match next {
					Some(at) => tokio::time::sleep_until(at.into()).await,
					None => std::future::pending().await,
				}
			};
			let expire = async {
				match expires {
					Some(after) => tokio::time::sleep(after).await,
					None => std::future::pending().await,
				}
			};
			let reply = async {
				match inflight.as_mut() {
					Some(request) => request.await,
					None => std::future::pending().await,
				}
			};

			tokio::select! {
				reason = producer.closed() => return reason,
				() = expire => {
					producer.revoke(Reason::Expired);
					return Reason::Expired;
				}
				result = reply => {
					inflight = None;
					match result {
						Ok(fresh) => {
							failures = 0;
							next = fresh.revalidate.map(|cadence| Instant::now() + cadence);
							producer.update(fresh.clone());
							grant = fresh;
						}
						Err(Error::Refused) => {
							producer.revoke(Reason::Refused);
							return Reason::Refused;
						}
						Err(err) => {
							// Evidence of nothing: the grant stands until `expires`.
							failures += 1;
							let delay = backoff(failures, grant.revalidate.unwrap_or(BACKOFF_MAX));
							tracing::warn!(id = %self.request.id, %err, ?delay, "auth revalidation failed; retrying");
							next = Some(Instant::now() + delay);
						}
					}
				}
				() = revalidate => {
					// One re-check at a time; the reply schedules the next.
					next = None;
					let client = self.client.clone();
					let mut request = self.request.clone();
					request.event = Event::Revalidate;
					inflight = Some(Box::pin(async move { client.post(&request).await }));
				}
			}
		}
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

/// How long until `at`, or zero when it has passed.
fn until(at: SystemTime) -> Duration {
	at.duration_since(SystemTime::now()).unwrap_or_default()
}

/// Exponential backoff from one second, capped by the cadence and [`BACKOFF_MAX`],
/// jittered by up to a quarter so a fleet does not retry in lockstep.
fn backoff(failures: u32, cadence: Duration) -> Duration {
	use rand::RngExt;
	let base = Duration::from_secs(1) * 2u32.saturating_pow(failures.saturating_sub(1).min(16));
	let base = base.min(cadence).min(BACKOFF_MAX);
	let jitter = rand::rng().random_range(0.75..=1.25);
	base.mul_f64(jitter)
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_pattern::Patterns;
	use std::sync::Mutex;
	use wiremock::matchers::{method, path};
	use wiremock::{Mock, MockServer, Request as Received, ResponseTemplate};

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn request() -> Request {
		Request::new("0123", "relay-1", crate::Transport::Quic, "/demo/room")
	}

	/// Records every request body the server saw, in order.
	#[derive(Clone, Default)]
	struct Log(Arc<Mutex<Vec<Request>>>);

	impl Log {
		fn events(&self) -> Vec<Event> {
			self.0.lock().unwrap().iter().map(|r| r.event.clone()).collect()
		}

		fn last(&self) -> Request {
			self.0.lock().unwrap().last().cloned().unwrap()
		}
	}

	impl wiremock::Match for Log {
		fn matches(&self, received: &Received) -> bool {
			self.0.lock().unwrap().push(received.body_json().unwrap());
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

	async fn settle() {
		tokio::time::sleep(Duration::from_millis(50)).await;
	}

	#[tokio::test]
	async fn connect_admits_and_end_follows_the_close() {
		let log = Log::default();
		let server = server(log.clone(), |_| {
			ResponseTemplate::new(200).set_body_json(grant(None, None))
		})
		.await;

		let bytes = Counters::default();
		let consumer = client(&server).connect(request(), bytes.clone()).await.unwrap();
		assert_eq!(consumer.grant().publish, patterns(&["**"]));
		assert_eq!(log.events(), [Event::Connect]);

		bytes.add_sent(7);
		bytes.add_received(11);
		consumer.close("disconnected");
		settle().await;

		let end = log.last();
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

		let consumer = client(&server).connect(request(), Counters::default()).await.unwrap();
		drop(consumer);
		settle().await;

		assert!(matches!(
			log.last().event,
			Event::End {
				reason: Reason::Dropped,
				..
			}
		));
	}

	#[tokio::test]
	async fn refusals_and_outages_refuse_at_connect() {
		for status in [403, 404, 500, 503] {
			let server = server(Log::default(), move |_| ResponseTemplate::new(status)).await;
			let err = client(&server)
				.connect(request(), Counters::default())
				.await
				.unwrap_err();
			match status {
				403 => assert!(matches!(err, Error::Refused), "{status}: {err}"),
				_ => assert!(matches!(err, Error::Unavailable(_)), "{status}: {err}"),
			}
		}

		let garbage = server(Log::default(), |_| ResponseTemplate::new(200).set_body_string("nope")).await;
		let err = client(&garbage)
			.connect(request(), Counters::default())
			.await
			.unwrap_err();
		assert!(matches!(err, Error::Unavailable(_)), "{err}");

		let nothing = server(Log::default(), |_| {
			ResponseTemplate::new(200).set_body_json(Grant::default())
		})
		.await;
		let err = client(&nothing)
			.connect(request(), Counters::default())
			.await
			.unwrap_err();
		assert!(matches!(err, Error::UselessGrant), "{err}");
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

		let mut consumer = client(&server).connect(request(), Counters::default()).await.unwrap();
		let fresh = tokio::time::timeout(Duration::from_secs(3), consumer.changed())
			.await
			.expect("a re-check within the cadence")
			.unwrap();
		assert_eq!(fresh.tier.as_deref(), Some("moved"));
		assert_eq!(log.events()[..2], [Event::Connect, Event::Revalidate]);
	}

	#[tokio::test]
	async fn a_refusal_on_recheck_revokes() {
		let server = server(Log::default(), |request| match request.event {
			Event::Connect => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(1)))),
			_ => ResponseTemplate::new(403),
		})
		.await;

		let consumer = client(&server).connect(request(), Counters::default()).await.unwrap();
		let reason = tokio::time::timeout(Duration::from_secs(3), consumer.closed())
			.await
			.expect("revoked within the cadence");
		assert_eq!(reason, Reason::Refused);
	}

	#[tokio::test]
	async fn an_outage_keeps_the_grant_until_expires() {
		let log = Log::default();
		let server = server(log.clone(), |request| match request.event {
			Event::Connect => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3)), Some(Duration::from_secs(1)))),
			_ => ResponseTemplate::new(503),
		})
		.await;

		let consumer = client(&server).connect(request(), Counters::default()).await.unwrap();
		tokio::time::sleep(Duration::from_millis(1500)).await;
		assert!(
			log.events().iter().filter(|e| **e == Event::Revalidate).count() >= 1,
			"re-checks happened"
		);
		assert_eq!(
			consumer.grant().publish,
			patterns(&["**"]),
			"the grant stands through the outage"
		);

		let reason = tokio::time::timeout(Duration::from_secs(5), consumer.closed())
			.await
			.expect("expired");
		assert_eq!(reason, Reason::Expired);
		settle().await;
		assert!(matches!(
			log.last().event,
			Event::End {
				reason: Reason::Expired,
				..
			}
		));
	}

	#[tokio::test]
	async fn expiry_fires_while_a_recheck_is_stalled() {
		let server = server(Log::default(), |request| match request.event {
			Event::Connect => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3)), Some(Duration::from_secs(1)))),
			_ => ResponseTemplate::new(200)
				.set_body_json(grant(Some(Duration::from_secs(3600)), Some(Duration::from_secs(60))))
				.set_delay(Duration::from_secs(30)),
		})
		.await;

		let consumer = client(&server).connect(request(), Counters::default()).await.unwrap();
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

		let consumer = client(&server).connect(request(), Counters::default()).await.unwrap();
		tokio::time::sleep(Duration::from_millis(1500)).await;
		assert!(log.events().contains(&Event::Revalidate), "the re-check is in flight");
		consumer.close("disconnected");
		settle().await;
		assert!(matches!(
			log.last().event,
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

	#[test]
	fn backoff_grows_and_stays_bounded() {
		let cadence = Duration::from_secs(30);
		let first = backoff(1, cadence);
		assert!(
			first >= Duration::from_millis(750) && first <= Duration::from_millis(1250),
			"{first:?}"
		);
		let later = backoff(10, cadence);
		assert!(later <= cadence.mul_f64(1.25), "{later:?}");
		assert!(backoff(40, Duration::from_secs(3600)) <= BACKOFF_MAX.mul_f64(1.25));
	}
}
