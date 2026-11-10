# [S] Find out whether Cloudflare's relay really sends SOLICIT

## Goal

We know which Setup Options Cloudflare's draft-18 relay
(`https://draft-18.cloudflare.mediaoverquic.com`) sends, and moq-dev/moq#8418
is resolved on that evidence. If it sends our SOLICIT option (0x40B5A) set
to 1, the issue closes as Cloudflare's bug, with a comment showing the
captured SETUP. If it doesn't, our SETUP parsing reads its options wrongly,
and this quest's PR fixes that parsing in Rust and JS.

## Plan

Decided 2026-11-10 (maintainer, `/quest-import` of
[moq-dev/moq#8418](https://github.com/moq-dev/moq/issues/8418)):
✅ verify, then decide (rejected: close as won't fix now; a quest waiting
on Cloudflare).

Why: SOLICIT is our own extension (`drafts/draft-lcurley-moq-solicit.md`,
`rs/moq-net/src/ietf/solicit.rs`), so a third-party relay sending it is
surprising, and the maintainer asked on the issue whether it really does.
The reporter's opt-in, [moq-dev/moq#8419](https://github.com/moq-dev/moq/pull/8419),
was already closed: no publisher option exists only to work around a peer
bug.

Capture what the relay's SETUP carries by connecting a draft-18 client and
logging the decoded options, the reporter's `implementation` string
included; a debug log of the peer's declared options is worth keeping if
none exists. Don't publish anything to the relay beyond what the capture
needs.

Public API: none, unless the parsing fix changes it. Wire: none, unless our
parsing is wrong.

## Closes

- [#8418](https://github.com/moq-dev/moq/issues/8418) - Cloudflare's relay sets SOLICIT=1 but never solicits
