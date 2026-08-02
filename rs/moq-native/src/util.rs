use std::net::SocketAddr;

/// Resolve a `host:port` string to a single [`std::net::SocketAddr`],
/// falling back to `default` when `addr` is `None`.
///
/// Accepts both literal socket addresses (e.g. `[::]:443`) and DNS hostnames
/// paired with a port (e.g. `fly-global-services:443`). Only the first
/// resolved address is returned; Quinn only supports a single IP when
/// binding.
pub(crate) fn resolve(addr: Option<&str>, default: &str) -> std::io::Result<SocketAddr> {
	use std::net::ToSocketAddrs;
	addr.unwrap_or(default)
		.to_socket_addrs()?
		.next()
		.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses resolved"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn resolves_socket_literal() {
		let addr = resolve(Some("[::]:0"), "[::]:443").unwrap();
		assert!(addr.ip().is_unspecified());
		assert_eq!(addr.port(), 0);
	}

	#[test]
	fn resolves_dns_hostname() {
		let addr = resolve(Some("localhost:0"), "[::]:443").unwrap();
		assert!(addr.ip().is_loopback());
		assert_eq!(addr.port(), 0);
	}

	#[test]
	fn falls_back_to_default() {
		let addr = resolve(None, "127.0.0.1:1234").unwrap();
		assert_eq!(addr.ip().to_string(), "127.0.0.1");
		assert_eq!(addr.port(), 1234);
	}
}
