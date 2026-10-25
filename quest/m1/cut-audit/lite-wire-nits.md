# [S] Lite's version gates, refusals, and draft text agree

## Goal

The lite code and `drafts/draft-lcurley-moq-lite.md` agree on which version
carries each message and parameter, malformed values that today decode and
vanish are refused, and the one refuse-only constant in lite's parameter
decoding is gone.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670, all source-inferred.

- **UNAUTHORIZED (0x3B)** is sent on lite-05/06 (`rs/moq-net/src/lite/error.rs:281`,
  no version gate), as [In-band auth](/quest/m1/auth/README.md) decided
  ([moq-dev/moq#5004](https://github.com/moq-dev/moq/pull/5004)), but the
  draft's changelog lists it under lite-07 only. Fix the draft.
- **Implementation (0x6) SETUP parameter** is parsed on lite-05/06
  (`lite/setup.rs:283`, JS `js/net/src/lite/setup.ts:286`), where those
  drafts call it unknown and ignored, so a non-UTF-8 value from an old peer
  becomes a protocol violation. Gate it on lite-07 like `has_ietf()`. New on
  main ([moq-dev/moq#6520](https://github.com/moq-dev/moq/pull/6520)).
- **SUBSCRIBE_DROP with start > end + 1** decodes and is dropped as an empty
  range (`lite/subscribe.rs:543-556`). Refuse it with `InvalidValue`, in Rust
  and JS.
- **`MAX_PARAMS = 64`** (`lite/parameters.rs:5`) exists only because the
  duplicate check is a linear scan. Use a set and delete the constant; the
  64 KiB SETUP bound already limits the count.
- **Draft text:** the Fetch section mentions an "implicit" Subscribe ID that
  FETCH does not have, and the `(s)`/`(b)` notations are never defined.
- **Partial groups (decision):** a lite subscription GROUP with
  `frame_start > 0` is cached as a group tail (`lite/subscriber.rs:1033`) so a
  front can continue a group across routes, while `rs/moq-net/AGENTS.md`
  says never to emit or accept partial groups, except for moq-transport
  passthrough. Recommendation: keep the code, since resume depends on it,
  and ask the maintainer to add route continuation to that rule's
  exceptions. Agents never edit `AGENTS.md` unprompted, so leave this under
  `## Decisions needed` if the maintainer has not answered.

Validate with `just drafts check`.

Public API: none. Wire: the Implementation gate and the DROP refusal change
what lite-05/06 peers may send without error; no encoding changes.
