# [M] Automatic LetsEncrypt support

## Goal

Implement and verify the behavior tracked in [#709](https://github.com/moq-dev/moq/issues/709)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

One of the biggest hurdles to running `moq-relay` is getting a certificate. We could simplify the setup process by using a Acme library to automatically provision and rotate TLS certificates.

LetsEncrypt has an API that performs a HTTP/TLS challenge to prove that you own a given domain name. `moq-relay` already requires a public IP address and listening on UDP, optionally listening on TCP too. We could leverage that to perform the challenge in-process without an external `certbot` process. I would use [instant-acme](https://crates.io/crates/instant-acme).

Bonus points for saving the certificate to disk and only performing the challenge if it's missing or about to expire. This will avoid making LetsEncrypt a startup blocker and potentially avoid being rate limited.

## Closes

- [#709](https://github.com/moq-dev/moq/issues/709) - close this issue when the quest finishes
