/// A cluster peer's identity: one relay has one key however it is spelled, and the
/// key never carries a credential, so it is safe to log and to show on `/nodes`.
///
/// Built only by [`PeerKey::canonicalize`], so no code path can key on a raw URL.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PeerKey(String);

impl PeerKey {
	/// The key for `peer`, a URL, a bare `host:port`, or a LAN discovery id.
	///
	/// Drops the query (where `?jwt=` and `?cost=` live), fragment, and userinfo,
	/// collapses repeated and trailing path slashes, and lets `Url` normalize the
	/// scheme, host case, and default port. It has to absorb every normalization a
	/// discovery path applies on the way in (mDNS strips the same parts before
	/// advertising), or one relay gets two keys and is dialed twice. A peer that
	/// does not parse as a URL keys on its raw string.
	pub(crate) fn canonicalize(peer: &str) -> Self {
		let Ok(mut url) = super::peer_url(peer) else {
			return Self(peer.to_string());
		};
		url.set_query(None);
		url.set_fragment(None);
		url.set_username("").ok();
		url.set_password(None).ok();
		// A non-special scheme like `moqt` keeps its trailing slash otherwise, and
		// `moqt://a.example:4443/` is an ordinary spelling of `moqt://a.example:4443`.
		let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
		let path = match segments.is_empty() {
			true => String::new(),
			false => format!("/{}", segments.join("/")),
		};
		url.set_path(&path);
		Self(url.into())
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
	}
}
