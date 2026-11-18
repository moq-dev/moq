# Public functions return what their callers use

## Goal

No public function in Rust, JS, or the bindings returns an outcome enum whose
callers collapse it the same way, one that is a `bool`, `Option`, `Poll`, or
`Result` in disguise, or an error variant that means a normal outcome. Each
child reshapes one surface in every language that mirrors it. Enums whose
variants callers really handle differently stay: sans-IO events, stream items
such as `Read { Frame, Gap, End }`, and state transitions.

## Plan

Planned 2026-11-18 (maintainer, /quest-plan of an audit of about 200
functions that return an enum, across Rust, JS, and the bindings). The
audit found about 60 that should change shape, most of them private; the
public ones are this epic's children, and the private ones are
[the private sweep](/quest/m4/outcome-enums-private.md).

Decided:

- ✅ Fix public and private shapes, with REVIEW.md's "Return what callers
  use" rule (added by this plan's PR) so new ones get flagged (rejected:
  public API only; the rule alone; also reshaping vendored code). Vendored
  code is out of scope: moq-rtmp's `rml` and moq-congestion's libwebrtc port.
- ✅ An error variant that means a normal outcome follows the
  [moq-dev/moq#9041](https://github.com/moq-dev/moq/pull/9041) ruling: first
  ask whether the caller needs to tell the case apart at all, and abort on any
  error when it does not.
- ✅ moq-quic's API changes freely: compatibility with quinn is not a goal
  (2026-11-18, maintainer).

The test the audit applied: when two callers collapse an enum the same way,
the API should return the collapsed form.

Public API: every child is breaking; each lists its migration. Wire: none,
unless [fill lookups](/quest/m2/outcome-enums/fill-lookups.md) needs a time
start on the wire.
