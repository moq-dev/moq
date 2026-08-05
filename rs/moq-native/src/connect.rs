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
