//! The credentials a client presents, kept out of logs and replaceable while it runs.

use std::task::Poll;

use moq_net::kio;
use url::Url;

/// A credential to present when dialing, such as a JWT.
///
/// Its `Debug` shows only the length, so a dumped config never leaks it.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl Token {
	/// The credential itself.
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl std::fmt::Debug for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Token(<{} bytes>)", self.0.len())
	}
}

impl std::str::FromStr for Token {
	type Err = EmptyToken;

	fn from_str(token: &str) -> Result<Self, Self::Err> {
		Self::try_from(token.to_string())
	}
}

impl TryFrom<String> for Token {
	type Error = EmptyToken;

	fn try_from(token: String) -> Result<Self, Self::Error> {
		match token.is_empty() {
			true => Err(EmptyToken),
			false => Ok(Self(token)),
		}
	}
}

impl From<Token> for String {
	fn from(token: Token) -> Self {
		token.0
	}
}

/// An empty token, which presents no credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a token cannot be empty")]
#[non_exhaustive]
pub struct EmptyToken;

/// The tokens a [`Connection`](crate::Connection) presents, which every dial reads afresh.
///
/// Clones share one set. Replacing it reaches the next dial and, on a live session with
/// AUTH, presents the new tokens at once, withdrawing the ones they replace only after
/// every new one has its answer. [`answered`](Self::answered) reports that answer.
#[derive(Clone)]
pub struct Tokens(kio::Producer<Set>);

/// One version of the tokens, numbered so a dial can tell whether they changed since.
#[derive(Clone, Default)]
pub(crate) struct Set {
	pub tokens: Vec<Token>,
	pub generation: u64,
	/// The peer's answer to this generation, once it has one.
	answer: Option<crate::Result<()>>,
	/// Why no session will answer again, once the connection ended.
	ended: Option<crate::Error>,
}

impl Tokens {
	#[cfg(any(feature = "_transport", test))]
	pub(crate) fn new(tokens: Vec<Token>) -> Self {
		Self(kio::Producer::new(Set {
			tokens,
			..Default::default()
		}))
	}

	/// Replace every token. The first is the connection credential, unless the dial URL
	/// carries its own `?jwt=`.
	pub fn set(&self, tokens: Vec<Token>) {
		// Never closed: this handle is a producer.
		let Ok(mut set) = self.0.write() else { return };
		if set.tokens != tokens {
			set.tokens = tokens;
			set.generation += 1;
			set.answer = None;
		}
	}

	/// Poll for the peer's answer to the tokens set last; see [`answered`](Self::answered).
	pub fn poll_answered(&self, waiter: &kio::Waiter) -> Poll<crate::Result<()>> {
		let answer = |set: &Set| set.answer.clone().or_else(|| set.ended.clone().map(Err));
		match self.0.poll_ref(waiter, |set| match answer(set) {
			Some(answer) => Poll::Ready(answer),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(answer)) => Poll::Ready(answer),
			// Never closed: this handle is a producer.
			Poll::Ready(Err(set)) => Poll::Ready(answer(&set).unwrap_or(Err(moq_net::Error::Closed.into()))),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Wait for the peer's answer to the tokens set last: `Ok` once a live session holds
	/// every one, in band or as the connection credential it dialed with.
	///
	/// While offline it waits for the next session. A later [`set`](Self::set) moves the
	/// wait on to its tokens.
	///
	/// # Errors
	///
	/// The peer's refusal of a token, [`moq_net::Error::Unsupported`] when the live session
	/// cannot carry an in-band token, or the connection's end, such as a refused dial.
	pub async fn answered(&self) -> crate::Result<()> {
		kio::wait(|waiter| self.poll_answered(waiter)).await
	}

	pub(crate) fn snapshot(&self) -> Set {
		self.0.read().clone()
	}

	pub(crate) fn consume(&self) -> kio::Consumer<Set> {
		self.0.consume()
	}

	/// Record the peer's answer to `generation`, unless the tokens changed since.
	pub(crate) fn answer(&self, generation: u64, answer: crate::Result<()>) {
		let Ok(mut set) = self.0.write() else { return };
		if set.generation == generation && set.answer.is_none() {
			set.answer = Some(answer);
		}
	}

	/// No session will answer again, because the connection ended with `err`.
	pub(crate) fn end(&self, err: crate::Error) {
		let Ok(mut set) = self.0.write() else { return };
		if set.ended.is_none() {
			set.ended = Some(err);
		}
	}
}

/// What one dial presents, decided from its URL, the offered versions, and the tokens.
#[derive(Clone)]
pub(crate) struct Credential {
	/// The URL to dial, carrying the connection credential in its query when an offered
	/// version has no SETUP token.
	#[cfg_attr(
		not(any(feature = "_transport", test)),
		allow(dead_code, reason = "only a compiled transport dials the URL")
	)]
	pub url: Url,
	/// The token SETUP carries, unless the URL brought its own credential.
	#[cfg_attr(
		not(any(feature = "_transport", test)),
		allow(dead_code, reason = "only a compiled transport sends SETUP")
	)]
	pub setup: Option<Token>,
	/// The credential the connection presents, from the URL or SETUP. Never also
	/// presented on an AUTH stream.
	connection: Option<String>,
}

