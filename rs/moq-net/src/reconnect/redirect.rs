//! Judging the URI a peer's GOAWAY names, and catching peers that bounce a connection between them.

use url::Url;

use super::Failure;

/// What to do with the URI a peer names in its GOAWAY.
///
/// The URI is dialed exactly as given, so it must carry whatever credentials the
/// new endpoint needs. Nothing from the current session is copied onto it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Redirect {
	/// Follow any URI that does not downgrade the scheme, letting the peer name the
	/// host as well as the port.
	///
	/// The host is dialed as written, so this hands the peer whatever this process
	/// can reach: a name it controls can carry an A record for loopback, a private
	/// range, or the metadata service, and nothing here resolves it to find out. A
	/// *literal* loopback, private, link-local, or IPC target is still refused, but
	/// that catches a misconfigured peer, not a hostile one. Use it only with an
	/// upstream trusted with the local network and with the tokens every session presents.
	Follow,
	/// Follow only when the host matches the dialed URL, so a peer can move us
	/// between ports or schemes but not to another host. The default.
	#[default]
	SameHost,
	/// Ignore the URI and redial the current targets.
	Ignore,
}

impl Redirect {
	/// The URL a GOAWAY assigns. `Ok(None)` keeps the current targets (the peer named no
	/// URI, or the policy ignores a URI it could parse), `Ok(Some)` replaces them, and `Err`
	/// is an explicit URI this policy refuses. A malformed URI is refused even under
	/// [`Self::Ignore`].
	///
	/// `pinned` is a certificate pin, which can only verify the host it was configured
	/// for, so it refuses a host change from an encrypted scheme even under [`Self::Follow`].
	pub(super) fn target(&self, uri: &str, current: &Url, pinned: bool) -> Result<Option<Url>, Failure> {
		if uri.is_empty() {
			return Ok(None);
		}

		// The URI can carry credentials, so the error names the reason, never the URI.
		// Parse before `Ignore`: a malformed redirect is terminal even when the policy
		// would otherwise stay on the current targets.
		let target = uri
			.parse::<Url>()
			.map_err(|_| Failure::Refused("the GOAWAY URI is malformed"))?;
		if matches!(self, Self::Ignore) {
			return Ok(None);
		}

		if scheme_tier(target.scheme()) < scheme_tier(current.scheme()) {
			return Err(Failure::Refused("the GOAWAY redirect downgrades the scheme"));
		}

		// Only as far as the URL itself says: a name is dialed, never resolved here,
		// so this refuses a peer that names a local address outright and says
		// nothing about one that hides the same address behind a hostname. That gap
		// is why [`Self::SameHost`] is the default; see [`is_local`].
		if is_local(&target) && !is_local(current) {
			return Err(Failure::Refused(
				"the GOAWAY redirect widens reachability to a local address",
			));
		}

		// Host only, not the full authority: the port is what a peer legitimately
		// moves us across when it hands off to a sibling process on the same box.
		let same_host = target.host_str() == current.host_str();
		if matches!(self, Self::SameHost) && !same_host {
			return Err(Failure::Refused("the GOAWAY redirect leaves the current host"));
		}
		// Only an encrypted dial installs the verifier a pin configures; tcp, unix, and
		// plaintext WebSocket never consult it.
		if pinned && encrypted(current.scheme()) && !same_host {
			return Err(Failure::Refused(
				"the GOAWAY redirect leaves the host a certificate pin verifies",
			));
		}

		Ok(Some(target))
	}
}

/// Whether a dial over `scheme` is encrypted, verifying the peer's certificate.
fn encrypted(scheme: &str) -> bool {
	scheme_tier(scheme) == 2
}

/// Rank a scheme so a peer-supplied redirect cannot silently drop encryption.
/// Unknown schemes rank lowest, so a forgotten classification is refused.
fn scheme_tier(scheme: &str) -> u8 {
	match scheme {
		"https" | "moqt" | "moql" | "wss" | "tls" => 2,
		"tcp" | "ws" | "http" => 1,
		// `unix` lands here deliberately: local IPC is not an upgrade over a
		// network transport, it is a different reachability class (see `is_local`).
		_ => 0,
	}
}

/// Whether a URL *says* it names something only reachable from this host or
/// network.
///
/// A judgement about the URL, not about where a dial would land. Literals and
/// `localhost` are decided here; every other name is left alone, since the
/// address behind it is chosen by whoever runs its DNS and can change between
/// this call and the dial. So a `false` here means "not local on its face", never
/// "verified remote".
fn is_local(url: &Url) -> bool {
	match url.host() {
		// A non-special scheme (`moqt://127.0.0.1`, and every other scheme but
		// `http`/`https`/`ws`/`wss`) parses its host as a domain even when it is an
		// address literal, so the literal check can't be left to the URL parser.
		Some(url::Host::Domain(host)) => match host.parse::<std::net::IpAddr>() {
			Ok(ip) => is_local_ip(ip),
			Err(_) => host == "localhost" || host.ends_with(".localhost"),
		},
		Some(url::Host::Ipv4(ip)) => is_local_ip(ip.into()),
		Some(url::Host::Ipv6(ip)) => is_local_ip(ip.into()),
		// No host at all, e.g. a `unix:` socket path.
		None => true,
	}
}

