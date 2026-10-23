# [M] moqsrc follows referenced broadcasts' announcements

## Goal

`moqsrc` follows a catalog-referenced sibling's announcements the way
`@moq/watch` does: each rendition reads the instance `moq_mux::Source::follow`
hands out, idles its pad (no EOS) while the sibling is offline, and restarts on
the same pad when another instance serves it. A reference that names an
`epoch` never plays another instance. Until this lands, `moqsrc` waits for a
sibling's first announcement and keeps that instance, like every other
non-following reader.

## Plan

Split from the reference announcements work (maintainer, 2026-10-22), which
landed `moq_mux::follow` and the waiting `Source::bind`. The moqsrc half drew a
new state-machine finding every review round, so it gets its own design pass.

- Start binds the rendition; End idles its pad; another instance restarts the
  stream on the same pad with a fresh stream-start, caps, and a segment.
- Renditions reading one sibling instance share that instance's timeline,
  whenever each starts on it, including one still subscribing when a
  replacement takes over.
- A sibling track that finishes while its instance stays up idles the pad and
  keeps following; a finished record names the instance that served it, so a
  reshape (disable and enable, an epoch handoff) still plays the next one.
- A rendition the catalog delists drains the instance it reads, then ends its
  pad and follows no later instance.
- A pump that has returned but is still in `active` must not swallow a relist:
  a relist arriving before the pump is joined matches its shape and starts
  nothing, for an idle delisted sibling and for a delisted catalog rendition
  whose track ends just before the relist. Cover both cases.

Weigh a simpler state model (one task per sibling instance owning its
renditions, or reconciling after each pump exits) before patching each case.

Public API: none (moqsrc behavior). Wire: none.

## Related

- [Sink restart follow](/quest/m1/sink-restart-follow.md) - moves moqsrc's own broadcast follow onto `follow::Instances`
