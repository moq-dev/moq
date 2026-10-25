# [XS] JS JSON and MSF readers refuse what Rust refuses

## Goal

`@moq/json` and `@moq/msf` refuse the same malformed input Rust does, so a
browser and a Rust reader never disagree about one catalog.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. JS side reproduced,
Rust side source-inferred. The UTF-8 handling is inherited; the strict parse
pass ([moq-dev/moq#5941](https://github.com/moq-dev/moq/pull/5941)) is new
on main.

- `js/json/src/parse.ts:3` uses a default `TextDecoder`, which replaces
  invalid UTF-8, strips a BOM, and accepts lone surrogates;
  `serde_json::from_slice` rejects all three. Use
  `new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })` and check
  `isWellFormed()` in the existing walk (`:15`).
- `js/msf/src/catalog.ts:153-179` accepts a missing or non-inline `initRef`,
  an `initRef` conflicting with `initData`, and duplicate keys (last wins);
  `rs/moq-msf/src/lib.rs:339-365` refuses each. Mirror the three checks.

Public API: none. Wire: none.

## Related

- [MSF duplicate init](/quest/m1/msf-duplicate-init.md) - the repeated `initDataList` id in the same reader
