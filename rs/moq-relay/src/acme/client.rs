//! Automatic certificates from an ACME directory, Let's Encrypt by default.
//!
//! [rustls-acme](rustls_acme) owns the account, order, and renewal loop and
//! answers TLS-ALPN-01 challenges from its certificate resolver. The relay hands
//! that resolver to every TLS listener, so QUIC, HTTPS, and every worker serve
//! the same certificate and pick up a renewal on their next handshake.

use moq_clock::Wall;
use std::{
	io,
	net::SocketAddr,
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use anyhow::Context as _;
use futures::StreamExt;
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls_acme::{AcmeState, EventOk, OrderError};
use url::Url;

/// How long the first issuance may take by default before startup fails.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a validation connection may take to finish its handshake.
const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long each systemd start-timeout extension lasts while issuance is pending.
#[cfg(unix)]
const EXTEND_USEC: u32 = 90_000_000;

/// How often the start timeout is extended, well inside [`EXTEND_USEC`].
const EXTEND_INTERVAL: Duration = Duration::from_secs(30);

/// Challenge handshakes in flight at once; connections past this are closed unanswered,
/// so a flood cannot exhaust descriptors. Far above the handful of vantage points a
/// directory validates from, so idle scanners do not crowd the validator out.
const CHALLENGE_CONCURRENCY: usize = 1024;

/// Failed validations tolerated within [`FAILURE_WINDOW`] before issuance refuses to start
/// and renewal pauses, leaving headroom under Let's Encrypt's limit of 5 per hostname per hour.
const FAILURE_LIMIT: usize = 3;

/// How long a failed validation counts against [`FAILURE_LIMIT`], matching Let's Encrypt's window.
const FAILURE_WINDOW: Duration = Duration::from_secs(60 * 60);

use super::Config;

impl Config {
	/// Validate against the listeners it must serve through and start the client, or `None` when unconfigured.
	pub(crate) fn init(
		&self,
		listen: &moq_tokio::tls::Listen,
		https: &crate::web::Https,
	) -> anyhow::Result<Option<Acme>> {
		if self.domains.is_empty() {
			anyhow::ensure!(
				self.contact.is_empty() && self.cache.is_none() && self.directory.is_none() && self.timeout().is_none(),
				"acme settings require acme.domains"
			);
			return Ok(None);
		}
		if cfg!(not(unix)) {
			anyhow::bail!("acme is only supported on unix, where acme.cache can be kept owner-only");
		}
		// One canonical set, so reordering or recasing the same names reuses the cached certificate.
		let mut domains: Vec<String> = self.domains.iter().map(|domain| domain.to_ascii_lowercase()).collect();
		domains.sort();
		domains.dedup();
		for domain in &domains {
			anyhow::ensure!(
				!domain.contains('*'),
				"acme.domains cannot include the wildcard {domain}: TLS-ALPN-01 cannot issue one"
			);
			anyhow::ensure!(
				matches!(ServerName::try_from(domain.as_str()), Ok(ServerName::DnsName(_))),
				"acme.domains entry {domain:?} is not a DNS name"
			);
		}
		anyhow::ensure!(
			listen.cert.is_empty() && listen.key.is_empty() && listen.generate.is_empty(),
			"acme cannot be combined with listen.tls.cert, listen.tls.key, or listen.tls.generate"
		);
		anyhow::ensure!(
			https.listen.is_some(),
			"acme requires web.https.listen: the directory validates each domain over TLS-ALPN-01 on port 443"
		);
		anyhow::ensure!(
			https.cert.is_empty() && https.key.is_empty(),
			"acme cannot be combined with web.https.cert or web.https.key"
		);
		let dir = self
			.cache
			.clone()
			.context("acme requires acme.cache to keep the account and certificate across restarts")?;
		create_private_dir(&dir).with_context(|| format!("failed to prepare acme.cache {}", dir.display()))?;

		let directory = self
			.directory
			.as_ref()
			.map_or(rustls_acme::acme::LETS_ENCRYPT_PRODUCTION_DIRECTORY, Url::as_str);
		let cache = Cache { dir };
		let failures = Failures {
			path: cache.path("failures", &domains, directory),
		};
		let cert = cache.path("cert", &domains, directory);
		let state = rustls_acme::AcmeConfig::new(&domains)
			.contact(self.contact.iter().map(Url::as_str))
			.directory(directory)
			.cache(cache)
			.state();

		Ok(Some(Acme {
			state,
			failures,
			cert,
			domains: domains.join(", "),
			timeout: self.timeout().unwrap_or(DEFAULT_TIMEOUT),
		}))
	}
}

/// The ACME client: the certificate resolver plus the order and renewal loop behind it.
pub(crate) struct Acme {
	state: AcmeState<io::Error>,
	failures: Failures,
	/// Where rustls-acme looks for the cached certificate.
	cert: PathBuf,
	domains: String,
	timeout: Duration,
}

impl Acme {
	/// The resolver every TLS listener serves from, which also answers the challenges.
	pub(crate) fn resolver(&self) -> Arc<dyn rustls::server::ResolvesServerCert> {
		self.state.resolver()
	}

	/// Serve only the TLS-ALPN-01 challenge on `listener` until a certificate is deployed.
	///
	/// A valid cached certificate deploys at once with no challenge. Otherwise this
	/// orders one, failing with the last ACME error once the startup budget runs out
	/// or once [`FAILURE_LIMIT`] validations failed within the hour, counting earlier
	/// starts and renewals: a supervisor restarting the relay would otherwise spend
	/// the rest of the directory's allowance and lock the hostname out for an hour.
	pub(crate) async fn issue(&mut self, listener: &std::net::TcpListener) -> anyhow::Result<()> {
		let addr = listener.local_addr()?;
		let mut failed = self.failures.recent()?;
		// A spent budget refuses only an order; the certificate it was renewing still serves.
		let spent = failed.len() >= FAILURE_LIMIT;
		if spent && !Cache::read(&self.cert)?.is_some_and(|pem| current(&pem)) {
			return Err(self.failures.refuse(&failed, &self.domains, addr, None));
		}

		// A second descriptor for the same socket, so the web server keeps the
		// original and takes over the queue once this returns.
		let listener = tokio::net::TcpListener::from_std(listener.try_clone()?)?;
		let challenge = tokio_rustls::TlsAcceptor::from(self.state.challenge_rustls_config());
		let mut handshakes = tokio::task::JoinSet::new();
		let health = moq_tokio::accept::Health::new("acme");
		// Readiness waits on issuance, so keep extending systemd's start timeout
		// rather than letting the unit's default kill the relay inside the budget.
		let mut extend = moq_clock::interval(EXTEND_INTERVAL);
		let deadline = moq_clock::sleep(self.timeout);
		tokio::pin!(deadline);
		let mut last = None;
		let mut issued = false;

		loop {
			tokio::select! {
				event = self.state.next() => match event.context("ACME client stopped")? {
					// Validation never ran, so failed renewals still count against the budget.
					Ok(EventOk::DeployedCachedCert) => {
						tracing::info!(domains = %self.domains, "serving the cached ACME certificate");
						return Ok(());
					}
					// rustls-acme could not deploy the cached certificate, so its next poll orders one.
					event if spent => {
						let last = event.err().map(|err| err.to_string());
						return Err(self.failures.refuse(&failed, &self.domains, addr, last.as_deref()));
					}
					// Not ready until it is saved: a certificate lost on restart would be ordered again.
					Ok(EventOk::DeployedNewCert) => {
						tracing::info!(domains = %self.domains, "issued an ACME certificate");
						issued = true;
					}
					Ok(EventOk::CertCacheStore) if issued => return self.failures.clear(),
					Ok(event) => tracing::debug!(?event, "ACME"),
					Err(rustls_acme::EventError::CertCacheStore(err)) => {
						return Err(anyhow::Error::new(err).context("failed to save the issued ACME certificate in acme.cache"));
					}
					// The directory failed a validation, or may still fail one rustls-acme stopped waiting on.
					Err(err @ rustls_acme::EventError::Order(OrderError::BadAuth(_) | OrderError::TooManyAttemptsAuth(_))) => {
						failed = self.failures.record()?;
						tracing::warn!(%err, domains = %self.domains, failed = failed.len(), "ACME validation failed");
						if failed.len() >= FAILURE_LIMIT {
							return Err(self.failures.refuse(&failed, &self.domains, addr, Some(&err.to_string())));
						}
						last = Some(err.to_string());
					}
					Err(err) => {
						tracing::warn!(%err, domains = %self.domains, "ACME issuance failed; retrying");
						last = Some(err.to_string());
					}
				},
				accepted = listener.accept() => match accepted {
					Ok((stream, _)) => {
						health.accepted();
						if handshakes.len() < CHALLENGE_CONCURRENCY {
							let handshake = challenge.accept(stream);
							// The validator only needs the handshake, then closes.
							handshakes.spawn(async move {
								let _ = moq_clock::timeout(CHALLENGE_TIMEOUT, handshake).await;
							});
						}
					}
					Err(err) => {
						if let Some(pause) = health.failed(&err) {
							moq_clock::sleep(pause).await;
						}
					}
				},
				Some(_) = handshakes.join_next() => {}
				_ = extend.tick() => {
					#[cfg(unix)]
					let _ = sd_notify::notify(&[sd_notify::NotifyState::ExtendTimeoutUsec(EXTEND_USEC)]);
				}
				_ = &mut deadline => {
					let last = last.as_deref().unwrap_or("no response from the directory");
					anyhow::bail!(
						"no ACME certificate for {} within {}: {last}; the directory validates each domain over TLS-ALPN-01 on port 443, and web.https.listen is {addr}",
						self.domains,
						humantime::format_duration(self.timeout),
					);
				}
			}
		}
	}

	/// Renew in the background forever, keeping the current certificate serving through failures.
	///
	/// Failed validations count against the same budget as [`Self::issue`], since
	/// rustls-acme retries a failed order within seconds. Once [`FAILURE_LIMIT`] fall
	/// within [`FAILURE_WINDOW`], renewal stops polling the ACME state, which pauses
	/// rustls-acme's order loop, until the oldest one ages out.
	///
	/// Never returns: the event stream is infinite, so its end panics, which stops
	/// the relay through its critical tasks instead of silently ending renewal.
	pub(crate) async fn run(self) {
		renew(self.state, self.failures, self.domains).await
	}
}

/// The renewal loop over `events`, split from [`Acme::run`] so tests can drive it with fake events.
async fn renew(
	mut events: impl futures::Stream<Item = rustls_acme::Event<io::Error, io::Error>> + Unpin,
	failures: Failures,
	domains: String,
) {
	loop {
		let resume = match failures.recent() {
			Ok(failed) if failed.len() < FAILURE_LIMIT => None,
			Ok(failed) => failed.first().map(|oldest| oldest.saturating_add(FAILURE_WINDOW)),
			// An unknown budget counts as spent, rather than risk the directory's limit.
			Err(err) => {
				tracing::error!(err = format!("{err:#}"), %domains, "failed to read the ACME failure record");
				Some(moq_clock::wall().saturating_add(FAILURE_WINDOW))
			}
		};
		if let Some(resume) = resume {
			tracing::warn!(
				%domains,
				resume = %humantime::format_rfc3339_seconds(resume.into()),
				"ACME renewal paused after {FAILURE_LIMIT} failed validations within the hour, short of the directory's limit; still serving the previous certificate"
			);
			moq_clock::sleep(resume.saturating_duration_since(moq_clock::wall())).await;
			continue;
		}

		match events.next().await.expect("the ACME event stream ended") {
			Ok(EventOk::DeployedNewCert) => {
				tracing::info!(%domains, "renewed the ACME certificate");
				if let Err(err) = failures.clear() {
					tracing::error!(%err, %domains, "failed to clear the ACME failure record");
				}
			}
			Ok(event) => tracing::debug!(?event, "ACME"),
			// Counted as in issue.
			Err(err @ rustls_acme::EventError::Order(OrderError::BadAuth(_) | OrderError::TooManyAttemptsAuth(_))) => {
				match failures.record() {
					Ok(failed) => tracing::warn!(
						%err,
						%domains,
						failed = failed.len(),
						"ACME renewal validation failed; still serving the previous certificate"
					),
					// Unrecorded, it would never count, so assume the budget is spent.
					Err(record) => {
						tracing::error!(%err, record = format!("{record:#}"), %domains, "ACME renewal validation failed and could not be recorded; pausing renewal for an hour");
						moq_clock::sleep(FAILURE_WINDOW).await;
					}
				}
			}
			Err(err) => tracing::error!(
				%err,
				%domains,
				"ACME renewal failed; still serving the previous certificate"
			),
		}
	}
}

/// Create `dir` if needed and restrict it to the owner, since it holds private keys.
fn create_private_dir(dir: &Path) -> io::Result<()> {
	std::fs::create_dir_all(dir)?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
	}
	Ok(())
}

