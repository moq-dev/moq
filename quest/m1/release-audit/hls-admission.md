# [M] Hold HLS resources to the playlist's origin

## Goal

HLS import fetches only resources with the playlist's scheme and authority,
including every redirect hop, so a playlist cannot steer the importer to
another origin.

## Plan

Security-sensitive follow-up from source review at main 1d43c096e0, also
relevant to release. Decided by the maintainer on 2026-10-22 and 2026-10-23:

- ✅ Same-origin only: a reference is accepted only when the URL it finally
  resolves to has the playlist's scheme, host, and port, whatever form the
  reference took (absolute, network-path, or relative). The check lives at
  the one fetch owner so every resource kind shares it. Cross-origin CDN
  references are refused, because a playlist must not steer the importer to
  internal or third-party hosts.
- ✅ Every redirect hop is held to the same rule before it is followed, not
  just the final URL, because an intermediate hop is already a request to
  the other origin.
- ✅ Drop `Config::client`: a caller's client keeps its own redirect policy,
  which the importer cannot override per request, and it has no in-repo
  consumer. A release port that keeps the API documents that the caller's
  client owns redirect handling.
- `file:` resources are read only when the playlist itself is a `file:` URL,
  which keeps local fixtures working. Other schemes are refused with
  `Error::ResourceRefused`, which ends the import.

Report the caller-visible policy and API change and consider a release
backport after validation.
