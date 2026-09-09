# [S] LARGEST_OBJECT is a bare Location from draft-17 on

## Goal

A draft-17-or-later peer's SUBSCRIBE_OK decodes instead of failing with `short
buffer`, because moq-net reads LARGEST_OBJECT (0x09) as the two bare varints
section 10.2 defines rather than a length-prefixed value. The parameter is also
advertised on every draft that defines it, in that draft's own form, so a
subscriber can size a backfill against a real Largest Location the first time it
subscribes to a track with content.

Boundaries: only Message Parameters changed at draft-17. Setup Options and Track
Properties are still Key-Value-Pairs, so the odd-type Length rule in section
1.4.3 still governs them and neither codec moves. moq-transport is someone
else's draft, so nothing under `drafts/` changes, and `mod ietf` is private, so
no public API moves either.

## Plan

What the tree does today, on main and dev alike:

- `rs/moq-net/src/ietf/parameters.rs` `impl Param for Location` writes two
  varints inside a length-prefixed value on every draft, explaining itself with
  "LARGEST_OBJECT (0x09) is an odd type, so the Key-Value-Pair rule gives it a
  Length on every draft". `location_inner_version` pins the inner varints to the
  draft-15 encoding for draft-14/15/16.
- `rs/moq-net/src/ietf/subscribe.rs:249` filters the emit with
  `Filter::is_draft20(version)`, so the encoder is only ever exercised on
  draft-20 and the decode path had never met another draft-18 sender.
- `js/net/src/ietf/parameters.ts` repeats both the length prefix and the comment
  in its `"location"` case, and `js/net/src/ietf/subscribe.ts:252` repeats the
  `Filter.isDraft20` gate.

What the drafts say. Draft-16 section 9.2 says "Parameters are serialized as
Key-Value-Pairs" and section 9.2.2.7 calls LARGEST_OBJECT "a length-prefixed
Location structure", so the current code is right through draft-16. Draft-17
replaced that: section 9.3, and section 10.2 from draft-18 on, defines
`Message Parameter { Type Delta (vi64), Value (..) }` with no Length field at
all, and lists `Location: Two consecutive varints (Group, Object)` beside
`Length-prefixed` as separate per-parameter encodings. Section 10.2.11 on
draft-18 and 19, 10.2.17 on draft-20, makes LARGEST_OBJECT a Location.
LARGEST_OBJECT is the only odd-typed Message Parameter this touches:
AUTHORIZATION_TOKEN (0x03), SUBSCRIPTION_FILTER and its LOCATION_FILTER
successor (0x21), and FILL_PARAMETERS (0x23) each say in their own definition
that they use length-prefixed encoding.

There is no ambiguity to raise with the working group. cloudflare/moq-rs, the
IETF-aligned implementation behind Cloudflare's production relays, splits
`encode_message_params`/`decode_message_params` from its Key-Value-Pair codec on
its `draft-18-dev` branch, lists LARGEST_OBJECT in
`TWO_VARINT_VALUE_PARAMETER_TYPES`, and pins the wire bytes `[1, 0x09, 4, 5]` in
`largest_object_uses_two_inline_varints`. moq-playa and red5-moq-relay read it
the same way. We are the outlier.

The work:

- `impl Param for Location`: length-prefixed through draft-16, two bare varints
  from draft-17 on. `location_inner_version` survives only for the draft-16 and
  earlier branch. Replace the comment with what the two sections actually say
  and cite them.
- Drop the `is_draft20` filter in `subscribe.rs` and `isDraft20` in
  `subscribe.ts`, so SUBSCRIBE_OK carries LARGEST_OBJECT on every draft that
  defines it once the track has content, which section 10.2.11 makes a MUST. The
  gate's stated reason is that a peer built before we sent it closes the session
  over an unexpected SUBSCRIBE_OK parameter; the implementation that reason was
  written about decodes SUBSCRIBE_OK parameters into a generic `KeyValuePairs`
  and never rejects an unknown key, on `main` (draft-16, and Cloudflare's
  deployed draft-14 and draft-16 relays) as well as on `draft-18-dev`. Confirm
  that against a deployed relay before landing rather than on the source alone.
- Mirror the encoding fix and the comment in `js/net/src/ietf/parameters.ts`.

Tests, byte-exact per draft rather than round-trip alone, since a round-trip
against ourselves is what hid this:

- Draft-16 emits the length-prefixed form; draft-17 through draft-20 emit
  `[count, 0x09, group, object]`, matching cloudflare/moq-rs's own assertion.
- A draft-18 SUBSCRIBE_OK carrying a bare LARGEST_OBJECT decodes to the right
  Location instead of `short buffer`. This is the regression test for the
  report: Group 427 as a two-byte varint currently becomes a demand for 427
  bytes.
- The same pair in `js/net`, and the bare form added to the `rs/moq-net/src/fuzz.rs`
  corpus if it costs nothing.

## Closes

- [#3558](https://github.com/moq-dev/moq/issues/3558) - close this issue when the quest finishes

## Related

- [Publisher priority](/quest/m0/3534-ietf-publisher-priority.md) - the sibling sweep of what a SUBSCRIBE_OK carries
- [FETCH_OK](/quest/m0/3559-ietf-fetch-ok.md) - the other draft-18 interop defect from the same reporter