/// Whether an address literal is one only this host or network can reach.
fn is_local_ip(ip: std::net::IpAddr) -> bool {
	match ip {
		std::net::IpAddr::V4(ip) => is_local_v4(ip),
		// An IPv4-mapped address reaches the same host as the v4 it wraps, so judge
		// it by that rather than by the v6 rules.
		std::net::IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
			Some(v4) => is_local_v4(v4),
			None => ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local(),
		},
	}
}

fn is_local_v4(ip: std::net::Ipv4Addr) -> bool {
	ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

/// Where `url` dials, without its userinfo, query, or fragment, where a peer may mint fresh
/// credentials on every redirect. Two URLs that differ only there reach the same endpoint.
pub(super) fn endpoint(url: &Url) -> Url {
	let mut bare = url.clone();
	// Only fails for a URL that cannot carry userinfo, which then has none to strip.
	let _ = bare.set_username("");
	let _ = bare.set_password(None);
	bare.set_query(None);
	bare.set_fragment(None);
	bare
}

/// Formats a dial URL for logs and errors as scheme, host, and port only.
///
/// A URL can carry a credential in its userinfo, query, or path (raw QUIC has no headers,
/// so a membership proof rides as a path segment). Dropping all three rather than redacting
/// known-secret spellings is deliberate: a denylist only covers the credentials that exist
/// today.
pub(super) struct Shown<'a>(pub &'a Url);

impl std::fmt::Display for Shown<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}://", self.0.scheme())?;
		let Some(host) = self.0.host_str() else {
			// No host at all, e.g. a `unix:` socket, where the path is the address
			// rather than a credential and is the only thing that identifies it.
			return write!(f, "{}", self.0.path());
		};
		write!(f, "{host}")?;
		match self.0.port() {
			Some(port) => write!(f, ":{port}"),
			None => Ok(()),
		}
	}
}

/// A run of GOAWAY redirects, and whether it has revisited an endpoint.
///
/// The peer picks every hop, so remembering them all would grow without bound. Brent's cycle
/// detection instead keeps one departed endpoint, replaced at doubling intervals, and catches a
/// cycle of any length within a few laps of it. Once caught, the run stays a loop until reset.
pub(super) struct Redirects {
	mark: Option<Url>,
	span: u64,
	hops: u64,
	looped: bool,
}

impl Default for Redirects {
	fn default() -> Self {
		Self {
			mark: None,
			span: 1,
			hops: 0,
			looped: false,
		}
	}
}

impl Redirects {
	/// Record leaving `from` for a different endpoint `to`, and return whether this run loops.
	pub(super) fn depart(&mut self, from: Url, to: &Url) -> bool {
		if self.mark.as_ref() == Some(to) {
			self.looped = true;
		}
		self.hops += 1;
		if self.hops == self.span {
			self.mark = Some(from);
			self.span = self.span.saturating_mul(2);
			self.hops = 0;
		}
		self.looped
	}

