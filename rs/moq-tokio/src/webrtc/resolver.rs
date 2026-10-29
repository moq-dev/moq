//! Resolves a peer's `.local` candidates over mDNS, one distinct name at a time.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;

use super::{Command, Error, candidate_address, with_address};
use crate::abort::AbortOnDrop;

/// How long to wait for a `.local` name to resolve: mdns-sd's first query plus its
/// retransmissions at one and two seconds (RFC 6762 section 5.2).
const MDNS_TIMEOUT: Duration = Duration::from_secs(5);

/// Looks up the address behind a `.local` host name, or `None` if it does not resolve.
pub(super) trait Lookup: Send + Sync + 'static {
	fn lookup(&self, host: &str) -> impl Future<Output = Option<IpAddr>> + Send;
}

/// A peer's resolver task, cancelled when this drops.
///
/// The task resolves one name at a time and takes one batch of lines ahead of the
/// one it is working on, so a peer's queued work is bounded by what its signaling
/// already sent, and its signaling waits behind its own slow names.
pub(super) struct Resolver {
	batches: mpsc::Sender<Vec<String>>,
	_task: AbortOnDrop,
}

impl Resolver {
	/// Start resolving into the driver's `commands`.
	pub fn spawn(commands: mpsc::Sender<Command>, lookup: impl Lookup) -> Self {
		let (batches, rx) = mpsc::channel(1);
		use tracing::Instrument;
		let task = crate::runtime::spawn(run(rx, commands, lookup).instrument(tracing::Span::current()));
		Self {
			batches,
			_task: AbortOnDrop::new(task),
		}
	}

	/// Queue `.local` candidate lines, waiting while the resolver is behind.
	pub async fn add(&self, lines: Vec<String>) -> Result<(), Error> {
		if lines.is_empty() {
			return Ok(());
		}
		self.batches.send(lines).await.map_err(|_| Error::Closed)
	}
}

async fn run(batches: mpsc::Receiver<Vec<String>>, commands: mpsc::Sender<Command>, lookup: impl Lookup) {
	// Once the driver exits no lookup can reach a connection, so stop, and let
	// signaling see the peer is closed.
	tokio::select! {
		_ = commands.closed() => {}
		_ = resolve(batches, &commands, lookup) => {}
	}
}

async fn resolve(mut batches: mpsc::Receiver<Vec<String>>, commands: &mpsc::Sender<Command>, lookup: impl Lookup) {
	while let Some(lines) = batches.recv().await {
		for (host, lines) in by_host(&lines) {
			let Some(ip) = lookup.lookup(host).await else {
				tracing::debug!(%host, "dropped remote candidates: the name did not resolve");
				continue;
			};
			for line in lines {
				match with_address(line, ip) {
					Ok(candidate) => {
						if commands.send(Command::Candidate(candidate)).await.is_err() {
							return;
						}
					}
					Err(err) => tracing::debug!(%err, "dropped a remote candidate"),
				}
			}
		}
	}
}

/// Group candidate lines by host name, in the order each name first appears.
fn by_host(lines: &[String]) -> Vec<(&str, Vec<&str>)> {
	let mut index = HashMap::new();
	let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
	for line in lines {
		let Some(host) = candidate_address(line) else {
			continue;
		};
		let group = *index.entry(host).or_insert_with(|| {
			groups.push((host, Vec::new()));
			groups.len() - 1
		});
		groups[group].1.push(line);
	}
	groups
}

/// The process-wide mDNS daemon, started on the first `.local` candidate.
static MDNS: Mutex<Option<mdns_sd::ServiceDaemon>> = Mutex::new(None);

/// Multicast DNS, preferring a name's IPv4 address.
pub(super) struct Mdns;

impl Lookup for Mdns {
	async fn lookup(&self, host: &str) -> Option<IpAddr> {
		let daemon = {
			let mut daemon = MDNS.lock().unwrap();
			match daemon.as_ref() {
				Some(daemon) => daemon.clone(),
				None => daemon
					.insert(
						mdns_sd::ServiceDaemon::new()
							.inspect_err(|err| tracing::debug!(%err, "failed to start the mDNS daemon"))
							.ok()?,
					)
					.clone(),
			}
		};

		let name = format!("{host}.");
		let timeout = MDNS_TIMEOUT.as_millis() as u64;
		let events = daemon
			.resolve_hostname(&name, Some(timeout))
			.inspect_err(|err| tracing::debug!(%err, "failed to query mDNS"))
			.ok()?;
		let found = loop {
			match events.recv_async().await {
				Ok(mdns_sd::HostnameResolutionEvent::AddressesFound(_, addrs)) if !addrs.is_empty() => {
					let addrs: Vec<IpAddr> = addrs.iter().map(|addr| addr.to_ip_addr()).collect();
					break addrs.iter().find(|ip| ip.is_ipv4()).or(addrs.first()).copied();
				}
				Ok(mdns_sd::HostnameResolutionEvent::SearchTimeout(_)) | Err(_) => break None,
				Ok(_) => {}
			}
		};
		let _ = daemon.stop_resolve_hostname(&name);
		found
	}
}

#[cfg(test)]
mod tests {
	use std::net::Ipv4Addr;
	use std::sync::Arc;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	/// How long the fake takes to answer, on the paused clock.
	const DELAY: Duration = Duration::from_secs(1);