/// The failed validations recorded in `acme.cache`, so restarts share one allowance.
///
/// One Unix timestamp per line, keyed like the certificate it was trying to issue.
struct Failures {
	path: PathBuf,
}

impl Failures {
	/// The failures within [`FAILURE_WINDOW`], oldest first.
	fn recent(&self) -> anyhow::Result<Vec<Wall>> {
		let Some(contents) = Cache::read(&self.path)? else {
			return Ok(Vec::new());
		};
		let cutoff = moq_clock::wall()
			.checked_sub(FAILURE_WINDOW)
			.unwrap_or(Wall::UNIX_EPOCH);
		let mut recent = Vec::new();
		for line in String::from_utf8_lossy(&contents).lines() {
			let secs: u64 = line
				.parse()
				.with_context(|| format!("malformed ACME failure record in {}", self.path.display()))?;
			let at = Wall::UNIX_EPOCH
				.checked_add(Duration::from_secs(secs))
				.context("failure timestamp out of range")?;
			if at > cutoff {
				recent.push(at);
			}
		}
		recent.sort();
		Ok(recent)
	}

	/// Record a failure now, returning the recent ones including it.
	fn record(&self) -> anyhow::Result<Vec<Wall>> {
		let mut recent = self.recent()?;
		recent.push(moq_clock::wall());
		let lines: String = recent
			.iter()
			.map(|at| format!("{}\n", at.saturating_duration_since(Wall::UNIX_EPOCH).as_secs()))
			.collect();
		Cache::write(&self.path, lines.as_bytes())?;
		Ok(recent)
	}

