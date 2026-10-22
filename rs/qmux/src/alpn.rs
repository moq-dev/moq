//! ALPN / subprotocol negotiation helpers shared by TLS and WebSocket transports.
//!
//! Each entry is `(alpn, versions)`: a single ALPN paired with the QMux
//! wire-format versions it can ride on. The wire form `{version.prefix()}{alpn}`
//! is emitted once per version, in order. An empty `versions` slice means
//! every QMux draft this crate knows about ([`Version::ALL`]).
//!
//! Bare version ALPNs (`qmux-02`, `qmux-01`, `qmux-00`, no app protocol
//! attached) are advertised/accepted by default, so a peer that only knows a
//! wire-format version can still connect. Set the `require_protocol` flag to
//! suppress them and advertise/accept only the configured prefixed pairs.
//!
//! [`parse`] recovers `(Version, Option<String>)` from a negotiated wire-format
//! ALPN, refusing anything that names no QMux version.

use crate::{Error, Version};

/// Resolve an entry's `versions` slice: empty falls back to every supported
/// QMux draft, mirroring JS's `null` value.
pub(crate) fn expand_versions(versions: &[Version]) -> &[Version] {
	if versions.is_empty() { Version::ALL } else { versions }
}

/// Build the ALPN list from `(alpn, versions)` entries.
///
/// Each entry emits `{v.prefix()}{alpn}` per version in `expand_versions(versions)`.
/// Unless `require_protocol` is set, the bare version ALPNs (`qmux-02`,
/// `qmux-01`, `qmux-00`) are appended after the prefixed pairs as a fallback for
/// peers that don't want to commit to an app protocol.
///
/// Suitable for TLS ALPN or WebSocket `Sec-WebSocket-Protocol`.
pub(crate) fn build<'a>(
	entries: impl IntoIterator<Item = (&'a str, &'a [Version])>,
	require_protocol: bool,
) -> Vec<String> {
	let mut out: Vec<String> = Vec::new();
	for (alpn, versions) in entries {
		for &version in expand_versions(versions) {
			out.push(format!("{}{}", version.prefix(), alpn));
		}
	}
	if !require_protocol {
		out.extend(Version::ALL.iter().map(|v| v.alpn().to_string()));
	}
	out
}

/// Parse a negotiated wire-format ALPN into `(version, app_protocol)`.
///
/// Recognises `{qmux-VV.}{alpn}` as `(QMuxVV, Some(alpn))` and the bare version ALPN
/// `qmux-VV` as `(QMuxVV, None)`. Anything else, an absent ALPN included, names no wire
/// format we speak.
pub(crate) fn parse(alpn: Option<&str>) -> Result<(Version, Option<String>), Error> {
	let alpn = alpn.unwrap_or_default();
	for &v in Version::ALL {
		if alpn == v.alpn() {
			return Ok((v, None));
		}
		if let Some(rest) = alpn.strip_prefix(v.prefix()) {
			let app = (!rest.is_empty()).then(|| rest.to_string());
			return Ok((v, app));
		}
	}
	Err(Error::InvalidProtocol(alpn.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn build_appends_bare_alpns_by_default() {
		let out = build([("moq-lite-04", &[Version::QMux01][..])], false);
		assert_eq!(out, vec!["qmux-01.moq-lite-04", "qmux-02", "qmux-01", "qmux-00"]);
	}

	#[test]
	fn build_emits_only_prefixed_pairs_when_require_protocol() {
		let out = build(
			[
				("moq-lite-04", &[Version::QMux01][..]),
				("moq-transport-17", &[Version::QMux00][..]),
			],
			true,
		);
		assert_eq!(out, vec!["qmux-01.moq-lite-04", "qmux-00.moq-transport-17"]);
	}

	#[test]
	fn build_expands_empty_versions_to_all_qmux_drafts() {
		let out = build([("moq-lite-04", &[][..])], true);
		assert_eq!(
			out,
			vec!["qmux-02.moq-lite-04", "qmux-01.moq-lite-04", "qmux-00.moq-lite-04"]
		);
	}

	#[test]
	fn build_emits_one_pair_per_listed_version() {
		let out = build([("moq-lite-04", &[Version::QMux00, Version::QMux01][..])], true);
		assert_eq!(out, vec!["qmux-00.moq-lite-04", "qmux-01.moq-lite-04"]);
	}

	#[test]
	fn build_empty_by_default_emits_only_bare_alpns() {
		let entries: [(&str, &[Version]); 0] = [];
		let out = build(entries, false);
		assert_eq!(out, vec!["qmux-02", "qmux-01", "qmux-00"]);
	}

	#[test]
	fn build_empty_with_require_protocol_emits_nothing() {
		let entries: [(&str, &[Version]); 0] = [];
		let out = build(entries, true);
		assert!(out.is_empty());
	}

	#[test]
	fn parse_recognises_prefixed_pairs() {
		assert_eq!(
			parse(Some("qmux-02.moq-lite-04")).unwrap(),
			(Version::QMux02, Some("moq-lite-04".to_string()))
		);
		assert_eq!(
			parse(Some("qmux-01.moq-lite-04")).unwrap(),
			(Version::QMux01, Some("moq-lite-04".to_string()))
		);
		assert_eq!(
			parse(Some("qmux-00.moq-transport-17")).unwrap(),
			(Version::QMux00, Some("moq-transport-17".to_string()))
		);
	}

	#[test]
	fn parse_recognises_bare_versions() {
		assert_eq!(parse(Some("qmux-02")).unwrap(), (Version::QMux02, None));
		assert_eq!(parse(Some("qmux-01")).unwrap(), (Version::QMux01, None));
		assert_eq!(parse(Some("qmux-00")).unwrap(), (Version::QMux00, None));
	}

	#[test]
	fn parse_refuses_anything_else() {
		for alpn in [
			None,
			Some(""),
			Some("h2"),
			Some("webtransport"),
			Some("webtransport.moq-lite-04"),
		] {
			assert!(
				matches!(parse(alpn), Err(Error::InvalidProtocol(_))),
				"{alpn:?} names no QMux version"
			);
		}
	}
}
