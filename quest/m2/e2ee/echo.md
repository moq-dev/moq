# [M] Encrypted viewer feedback

## Goal

A protected broadcast can solicit viewer feedback, and every `.echo` report is as confidential to relays as the media it describes.

Today `moq_mux::e2ee::Producer` refuses a catalog with an `echo` section, and the browser and CLI refuse `echo` with a credential, because a viewer would publish its `feedback.json` in plaintext beside the protected broadcast. This quest replaces those refusals with an encrypted path; until it lands, they stay.

## Plan

- Each viewer encrypts its report under the broadcast's credential with an epoch it mints itself (`credential.produce()`), like any other publisher instance (maintainer, 2026-10-17). A viewer already holds the whole credential, and the drafts do not promise sender authenticity between credential holders. A fresh epoch keeps nonces unique without new key material. A viewer cannot write under the publisher's generation: it only holds a consume-only one, and sharing an epoch repeats nonces.
- The `echo` section rides inside the encrypted catalog, so its path is already hidden in the catalog itself. The relay still sees the prefix the viewers announce under, as with any broadcast path. Hide the viewer's application-assigned name with an opaque segment, and keep the literal `.echo` suffix so readers can still recognize feedback and skip it as media. The publisher discovers reports by announcement under the prefix, binds each route's epoch with `credential.consume`, and reads the physical `feedback.json`.
- Rendition keys in a report are already the physical names from the decrypted catalog, so the snapshot schema does not change.
- Expected touch points: the `echo` section in `drafts/draft-lcurley-moq-hang.md` and a feedback note in `drafts/draft-lcurley-moq-e2ee.md`; `moq-mux` (lift the producer's refusal, and an encrypted feedback reader); the `moq-cli` and `@moq/watch` echo paths; and `@moq/e2ee`. No moq-lite or MoQ Transport wire change, and no new `moq-e2ee` API is expected.
- Rejected (2026-10-17):
  - A separate feedback credential derived from the broadcast secret under its own HKDF label. That needs new key-schedule API in both cores and a draft change, and only pays off if feedback should be writable by parties who cannot watch, which is not a goal today.
  - Letting a protected catalog solicit plaintext feedback, which breaks the epic's rule that every application payload is confidential.
- Cover a protected publisher reading two encrypted viewers' reports, a plaintext reader finding nothing it can parse, a wrong credential failing authentication, and Rust and TypeScript viewers interoperating.

## Related

- [Media stats and viewer feedback](/quest/m2/stats/README.md) - the plaintext `echo` design this extends
- [Encrypted browser components](/quest/m2/e2ee/browser.md) - owns the browser `echo` refusal this replaces
- [Encrypted native CLI](/quest/m2/e2ee/cli.md) - owns the CLI `--echo` refusal this replaces