	/// Answers each name after [`DELAY`], recording the queries and how many overlap.
	#[derive(Clone, Default)]
	struct Fake {
		queries: Arc<Mutex<Vec<String>>>,
		in_flight: Arc<AtomicUsize>,
		most_in_flight: Arc<AtomicUsize>,
	}

	impl Fake {
		fn queries(&self) -> Vec<String> {
			self.queries.lock().unwrap().clone()
		}
	}

	/// Counts a query in flight until its future completes or is dropped.
	struct InFlight(Arc<AtomicUsize>);

	impl Drop for InFlight {
		fn drop(&mut self) {
			self.0.fetch_sub(1, Ordering::SeqCst);
		}
	}

	impl Lookup for Fake {
		async fn lookup(&self, host: &str) -> Option<IpAddr> {
			self.queries.lock().unwrap().push(host.to_string());
			let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
			let _in_flight = InFlight(self.in_flight.clone());
			self.most_in_flight.fetch_max(now, Ordering::SeqCst);
			kio::clock::sleep(DELAY).await;
			let index: u8 = host
				.trim_start_matches("host")
				.trim_end_matches(".local")
				.parse()
				.unwrap();
			Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, index)))
		}
	}

	fn line(foundation: usize, host: usize) -> String {
		format!("candidate:{foundation} 1 udp 2113937151 host{host}.local 54321 typ host")
	}

	/// The next candidate the resolver handed the driver, or `None` once it stopped.
	async fn next(commands: &mut mpsc::Receiver<Command>) -> Option<str0m::Candidate> {
		match commands.recv().await? {
			Command::Candidate(candidate) => Some(candidate),
			_ => panic!("the resolver only sends candidates"),
		}
	}

	#[tokio::test(start_paused = true)]
	async fn lines_sharing_a_name_issue_one_query() {
		let fake = Fake::default();
		let (tx, mut commands) = mpsc::channel(1);
		let resolver = Resolver::spawn(tx, fake.clone());

		resolver.add((0..100).map(|i| line(i, 7)).collect()).await.unwrap();
		for _ in 0..100 {
			let candidate = next(&mut commands).await.unwrap();
			assert_eq!(candidate.addr(), "192.0.2.7:54321".parse().unwrap());
		}
		assert_eq!(fake.queries(), ["host7.local"]);
	}

	#[tokio::test(start_paused = true)]
	async fn distinct_names_resolve_one_at_a_time() {
		let fake = Fake::default();
		let (tx, mut commands) = mpsc::channel(1);
		let resolver = Resolver::spawn(tx, fake.clone());

		let start = kio::clock::now();
		resolver.add((0..10).map(|i| line(i, i)).collect()).await.unwrap();
		for i in 0..10 {
			let candidate = next(&mut commands).await.unwrap();
			assert_eq!(candidate.addr().ip(), IpAddr::V4(Ipv4Addr::new(192, 0, 2, i as u8)));
		}
		assert_eq!(fake.most_in_flight.load(Ordering::SeqCst), 1);
		assert_eq!(start.elapsed(), DELAY * 10);
	}

	#[tokio::test(start_paused = true)]
	async fn trickled_names_wait_for_the_resolver() {
		let fake = Fake::default();
		let (tx, _commands) = mpsc::channel(1);
		let resolver = Resolver::spawn(tx, fake.clone());

		// The first is resolving and the second waits in the channel.
		resolver.add(vec![line(0, 0)]).await.unwrap();
		resolver.add(vec![line(1, 1)]).await.unwrap();

		// The third waits for the first to finish, rather than queueing.
		let start = kio::clock::now();
		resolver.add(vec![line(2, 2)]).await.unwrap();
		assert_eq!(start.elapsed(), DELAY);
		assert_eq!(fake.most_in_flight.load(Ordering::SeqCst), 1);
	}

	#[tokio::test(start_paused = true)]
	async fn dropping_the_resolver_cancels_resolution() {
		let fake = Fake::default();
		let (tx, mut commands) = mpsc::channel(1);
		let resolver = Resolver::spawn(tx, fake.clone());

		resolver.add(vec![line(0, 0), line(1, 1)]).await.unwrap();
		while fake.queries().is_empty() {
			tokio::task::yield_now().await;
		}
		drop(resolver);

		// The task is gone, with its query and its handle on the driver.
		assert!(next(&mut commands).await.is_none());
		assert_eq!(fake.in_flight.load(Ordering::SeqCst), 0);
		assert_eq!(fake.queries(), ["host0.local"]);
	}

	#[tokio::test(start_paused = true)]
	async fn resolution_stops_with_the_driver() {
		let fake = Fake::default();
		let (tx, commands) = mpsc::channel(1);
		let resolver = Resolver::spawn(tx, fake.clone());

		resolver.add(vec![line(0, 0), line(1, 1)]).await.unwrap();
		while fake.queries().is_empty() {
			tokio::task::yield_now().await;
		}
		drop(commands);

		// The query in flight is cancelled before it answers, the rest of the batch
		// is skipped, and signaling sees the peer is closed.
		kio::clock::sleep(DELAY / 2).await;
		assert_eq!(fake.in_flight.load(Ordering::SeqCst), 0);
		assert!(matches!(resolver.add(vec![line(2, 2)]).await, Err(Error::Closed)));
		assert_eq!(fake.queries(), ["host0.local"]);
	}
}
