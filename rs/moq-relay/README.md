# moq-relay

**moq-relay** is a server that forwards subscriptions from publishers to subscribers, caching and deduplicating along the way.
It's designed to be run in a datacenter, relaying media across multiple hops to deduplicate and improve QoS.

The only argument is the path to a TOML configuration file.
See [relay.toml](../dev/relay.toml) for an example configuration.

## HTTP
Primarily for debugging, you can also connect to the relay via HTTP.

-  `GET /certificate.sha256`: Returns the fingerprint of the TLS certificate.
-  `GET /announced/*prefix`: Returns all of the announced tracks with the given (optional) prefix.
-  `GET /fetch/*path`: Returns the latest group of the given track.

The HTTP server listens on the same bind address, but TCP instead of UDP.
The default is `http://localhost:4443`.
HTTPS is currently not supported.

## Clustering
In order to scale MoQ, you will eventually need to run multiple moq-relay instances potentially in different regions.
This is called *clustering*, where the goal is that a user connects to the closest relay and they magically form a mesh behind the scenes.

**moq-relay** uses a simple clustering scheme using moq-lite.
This is both dog-fooding and a surprisingly ueeful way to distribute live metadata at scale.

We currently use a single "root" node that is used to discover members of the cluster and what broadcasts they offer.
This is a normal moq-relay instance, potentially serving public traffic, unaware of the fact that it's in charge of other relays.

The other moq-relay instances accept internet traffic and consult the root for routing.
They can then advertise their internal ip/hostname to other instances when publishing a broadcast.

Cluster arguments:

-   `--cluster-root <HOST>`: The hostname/ip of the root node. If missing, this node is a root.
-   `--cluster-node <HOST>`: The hostname/ip of this instance. There needs to be a corresponding valid TLS certificate, potentially self-signed. If missing, published broadcasts will only be available on this specific relay.

## Authentication

The relay supports JWT-based authentication and authorization with path-based access control.

For detailed authentication setup, including token generation and configuration examples, see:
**[Authentication Documentation](../../docs/auth.md)**

Key features:
- JWT tokens passed via query parameters (`?jwt=<token>`)
- Path-based authorization with `root`, `pub`, and `sub` claims
- Anonymous access support for public content
- Symmetric key cryptography (HMAC-SHA256/384/512)
- Asymmetric key cryptography (RSASSA-PKCS1-SHA256/384/512, RSASSA-PSS-SHA256/384/512, ECDSA-SHA256/384, EdDSA)

Quick example configuration in your `.toml` file:
```toml
[auth]
key = "dev/root.jwk"    # JWT signing key
public = "anon"         # Allow anonymous access to /anon prefix
```
