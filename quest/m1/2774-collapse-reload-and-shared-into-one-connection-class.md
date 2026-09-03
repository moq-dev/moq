# [L] Collapse Reload and Shared into one Connection class

## Goal

Implement and verify the behavior tracked in [#2774](https://github.com/moq-dev/moq/issues/2774)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #2705, which added `Connection.Shared` (a reactive handle on a pooled `{origin, reconnect loop}` keyed by relay URL) alongside the existing `Connection.Reload`.

#### The problem with the current names

`Shared` names the mechanism, not the role. Sharing is how it works; what it is to a caller is "the connection to a relay". A caller writing `new Connection.Shared({ url })` is thinking "I want a connection", not "I want a shared one". Three consequences:

- It marks the default. The only reasons not to share are a pinned certificate or a supplied transport, and both send you to `Reload` instead. A name that qualifies the thing everyone should use implies an `Unshared` peer that does not meaningfully exist.
- Sharing is supposed to be invisible. Two `<moq-watch>` tiles becoming one QUIC session is the selling point precisely because nobody has to arrange it, so encoding it in the type name makes callers reason about something they should not have to.
- It ages badly. The stated direction is reconnect and GOAWAY on by default with `Established`/`Reload` dropped from the published surface. At that point this class *is* the connection API and the qualifier is noise, plus a second breaking rename to remove it.

The repo rule this trips is "name by role, not by today only implementation" (CLAUDE.md, Public API Scrutiny). Note there is no Rust counterpart to converge on: `moq_native::Reconnect` names the behavior and there is no pooling type at all.

#### Proposal

One `Connection` class, with reconnect and sharing as defaults rather than as separate types:

- `new Connection({ url })` reconnects and shares by default.
- `share: false` for the cases that cannot be shared honestly (pinned certificate, supplied transport).
- `Established` and `Reload` drop off the published surface (`@internal` or unexported), leaving one entry point.
- GOAWAY slots into the same loop: the redirect handler dials the new URI and attaches it to the same origin, and nothing above holds a session.

#### The part that needs design, not just a rename

`Reload.close()` closes the connection; `Shared.close()` releases one handle and lets the last one out tear it down. Collapsing them means one `close()` whose meaning depends on construction options, which could easily be worse than two honest types. Options worth weighing: always refcount (a lone holder closing is then the same thing), or keep the unshared path internal-only so the ambiguity never reaches a caller.

Doing this while #2705 is already breaking would save consumers a second migration of the same call sites. Deferring is defensible; the cost is that migration happening twice.

## Closes

- [#2774](https://github.com/moq-dev/moq/issues/2774) - close this issue when the quest finishes
