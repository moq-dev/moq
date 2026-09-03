# [M] Browser teleoperation package

## Goal

`@moq/robot` mirrors the Rust crate, so a browser client consumes the catalog
and delivery classes rather than reimplementing them.

## Plan

Follow the existing split: `net`, `hang`, `json` and `token` each have a Rust
crate and a TypeScript package, with zod schemas mirroring the Rust types.
There is no `@moq/mux`, so the catalog extension goes through the same seam
`js/hang/src/catalog/root.ts` uses to extend the root schema.

A browser observer cannot degrade the operator's classes (`clamp_combined`
bounds the window at the publisher, and `Subscription::default()` is already
`Duration::ZERO`), so this package exists for reuse, not for safety: the
catalog schema, the snapshot and append-log shapes, and the instrumentation are
the same on both sides and should be written once.

Port `js/moq-boy` onto it alongside the Rust port, for the same reason: it is
the only existing consumer and the only available falsification test.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
