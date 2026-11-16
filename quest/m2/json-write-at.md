# [M] moq-json writes take a value and a time

## Goal

Every moq-json write takes `(value, at)`: the value, then the timestamp it
applies at. `Timed<T>` is what a read returns and appears in no write
signature. This holds for snapshot, stream, and window producers in
`rs/moq-json` and `@moq/json`, and for the catalog-registered wrappers in
`moq_mux::json`, so a caller who has learned one mode's producer knows the
others.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Writes take `(value, at)` and `Timed<T>` stays on reads only, in Rust
  and JS (rejected: `Timed` everywhere, including `mutate`, `modify`, and
  `moq_mux::json`; keep as is).

Facts found by the audit:

- Rust passes the write time three ways. `snapshot::Producer::update(Timed<&T>)`
  but `mutate(f, at)` and `modify(at)` on the same type
  (`rs/moq-json/src/snapshot/producer.rs`);
  `stream::Producer::append(Timed<&T>)`;
  `window::Producer::push(Timed<&T>)` and `pop(count: Timed<u64>)`
  (`rs/moq-json/src/window/producer.rs`), where a count is not a timed
  value.
- `moq_mux::json::Snapshot::update(&T, timestamp)` and
  `Stream::append(&T, timestamp)` (`rs/moq-mux/src/json.rs`) are already
  positional.
- JS is mixed the same way: `update({ value, at })`, `append({ value, at })`,
  and `push({ value, at })` take a `Timed<T>`, while `mutate(fn, at)` and
  `pop(count, at)` are positional (`js/json/src/{snapshot,stream,window}/producer.ts`).
- Users outside the crate, by files that name a mode: Rust moq-stats (6),
  moq-hls (5), moq-c (4), moq-mux (2), hang, moq-boy, moq-cli, moq-ffi;
  JS `js/publish` (8), `js/watch` (5), `js/hang`, `js/room`, `demo/carla`
  (3 each), `js/stats`, `demo/web`, `js/msf`, `demo/boy`.

For the PR:

- Rust: `update(&T, at)`, `append(&T, at)`, `push(&T, at)`, and
  `pop(count, at)`. JS: `update(value, at)`, `append(value, at)`, and
  `push(value, at)`. `Timed` keeps its place on `next()`, `latest()`, and
  the iterators.
- The bindings' JSON producers (`rs/moq-ffi`, `rs/libmoq`, and the
  wrappers) already take the value and the time as two arguments; check
  each and leave them.
- Recommended while the signatures are open, as one rename each: the
  config types follow one scheme per mode (`producer::Config` and
  `consumer::Config`), replacing `snapshot::Config` with
  `snapshot::consumer::Config`, `stream::Config`, and
  `window::{ProducerConfig, ConsumerConfig}`. Drop this if it grows the PR
  past a mechanical change.
- Not changed: `snapshot::Consumer::new` takes a `track::Consumer` and
  subscribes itself, while stream and window take a `track::Subscriber`
  ([moq-dev/moq#7736](https://github.com/moq-dev/moq/pull/7736), on
  purpose). Document the difference on the three constructors.
- [The catalog at its timestamp](/quest/m2/catalog-at-timestamp.md) reads
  `Timed.at` from the catalog consumer; it is a read and is untouched.

Tests: the existing producer tests, rewritten to the new calls; no
behaviour changes.

Public API: breaking. Rust `moq_json::{snapshot,stream,window}::Producer`
write methods; JS the same three producers in `@moq/json`. Wire: none.
