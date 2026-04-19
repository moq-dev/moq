use anyhow::Context;

/// Resolve a `host:port` string to a single [`std::net::SocketAddr`],
/// falling back to `default` when `addr` is `None`.
///
/// Accepts both literal socket addresses (e.g. `[::]:443`) and DNS hostnames
/// paired with a port (e.g. `fly-global-services:443`). Only the first
/// resolved address is returned; Quinn only supports a single IP when
/// binding/connecting.
pub(crate) fn resolve(addr: Option<&str>, default: &str) -> anyhow::Result<std::net::SocketAddr> {
	use std::net::ToSocketAddrs;
	addr.unwrap_or(default)
		.to_socket_addrs()
		.context("invalid address")?
		.next()
		.context("no addresses resolved")
}