	fn clear(&self) -> anyhow::Result<()> {
		match std::fs::remove_file(&self.path) {
			Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.into()),
			_ => Ok(()),
		}
	}

	/// The startup error once `failed` reaches [`FAILURE_LIMIT`].
	fn refuse(&self, failed: &[Wall], domains: &str, addr: SocketAddr, last: Option<&str>) -> anyhow::Error {
		let retry = failed
			.first()
			.map_or(Some(moq_clock::wall()), |oldest| oldest.checked_add(FAILURE_WINDOW))
			.map(|retry| humantime::format_rfc3339_seconds(retry.into()).to_string())
			.unwrap_or_else(|| "the supported wall-time range".to_string());
		let last = last.map(|last| format!("; last error: {last}")).unwrap_or_default();
		anyhow::anyhow!(
			"ACME validation for {domains} failed {} times in the last hour; stopping before the directory's failed-validation limit locks the hostname out (Let's Encrypt allows 5 failed validations per hostname per hour). Check that each domain resolves to this relay and that TCP port 443 reaches web.https.listen ({addr}), then restart after {} or delete {}{last}",
			failed.len(),
			retry,
			self.path.display(),
		)
	}
}

/// Keeps the account key and certificate in `dir`, one owner-only file each.
///
/// Files are keyed by a hash of the contacts or domains plus the directory URL, so
/// a certificate missing a configured domain or issued by another directory is
/// simply not found and gets reissued. An expired one counts as missing too.
struct Cache {
	dir: PathBuf,
}

