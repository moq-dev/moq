# [S] Firefox tabs prove their route key to the relay

## Goal

Before the lite-07 cut, a lite-07 session from a current Firefox tab connects
to `moq-relay` with its route-key proof verified, as a current Chromium tab's
does, and the nightly route key lane keeps both that way.

The lane (`test/interop/clients/js/route-key.ts`, in `test/interop` nightly
and the release gate) dials the relay from each browser on lite-07 and reads
the relay's log for the tab's verified proof. Each build either must prove or
is not yet covered: it must then send no proof, and it skips, or fails under
`CI_STRICT`, which release PRs set. Chromium 158 and later must prove, which
the [Playwright bump](/quest/m2/cluster-routing/routes/playwright-pin.md)
reaches. Every Firefox build is not yet covered.

Firefox's exporter departs from draft-ietf-webtrans-http3 section 4.8: neqo
writes the session ID into the exporter context as a varint
(`neqo-http3/src/webtransport.rs`, since
[mozilla/neqo#3464](https://github.com/mozilla/neqo/pull/3464)), where the
draft has used a 64-bit field since the exporter was defined (draft-10). The
relay and Chromium follow the draft. Checked 2026-11-06 against Firefox 157,
whose export matches the relay's derivation with a varint session ID and no
other. Firefox 160 changes only the arguments
([bug 2079306](https://bugzilla.mozilla.org/show_bug.cgi?id=2079306)), and
neqo's `main` still writes the varint. So `@moq/net` sends no proof from any
Firefox (`js/net/src/lite/route_key.ts`, keyed on the user agent, since the
exporter's arity cannot tell the derivation apart), and a Firefox tab
connects unproven rather than being refused.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete while queueing
[moq-dev/moq#8022](https://github.com/moq-dev/moq/pull/8022)): cover Firefox
and Chromium, treat a mismatch as a bug that blocks the cut, and never send a
proof the relay will reject.

Once a Firefox release carries neqo's fix, gate `route_key.ts` on Firefox
majors before it, set the lane's table (`uncovered` in `route-key.ts`) to
require a proof from it on, and bump Playwright to a release bundling it,
the way the [Playwright bump](/quest/m2/cluster-routing/routes/playwright-pin.md)
does for Chromium. Verify the fixed build against the relay the way the
2026-11-06 check did: its export must match the relay's for the same session.

## Waiting

- Maintainer: approve reporting the derivation to mozilla/neqo (outside the
  moq-dev and kixelated orgs, so an agent may not post unasked), as an issue
  with a one-line patch writing the session ID as 64 bits; the draft is in
  [moq-dev/moq#8116](https://github.com/moq-dev/moq/pull/8116). Done when the
  issue or PR exists.
- Maintainer: decide whether the lite-07 cut waits for a Firefox that proves.
  (1) Wait, as decided: the release gate fails on Firefox until a fixed
  Firefox ships and Playwright bundles it, on a timeline outside our control.
  (2) Cut with Firefox unproven, as Safari tabs already are: the lane passes a
  Firefox that sends no proof under `CI_STRICT`, and flips to proven later.
  (3) Have the relay also accept neqo's derivation: refused, since it departs
  from the draft and tries a second path at runtime. Recommend (2): the fix
  is upstream, an unproven tab is a supported state, and the lane still
  fails loud if a Firefox tab is refused. Done when this Plan records the pick.
- mozilla/neqo: the exporter context writes the session ID as 64 bits; check
  `webtransport_export_keying_material` in `neqo-http3/src/webtransport.rs`,
  then the first Firefox release vendoring it.
