# [L] Subscriber staleness is max-delay

## Goal

Subscriber staleness budgets are named `max_delay` in Rust and
`maxDelay` in JavaScript, mirrored by the bindings and the corresponding CLI
flags. Publisher retention keeps `max_age`/`maxAge` and `--max-age`. The
rename changes no retention, delivery, presentation, or wire behavior.

## Plan

Decided with the maintainer on 2026-10-02: subscriber staleness is a delivery
delay, while a publisher's retention ceiling is an age of cached content.
Use different names for these different roles rather than rename both.

- Rename `moq_net::track::Subscription::max_age` and its builder to
  `max_delay`, with `maxDelay` in `@moq/net`, and update every public consumer
  option or method that exposes the same subscriber budget in Rust and the
  bindings. Keep `track::Info::max_age`, catalog/import retention, and their
  publisher-facing options unchanged. Classify each occurrence by its role;
  do not perform an indiscriminate search-and-replace.
- Rename subscriber/export staleness CLI flags from `--max-age` to
  `--max-delay`, except T-STD export, whose own quest unifies presentation
  and staleness under `--delay`. Keep `moq play --delay` unchanged.
  Publisher/import retention remains `--max-age`. flv and mkv take the
  rename here and move to `--delay` later, in
  [FLV export delay](/quest/m1/flv-export-delay.md) and
  [MKV export delay](/quest/m1/mkv-export-delay.md) (decided in the
  2026-10-06 audit).
- Keep the wire field identifiers, encoding, and interpretation unchanged.
  Describe subscriber staleness consistently in the matching draft and docs;
  a terminology edit must not accidentally rename the publisher's field.
- This is a published API and CLI break.
  Replace the old APIs and flags instead of adding aliases or compatibility
  shims. Follow the existing unsupported-flag error convention.
- Search the whole repository for the affected APIs and binaries. Update the
  binding samples, concept documentation, CLI help, recipes, and examples
  inline. The maintainer chose existing-document updates, without a separate
  explanatory guide quest.
- Use existing subscription/retention tests to verify behavior is unchanged,
  and extend CLI parsing tests to distinguish publisher `--max-age` from
  subscriber `--max-delay`. Verify examples against `--help` and run the
  affected checks plus `just test interop --all` for the binding changes.

Public API: breaking subscriber option, method, and CLI names;
publisher retention names stay the same. Wire: no encoding or behavior change.

## Related

- [T-STD delay](/quest/m1/tstd/delay.md) - TS export already unifies presentation and staleness under `--delay`
