use url::Url;

/// A cluster peer's identity: one relay has one key however it is spelled, and the
/// key never carries a credential, so it is safe to log and to show on `/nodes`.
///
/// Built only by [`PeerKey::canonicalize`], so no code path can key on a raw URL.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PeerKey(String);

impl PeerKey {
	/// The key for `peer`, a URL, a bare `host:port`, or a LAN discovery id.
	///
	/// Drops the query (where `?jwt=` and `?cost=` live) and everything
	/// [`normalize`] drops. It has to absorb every normalization a discovery path
	/// applies on the way in (mDNS strips the same parts before advertising), or
	/// one relay gets two keys and is dialed twice. A peer that does not parse as
	/// a URL keys on its raw string, minus any query, fragment, or userinfo.
	pub(crate) fn canonicalize(peer: &str) -> Self {
		let Ok(mut url) = super::peer_url(peer) else {
			return Self(redact(peer));
		};
		normalize(&mut url);
		url.set_query(None);
		Self(url.into())
	}
}

/// Drop the parts of a peer URL that name no different peer and that no peer acts
/// on: the fragment, userinfo, DNS host case, and repeated and trailing path
/// slashes (`moq_auth::path::normalize` reads the request path the same way).
/// Applied to the dial URL as well as the key, so two spellings that share a key
/// also dial alike, and a peer list naming both is no conflict.
pub(crate) fn normalize(url: &mut Url) {
	url.set_fragment(None);
	url.set_username("").ok();
	url.set_password(None).ok();
	// `Url` lowercases only a special scheme's host, so `moqt://HOST` keeps its case.
	if let Some(url::Host::Domain(host)) = url.host()
		&& host.bytes().any(|b| b.is_ascii_uppercase())
	{
		let host = host.to_ascii_lowercase();
		url.set_host(Some(&host)).ok();
	}
	// A non-special scheme like `moqt` keeps its trailing slash otherwise, and
	// `moqt://a.example:4443/` is an ordinary spelling of `moqt://a.example:4443`.
	let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
	let path = match segments.is_empty() {
		true => String::new(),
		false => format!("/{}", segments.join("/")),
	};
	url.set_path(&path);
}

/// `peer` without a query, fragment, or userinfo, for a peer that is not a URL.
fn redact(peer: &str) -> String {
	let peer = &peer[..peer.find(['?', '#']).unwrap_or(peer.len())];
	let (scheme, rest) = match peer.split_once("://") {
		Some((scheme, rest)) => (Some(scheme), rest),
		None => (None, peer),
	};
	let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
	let rest = match authority.rfind('@') {
		Some(at) => &rest[at + 1..],
		None => rest,
	};
	match scheme {
		Some(scheme) => format!("{scheme}://{rest}"),
		None => rest.to_string(),
	}
}

impl std::fmt::Display for PeerKey {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every spelling of one relay shares a key, and the key keeps no credential.
	#[test]
	fn spellings_share_a_key_without_credentials() {
		let key = PeerKey::canonicalize("host:4443");
		assert_eq!(key.to_string(), "https://host:4443/");
		for spelling in [
			"https://host:4443/",
			"https://HOST:4443",
			"https://host:4443/?jwt=abc&cost=2",
			"https://user:secret@host:4443/",
			"https://host:4443/#frag",
			"https://host:4443//",
		] {
			assert_eq!(PeerKey::canonicalize(spelling), key, "{spelling}");
		}
		assert_eq!(
			PeerKey::canonicalize("moqt://a.example:4443/"),
			PeerKey::canonicalize("moqt://a.example:4443")
		);
		assert_eq!(
			PeerKey::canonicalize("https://user:secret@peer.example//relay/").to_string(),
			"https://peer.example/relay"
		);
		assert_ne!(key, PeerKey::canonicalize("host:5555"));
		assert_eq!(
			PeerKey::canonicalize("moqt://HOST.example:4443"),
			PeerKey::canonicalize("moqt://host.example:4443/")
		);
	}

	/// A peer that is not a URL keys without its credential.
	#[test]
	fn a_malformed_peer_keys_without_credentials() {
		assert_eq!(
			PeerKey::canonicalize("https://z:bad/?jwt=secret").to_string(),
			"https://z:bad/"
		);
		assert_eq!(
			PeerKey::canonicalize("https://user:pw@z:bad/x#frag").to_string(),
			"https://z:bad/x"
		);
		assert_eq!(PeerKey::canonicalize("not a url?jwt=secret").to_string(), "not a url");
	}
}
