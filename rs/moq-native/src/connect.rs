use url::Url;

/// One or more addresses for the same peer, tried in order until one connects.
///
/// Most callers have a single URL and never name this type:
/// [`Client::connect`](crate::Client::connect) takes `impl Into<Addrs>` and the
/// [`From<Url>`] impl covers it. Several addresses are for a peer that was
/// discovered rather than configured, where the record lists every interface the
/// peer answered on and nothing says which of them routes from here.
///
/// Non-empty by construction, so a connection always has somewhere to dial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addrs(
	/// Never empty: every constructor takes at least one URL.
	Vec<Url>,
);

impl Addrs {
	/// A peer reachable at one address.
	pub fn new(url: Url) -> Self {
		Self(vec![url])
	}

	/// Add a fallback, tried only when everything before it fails.
	pub fn or(mut self, url: Url) -> Self {
		self.0.push(url);
		self
	}

	/// Collect addresses in dial order, or `None` when there are none.
	///
	/// The `None` is where an empty candidate list has to be dealt with: a peer
	/// that advertised no reachable address is not something to dial and retry,
	/// it's something to skip.
	pub fn collect(urls: impl IntoIterator<Item = Url>) -> Option<Self> {
		let urls: Vec<Url> = urls.into_iter().collect();
		(!urls.is_empty()).then_some(Self(urls))
	}

	/// The addresses, in the order they are dialed. Never empty.
	pub fn as_slice(&self) -> &[Url] {
		&self.0
	}
}

impl From<Url> for Addrs {
	fn from(url: Url) -> Self {
		Self::new(url)
	}
}

/// A dial URL reduced to the part that names the peer, for logs and errors.
///
/// A dial URL's path and query are exactly where credentials live: `?jwt=` on a
/// relay dial, and a LAN mesh membership proof, which rides as a path segment
/// because raw QUIC has no headers to put it in. Formatting one into a log line
/// puts a replayable credential wherever logs are shipped, so nothing outside
/// this type formats a dial `Url` directly.
///
/// Dropping both rather than redacting known-secret spellings is deliberate: a
/// denylist only covers the credentials that exist today, and the next one to be
/// added would leak until someone remembered to extend it.
pub(crate) struct Endpoint<'a>(pub &'a Url);

impl std::fmt::Display for Endpoint<'_> {
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

/// Error returned when connection setup fails for a terminal auth reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
	/// The server rejected the credentials (HTTP 401). Retrying with the same
	/// token will fail again.
	#[error("unauthorized")]
	Unauthorized,

	/// The credentials were understood but don't grant access to this path
	/// (HTTP 403).
	#[error("forbidden")]
	Forbidden,
}

impl ConnectError {
	pub(crate) fn from_status_u16(status: u16) -> Option<Self> {
		match status {
			401 => Some(Self::Unauthorized),
			403 => Some(Self::Forbidden),
			_ => None,
		}
	}

	/// Whether this is an authentication failure, meaning a retry is pointless
	/// until the credentials change.
	pub fn is_auth(&self) -> bool {
		matches!(self, Self::Unauthorized | Self::Forbidden)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn auth_statuses_are_terminal() {
		assert_eq!(ConnectError::from_status_u16(401), Some(ConnectError::Unauthorized));
		assert_eq!(ConnectError::from_status_u16(403), Some(ConnectError::Forbidden));
	}

	#[test]
	fn non_auth_statuses_are_not_terminal() {
		for status in [400, 404, 500] {
			assert_eq!(ConnectError::from_status_u16(status), None);
		}
	}

	fn url(s: &str) -> Url {
		s.parse().expect("valid url")
	}

	/// Dial order is the order the addresses went in, and a lone URL converts
	/// implicitly so `connect(url)` keeps reading the same.
	#[test]
	fn addrs_preserve_dial_order() {
		let one = Addrs::from(url("moqt://a:4443"));
		assert_eq!(one.as_slice(), [url("moqt://a:4443")]);

		let three = Addrs::new(url("moqt://a:4443"))
			.or(url("moqt://b:4443"))
			.or(url("moqt://c:4443"));
		assert_eq!(
			three.as_slice(),
			[url("moqt://a:4443"), url("moqt://b:4443"), url("moqt://c:4443")]
		);
	}

	/// The whole point of [`Endpoint`]: a dial URL's credentials live in its path
	/// and query, so neither may survive into anything loggable.
	///
	/// The LAN mesh case is the sharp one. Its membership proof is a path segment
	/// (raw QUIC has no headers to put it in), so a dial URL logged verbatim hands
	/// a replayable credential to anyone who can read logs.
	#[test]
	fn endpoint_keeps_credentials_out_of_logs() {
		const SECRET: &str = "a4f1c93e8b7d";

		for dial in [
			// A LAN mesh dial: the proof is a path segment.
			&format!("moqt://192.168.1.5:4443/.cluster/{SECRET}"),
			// A relay dial: the token is in the query.
			&format!("https://relay.example.com/anon?jwt={SECRET}"),
			// Both at once, plus userinfo.
			&format!("https://user:{SECRET}@relay.example.com:8443/room/{SECRET}?jwt={SECRET}"),
		] {
			let rendered = Endpoint(&url(dial)).to_string();
			assert!(!rendered.contains(SECRET), "{dial} leaked through as {rendered}");
		}

		// It still names the peer, which is what a log line is for.
		assert_eq!(
			Endpoint(&url("moqt://192.168.1.5:4443/.cluster/abc")).to_string(),
			"moqt://192.168.1.5:4443"
		);
		// A default port is elided rather than invented.
		assert_eq!(
			Endpoint(&url("https://relay.example.com/anon?jwt=abc")).to_string(),
			"https://relay.example.com"
		);
		// A socket path is the address, not a credential, so it survives.
		assert_eq!(Endpoint(&url("unix:/run/moq.sock")).to_string(), "unix:///run/moq.sock");
	}

	/// A peer that advertised nothing reachable is `None` at construction, not a
	/// connection that retries an empty list forever.
	#[test]
	fn addrs_collect_rejects_an_empty_list() {
		assert_eq!(Addrs::collect([]), None);
		assert_eq!(
			Addrs::collect([url("moqt://a:4443"), url("moqt://b:4443")]),
			Some(Addrs::new(url("moqt://a:4443")).or(url("moqt://b:4443")))
		);
	}
}
