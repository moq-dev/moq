use crate::Error;

/// Validate that a protocol name contains only valid HTTP token characters (tchar).
#[cfg_attr(
	not(any(feature = "ws", feature = "tcp", all(unix, feature = "uds"))),
	expect(
		dead_code,
		reason = "only the byte-stream and WebSocket transports negotiate a protocol"
	)
)]
pub(crate) fn validate_protocol(protocol: &str) -> Result<(), Error> {
	if protocol.is_empty() {
		return Err(Error::InvalidProtocol(protocol.to_string()));
	}

	for c in protocol.chars() {
		if !is_tchar(c) {
			return Err(Error::InvalidProtocol(protocol.to_string()));
		}
	}

	Ok(())
}

/// Check if a character is a valid HTTP token character per RFC 7230.
fn is_tchar(c: char) -> bool {
	matches!(c,
		'a'..='z' | 'A'..='Z' | '0'..='9'
		| '!' | '#' | '$' | '%' | '&' | '\'' | '*' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~'
	)
}