	/// Start a new run.
	pub(super) fn reset(&mut self) {
		*self = Self::default();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn url(value: &str) -> Url {
		value.parse().expect("valid url")
	}

	fn refused(res: Result<Option<Url>, Failure>) -> bool {
		matches!(res, Err(Failure::Refused(_)))
	}

	/// A peer must not be able to redirect us somewhere we could not already
	/// reach. IPv4-mapped and link-local forms are the easy ones to miss.
	#[test]
	fn local_targets_are_recognized() {
		let local = [
			"https://127.0.0.1/",
			"https://localhost/",
			"https://[::1]/",
			"https://10.0.0.1/",
			"https://169.254.1.1/",
			"https://[::ffff:127.0.0.1]/",
			"https://[::ffff:10.0.0.1]/",
			"https://[fe80::1]/",
			"https://[fc00::1]/",
			"https://0.0.0.0/",
		];
		for url in local {
			assert!(is_local(&url.parse().unwrap()), "{url} should be local");
		}

		for url in ["https://example.com/", "https://8.8.8.8/", "https://[2606:4700::1]/"] {
			assert!(!is_local(&url.parse().unwrap()), "{url} should not be local");
		}

		// A public endpoint may not redirect us inward, but a local one may stay local.
		let public = url("https://relay.example/");
		let localhost = url("https://127.0.0.1:4443/");
		assert!(refused(
			Redirect::Follow.target("https://[::ffff:127.0.0.1]/", &public, false)
		));
		assert_eq!(
			Redirect::Follow
				.target("https://127.0.0.1:9999/", &localhost, false)
				.unwrap()
				.unwrap()
				.port(),
			Some(9999)
		);
	}

	/// The scheme ranking is what stops a peer from quietly moving us off an
	/// encrypted transport. An unknown scheme ranks lowest so a classification we
	/// forgot to add is refused rather than trusted.
	#[test]
	fn scheme_tiers_rank_encrypted_above_plaintext() {
		for scheme in ["https", "moqt", "moql", "wss", "tls"] {
			assert_eq!(scheme_tier(scheme), 2, "{scheme} is encrypted");
		}
		for scheme in ["tcp", "ws", "http"] {
			assert_eq!(scheme_tier(scheme), 1, "{scheme} is plaintext");
		}
		// `unix` is not an upgrade over a network transport, it is a different
		// reachability class, so it must not outrank one.
		for scheme in ["unix", "gopher", ""] {
			assert_eq!(scheme_tier(scheme), 0, "{scheme} is unclassified");
		}
	}

	/// A redirect may hold the scheme or improve it, never weaken it. A downgrade
	/// is a refusal, not a reason to redial the address the peer is leaving.
	#[test]
	fn target_refuses_a_scheme_downgrade() {
		let secure = url("https://relay.example/");
		let plain = url("http://relay.example/");

		assert!(refused(Redirect::Follow.target("http://other.example/", &secure, false)));
		assert!(refused(Redirect::Follow.target("unix:///tmp/moq.sock", &secure, false)));

		// Same tier and upgrades are followed.
		let same = url("https://other.example/");
		assert_eq!(
			Redirect::Follow
				.target("https://other.example/", &secure, false)
				.unwrap(),
			Some(same.clone())
		);
		assert_eq!(
			Redirect::Follow.target("https://other.example/", &plain, false).unwrap(),
			Some(same)
		);
	}

	/// Native schemes must classify address literals even when the URL parser returns a domain.
	#[test]
	fn a_non_special_scheme_classifies_its_literal_host() {
		for url in [
			"moqt://127.0.0.1/",
			"moql://10.0.0.1/",
			"tcp://169.254.169.254/",
			"moqt://[::1]/",
			"moqt://[::ffff:127.0.0.1]/",
		] {
			assert!(is_local(&url.parse().unwrap()), "{url} should be local");
		}
		for url in ["moqt://8.8.8.8/", "moql://[2606:4700::1]/", "tcp://relay.example/"] {
			assert!(!is_local(&url.parse().unwrap()), "{url} should not be local");
		}

		let public = url("moqt://relay.example/");
		assert!(
			refused(Redirect::Follow.target("moqt://169.254.169.254/", &public, false)),
			"a literal local target is refused whatever the scheme"
		);
	}

	/// The reachability guard reads the URL, so it cannot see a local address
	/// hiding behind a hostname: any name the peer controls can carry an A record
	/// for loopback or a private range, and a second lookup at dial time may answer
	/// differently anyway. The default policy is what closes that, by refusing to
	/// let a peer name a host at all.
	#[test]
	fn a_peer_named_host_is_refused_by_default() {
		let public = url("https://relay.example/");

		// The bypass: a name is not local on its face, so the reachability guard
		// passes it through whatever it resolves to.
		let rebindable = url("https://rebind.attacker.example/");
		assert!(!is_local(&rebindable), "a hostname is never classified as local");
		assert_eq!(
			Redirect::Follow.target(rebindable.as_str(), &public, false).unwrap(),
			Some(rebindable.clone()),
			"Follow is explicit trust: it dials the name the peer chose"
		);

		// So the default refuses the host change instead of trying to judge it.
		assert_eq!(Redirect::default(), Redirect::SameHost);
		assert!(
			refused(Redirect::default().target(rebindable.as_str(), &public, false)),
			"a peer-named host is refused without resolving it"
		);
		assert_eq!(
			super::super::Goaway::default().redirect,
			Redirect::SameHost,
			"and the default config carries that default"
		);
	}

	/// Only an explicit URI can assign, and one the policy will not follow is an
	/// error rather than a quiet fallback: the loop ends on it instead of redialing.
	#[test]
	fn only_an_accepted_redirect_replaces_the_targets() {
		let current = url("https://relay.example/");

		// No URI, or a policy that ignores it: keep the current targets.
		assert_eq!(Redirect::Follow.target("", &current, false).unwrap(), None);
		assert_eq!(
			Redirect::Ignore
				.target("https://relay.example:5443/", &current, false)
				.unwrap(),
			None
		);

		// An explicit URI the policy will not follow is refused.
		for (policy, uri) in [
			(Redirect::SameHost, "https://other.example/"),
			(Redirect::Follow, "not a url"),
			(Redirect::Ignore, "not a url"),
			(Redirect::Follow, "http://relay.example/"),
			(Redirect::Follow, "https://127.0.0.1/"),
		] {
			assert!(refused(policy.target(uri, &current, false)), "{policy:?}: {uri}");
		}

		// An explicit assignment remains an assignment even if its URL is unchanged.
		assert_eq!(
			Redirect::SameHost.target(current.as_str(), &current, false).unwrap(),
			Some(current.clone())
		);
		let moved = "https://relay.example:5443/";
		assert_eq!(
			Redirect::SameHost.target(moved, &current, false).unwrap(),
			Some(url(moved))
		);
	}

	/// A fingerprint is a Rustls check, installed only on encrypted dials. Schemes that
	/// never install that verifier must not inherit the pin.
	#[test]
	fn a_certificate_pin_only_covers_encrypted_schemes() {
		for scheme in ["https", "wss", "moqt", "moql", "tls"] {
			assert!(encrypted(scheme), "{scheme}");
		}
		for scheme in ["http", "ws", "tcp", "unix"] {
			assert!(!encrypted(scheme), "{scheme}");
		}
		assert_eq!(
			Redirect::Follow
				.target("tcp://other.example:1/", &url("tcp://relay.example:1/"), true)
				.unwrap(),
			Some(url("tcp://other.example:1/")),
			"a plaintext dial never consulted the pin"
		);
	}

	/// A certificate pin verifies only the host it was configured for, so it
	/// refuses a host change even when the policy would follow one.
	#[test]
	fn a_certificate_pin_holds_the_host() {
		let current = url("https://relay.example/");
		assert!(refused(Redirect::Follow.target("https://other.example/", &current, true)));
		let moved = "https://relay.example:5443/";
		assert_eq!(Redirect::Follow.target(moved, &current, true).unwrap(), Some(url(moved)));
	}

	/// `SameHost` lets a peer move us between ports or schemes on the endpoint we
	/// already chose, but not onto a different host.
	#[test]
	fn same_host_follows_a_port_not_a_host() {
		let current = url("https://relay.example:4443/");

		assert!(
			refused(Redirect::SameHost.target("https://elsewhere.example/", &current, false)),
			"another host is refused"
		);

		let moved = Redirect::SameHost
			.target("https://relay.example:5443/", &current, false)
			.unwrap()
			.unwrap();
		assert_eq!(moved.port(), Some(5443), "a different port on the same host is followed");
	}

	#[test]
	fn redirect_loop_detection_catches_any_cycle_in_constant_memory() {
		for tail in 0..4 {
			for cycle in 2..=8 {
				// `tail` one-way moves, then `cycle` endpoints redirecting around a ring.
				let at = |hop: usize| {
					if hop < tail {
						url(&format!("tcp://tail{hop}/"))
					} else {
						url(&format!("tcp://ring{}/", (hop - tail) % cycle))
					}
				};
				let mut redirects = Redirects::default();
				let mut caught = None;
				for hop in 0..4 * (tail + cycle) {
					let looped = redirects.depart(at(hop), &at(hop + 1));
					match caught {
						Some(_) => assert!(looped, "a caught loop stays caught"),
						None if looped => caught = Some(hop),
						None => {}
					}
				}
				let caught = caught.unwrap_or_else(|| panic!("tail {tail}, cycle {cycle}: never caught"));
				// Never before the run first returns to an endpoint it left.
				assert!(
					caught + 1 >= tail + cycle,
					"tail {tail}, cycle {cycle}: caught at {caught}"
				);
			}
		}

		let mut redirects = Redirects::default();
		for hop in 0..1000 {
			let from = url(&format!("tcp://a{hop}/"));
			let to = url(&format!("tcp://a{}/", hop + 1));
			assert!(!redirects.depart(from, &to), "a one-way run never loops");
		}
	}

	#[test]
	fn an_endpoint_ignores_the_credentials() {
		assert_eq!(
			endpoint(&url("tcp://token-a:secret@relay:1/path?jwt=a#x")),
			endpoint(&url("tcp://token-b@relay:1/path?jwt=b"))
		);
		// The path is part of the request target, so it names a different endpoint.
		assert_ne!(endpoint(&url("tcp://relay:1/a")), endpoint(&url("tcp://relay:1/b")));
	}

	#[test]
	fn a_shown_url_drops_every_credential() {
		let shown = Shown(&url("moqt://user:pass@relay:4443/.cluster/secret?jwt=token")).to_string();
		assert_eq!(shown, "moqt://relay:4443");
		assert_eq!(Shown(&url("unix:///run/moq.sock")).to_string(), "unix:///run/moq.sock");
	}
}

