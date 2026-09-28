# [S] AUTH endings are exact on both sides

## Goal

The loose ends Codex left on [#4062](https://github.com/moq-dev/moq/pull/4062)
are closed, so every way an AUTH stream ends reports what actually happened:

- A lite AUTH_ERROR whose code does not fit a `u32` is a protocol violation.
  Today `rs/moq-net/src/lite/session.rs` saturates it with
  `u32::try_from(refused.code).unwrap_or(u32::MAX)`, so distinct peer codes
  collapse into one and the app sees a code the peer never sent.
- JS `AuthSession.close()` in `js/net/src/auth_session.ts` clears its tokens
  but never recomputes the union, so a retained `auth.grant` keeps reporting
  a live grant after the session closed. Rust already clears it.
- A JS presenter whose acceptor ends the grant with a clean FIN exits the read
  loop without closing its own write half, unlike the AUTH_ERROR branch, so
  the acceptor's `Issued.closed` stays pending until the session ends.
- A Rust `Issued::closed()` never resolves if the session drops the task
  serving that token (`AuthServe` in the lite publisher, `Serve` in
  `ietf/auth.rs`): only the task's own completion records `issue.peer`.

## Plan

- Fail loud on the unrepresentable code rather than widen the error type:
  the codes AUTH_ERROR carries are session codes, which are `u32` everywhere
  else.
- Settle a dropped serve task from a drop path (a guard that records the
  session's error on `Issue` and wakes waiters), so no exit path can forget
  it.
- One regression test per item, each failing without its fix.

The fifth deferred item, a grant recheck after async origin resolution,
belongs to [Origin narrowing](/quest/m1/auth/narrowing.md) with the other
watcher races.

Public API: none. Wire: none.