impl Credential {
	pub fn new(url: &Url, versions: &moq_net::Versions, set: &Set) -> Self {
		if let Some(jwt) = jwt(url) {
			// The URL's own credential (a cluster's per-peer `?jwt=`) wins, so the tokens
			// all ride AUTH and no second credential reaches SETUP or the query.
			return Self {
				url: url.clone(),
				setup: None,
				connection: Some(jwt),
			};
		}
		let Some(first) = set.tokens.first() else {
			return Self {
				url: url.clone(),
				setup: None,
				connection: None,
			};
		};
		// The URL is fixed before the version is negotiated, so it carries the credential
		// whenever a version that may be picked could not carry it in SETUP.
		let url = match versions.iter().all(|version| version.has_setup_token()) {
			true => url.clone(),
			false => with_jwt(url, first.as_str()),
		};
		Self {
			url,
			setup: Some(first.clone()),
			connection: Some(first.as_str().to_string()),
		}
	}

	/// Whether `tokens` still include the connection credential, which then answers for itself.
	pub fn among(&self, tokens: &[Token]) -> bool {
		self.connection
			.as_deref()
			.is_some_and(|connection| tokens.iter().any(|token| token.as_str() == connection))
	}

	/// The tokens to hold on their own AUTH streams, each once.
	pub fn extra(&self, tokens: &[Token]) -> Vec<Token> {
		let mut extra: Vec<Token> = Vec::new();
		for token in tokens {
			if self.connection.as_deref() != Some(token.as_str()) && !extra.contains(token) {
				extra.push(token.clone());
			}
		}
		extra
	}
}

/// The request target's query: the URL's own, or for `unix:` the one inside `?path=`.
fn target_query(url: &Url) -> Option<String> {
	match url.scheme() {
		"unix" => url
			.query_pairs()
			.find(|(k, _)| k == "path")
			.and_then(|(_, path)| path.split_once('?').map(|(_, query)| query.to_string())),
		_ => url.query().map(str::to_string),
	}
}

/// The non-empty `jwt` the URL's request target already carries.
fn jwt(url: &Url) -> Option<String> {
	let query = target_query(url)?;
	url::form_urlencoded::parse(query.as_bytes())
		.find(|(k, v)| k == "jwt" && !v.is_empty())
		.map(|(_, v)| v.into_owned())
}

