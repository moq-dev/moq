# Security Policy

## Reporting a Vulnerability

Report vulnerabilities privately through [GitHub's private vulnerability reporting](https://github.com/moq-dev/moq/security/advisories/new).
Alternatively, email <security@moq.dev>.
Please don't open a public issue, PR, or Discord thread for a suspected vulnerability.

Include the affected package and version, how to reproduce it, and the impact you expect.
AI-assisted reports are welcome, as long as the issue is real and the reproduction actually works.
We'll follow up on the advisory, and credit you when it's published unless you'd rather stay anonymous.

## Supported Versions

Fixes land on `main` and are backported to the latest release of each affected package.
Older releases don't get security fixes; upgrade to the latest.

## Scope

Anything received over the network is untrusted, so we treat these as vulnerabilities:

- Crashes, panics, or memory corruption triggered by a remote peer.
- A peer making a relay or client hold unbounded memory, CPU, or state.
- Bypassing authentication or reaching broadcasts a token doesn't grant.
- Leaking one session's media or metadata to another.

Bugs in dependencies are in scope when our code is affected.
`rs/moq-quic`, `rs/moq-sock`, and `rs/moq-tokio` carry a hard fork of quinn that `cargo audit` can't match, so quinn advisories are ported by hand; tell us if we missed one.