impl Cache {
	fn path(&self, kind: &str, names: &[String], directory: &str) -> PathBuf {
		let mut digest = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
		for name in names {
			digest.update(name.as_bytes());
			digest.update(&[0]);
		}
		digest.update(directory.as_bytes());
		self.dir.join(format!("{kind}-{}", hex::encode(digest.finish())))
	}

	fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
		match std::fs::read(path) {
			Ok(contents) => Ok(Some(contents)),
			Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
			Err(err) => Err(err),
		}
	}

	/// Replace `path` atomically, so a crash mid-write never leaves a torn key behind.
	fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
		use std::io::Write;

		let partial = path.with_extension("partial");
		let mut options = std::fs::OpenOptions::new();
		options.write(true).create(true).truncate(true);
		#[cfg(unix)]
		std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
		let mut file = options.open(&partial)?;
		file.write_all(contents)?;
		file.sync_all()?;
		std::fs::rename(partial, path)
	}
}

/// Whether the leaf in a cached key-and-chain PEM is valid right now.
fn current(pem: &[u8]) -> bool {
	let Some(Ok(leaf)) = CertificateDer::pem_slice_iter(pem).next() else {
		return false;
	};
	x509_parser::parse_x509_certificate(&leaf).is_ok_and(|(_, cert)| cert.validity().is_valid())
}

#[async_trait::async_trait]
impl rustls_acme::CertCache for Cache {
	type EC = io::Error;

	async fn load_cert(&self, domains: &[String], directory: &str) -> io::Result<Option<Vec<u8>>> {
		let cached = Self::read(&self.path("cert", domains, directory))?;
		Ok(cached.filter(|pem| {
			let current = current(pem);
			if !current {
				tracing::info!("the cached ACME certificate is expired or unreadable; reissuing");
			}
			current
		}))
	}

	async fn store_cert(&self, domains: &[String], directory: &str, cert: &[u8]) -> io::Result<()> {
		Self::write(&self.path("cert", domains, directory), cert)
	}
}

#[async_trait::async_trait]
impl rustls_acme::AccountCache for Cache {
	type EA = io::Error;

	async fn load_account(&self, contact: &[String], directory: &str) -> io::Result<Option<Vec<u8>>> {
		Self::read(&self.path("account", contact, directory))
	}

	async fn store_account(&self, contact: &[String], directory: &str, account: &[u8]) -> io::Result<()> {
		Self::write(&self.path("account", contact, directory), account)
	}
}

#[cfg(all(test, unix))]
#[path = "tests.rs"]
mod tests;
