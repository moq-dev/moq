# [S] moq-e2ee on rind

## Goal

moq-e2ee's credential derivation (HKDF-SHA256), credential IDs and name nonces
(HMAC-SHA256), and frame and track-name protection (AES-128-GCM) run on rind,
with no aws-lc-rs dependency and byte-identical output.

## Plan

Decided with the maintainer on 2026-10-26; see the
[epic](/quest/m2/rind/README.md). A test pins ciphertexts from the aws-lc
implementation before the switch, so interop with already-protected content
is proven, not assumed.

Public API: none, unless an error type exposes aws-lc-rs. Wire: unchanged.
