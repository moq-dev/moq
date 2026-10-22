# [S] Keep auth JSON credentials on the configured origin

## Goal

The native auth client cannot silently forward credential-bearing JSON bodies
to another origin through an HTTP redirect. Unsupported redirect behavior
fails explicitly before transmitting credentials to the new destination.

## Plan

P2 hardening, inherited from release; main also adds attached-token requests.
At main 6369eca, `rs/moq-auth/src/client.rs` validates the configured URL but
uses reqwest's default redirect policy. Tokens in JSON bodies are not protected
by stripping sensitive headers when an origin changes.

A reproduction using the same client/request construction and locked reqwest
0.13.5 sent a dummy token to a local auth listener. Its 307 redirected to a
different host/port, which received `POST /foreign` with
`{"token":"AUDIT_DUMMY_TOKEN"}`. This proves cross-origin body forwarding
in the dependency used by auth, not a full admission-path exploit or a tested
HTTPS downgrade. The configured auth service is trusted: the trigger requires
an unintended/open redirect or misconfiguration there or at its proxy, not
merely an arbitrary MoQ peer.

Prefer refusing redirects for credential exchange. If legitimate consumers
require them, settle the explicit same-origin and secure-scheme contract with
the maintainer before implementation. Cover 307/308 and all credential-bearing
requests with sentinel bodies and verify the forbidden destination receives
nothing. No MoQ wire change is expected; document any changed endpoint
configuration behavior inline. Current consumers are native token validation
and attached-token exchange. Consider a release-compatible backport after main.