/// `url` with `jwt` appended to its request target's query.
fn with_jwt(url: &Url, jwt: &str) -> Url {
	let mut url = url.clone();
	if url.scheme() != "unix" {
		url.query_pairs_mut().append_pair("jwt", jwt);
		return url;
	}
	// A unix socket's request target is one form-encoded `?path=` value, so the
	// credential joins the query inside it.
	let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
	let mut serializer = url::form_urlencoded::Serializer::new(String::new());
	let mut found = false;
	for (k, v) in &pairs {
		match k == "path" {
			true => {
				found = true;
				let mut target = url::form_urlencoded::Serializer::new(String::new());
				target.append_pair("jwt", jwt);
				let separator = if v.contains('?') { '&' } else { '?' };
				serializer.append_pair(k, &format!("{v}{separator}{}", target.finish()));
			}
			false => {
				serializer.append_pair(k, v);
			}
		}
	}
	if !found {
		let mut target = url::form_urlencoded::Serializer::new(String::new());
		target.append_pair("jwt", jwt);
		serializer.append_pair("path", &format!("?{}", target.finish()));
	}
	url.set_query(Some(&serializer.finish()));
	url
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tokens(tokens: &[&str]) -> Set {
		Set {
			tokens: tokens.iter().map(|token| token.parse().unwrap()).collect(),
			generation: 3,
			..Default::default()
		}
	}

	fn versions(names: &[&str]) -> moq_net::Versions {
		names
			.iter()
			.map(|name| name.parse::<moq_net::Version>().unwrap())
			.collect::<Vec<_>>()
			.into()
	}

	/// Only lite-07 and moq-transport carry the credential in SETUP, so the URL stays clean.
	#[test]
	fn setup_versions_keep_the_credential_out_of_the_url() {
		let url: Url = "https://relay.example.com/room".parse().unwrap();
		let credential = Credential::new(
			&url,
			&versions(&["moq-lite-07-wip", "moq-transport-17"]),
			&tokens(&["a", "b"]),
		);
		assert_eq!(credential.url, url);
		assert_eq!(credential.setup.as_ref().map(Token::as_str), Some("a"));
		assert_eq!(credential.extra(&tokens(&["a", "b"]).tokens), tokens(&["b"]).tokens);
	}

	/// An offered version without a SETUP token needs the credential in the query, and the
	/// same value rides SETUP too.
	#[test]
	fn an_older_version_puts_the_credential_in_the_url() {
		let url: Url = "https://relay.example.com/room?x=1".parse().unwrap();
		let credential = Credential::new(&url, &moq_net::Versions::all(), &tokens(&["a b"]));
		assert_eq!(credential.url.as_str(), "https://relay.example.com/room?x=1&jwt=a+b");
		assert_eq!(credential.setup.as_ref().map(Token::as_str), Some("a b"));
		assert!(credential.extra(&tokens(&["a b"]).tokens).is_empty());
	}

	/// A URL credential stays the connection credential, so every token rides AUTH,
	/// except one equal to it, which would be granted twice.
	#[test]
	fn a_url_credential_sends_every_token_on_auth() {
		let url: Url = "moqt://relay.example.com/room?jwt=peer".parse().unwrap();
		let credential = Credential::new(&url, &moq_net::Versions::all(), &tokens(&["a", "peer", "a"]));
		assert_eq!(credential.url, url);
		assert_eq!(credential.setup, None);
		assert_eq!(
			credential.extra(&tokens(&["a", "peer", "a"]).tokens),
			tokens(&["a"]).tokens
		);
	}

	#[test]
	fn no_tokens_present_nothing() {
		let url: Url = "https://relay.example.com/room".parse().unwrap();
		let credential = Credential::new(&url, &moq_net::Versions::all(), &Set::default());
		assert_eq!(credential.url, url);
		assert_eq!(credential.setup, None);
	}

	/// A unix socket's request target lives in `?path=`, so the credential goes inside it.
	#[test]
	fn a_unix_credential_joins_the_path_target() {
		for (url, expected) in [
			("unix:///tmp/moq.sock?path=%2Froom", "/room?jwt=a"),
			("unix:///tmp/moq.sock?path=%2Froom%3Fx%3D1", "/room?x=1&jwt=a"),
			("unix:///tmp/moq.sock", "?jwt=a"),
		] {
			let url: Url = url.parse().unwrap();
			let credential = Credential::new(&url, &moq_net::Versions::all(), &tokens(&["a"]));
			let path = credential
				.url
				.query_pairs()
				.find(|(k, _)| k == "path")
				.map(|(_, v)| v.into_owned());
			assert_eq!(path.as_deref(), Some(expected), "{url}");
			// The rewritten target is read back as already carrying the credential.
			assert_eq!(jwt(&credential.url).as_deref(), Some("a"));
		}
	}

	#[test]
	fn a_token_never_logs_its_value() {
		let token: Token = "secret".parse().unwrap();
		assert_eq!(format!("{token:?}"), "Token(<6 bytes>)");
		assert!("".parse::<Token>().is_err());
	}

	#[test]
	fn set_bumps_the_generation_only_on_change() {
		let tokens = Tokens::new(vec!["a".parse().unwrap()]);
		tokens.set(vec!["a".parse().unwrap()]);
		assert_eq!(tokens.snapshot().generation, 0);
		tokens.set(vec!["b".parse().unwrap()]);
		assert_eq!(tokens.snapshot().generation, 1);
	}
}
