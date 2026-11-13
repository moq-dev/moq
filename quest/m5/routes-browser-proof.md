# [S] Firefox tabs prove their route key to the relay

## Goal

Once a Firefox release follows the draft's exporter, a lite-07 session from
it connects to `moq-relay` with its route-key proof verified, as a current
Chromium tab's does, and the nightly route key lane keeps both that way.

The lane (`test/interop/clients/js/route-key.ts`, in `test/interop` nightly
and the release gate) dials the relay from each browser on lite-07 and reads
the relay's log for the tab's verified proof. Each build either must prove or
must send no proof. One that must send none either holds the lite-07 cut,
skipping or failing under `CI_STRICT` (which release PRs set), or connects
unproven by design and passes. Chromium 158 and later must prove, which the
[Playwright bump](/quest/m2/cluster-routes/playwright-pin.md)
reaches. Every Firefox build connects unproven, so the lite-07 cut does not
wait on Firefox.

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

Moved to m5 2026-11-12 (maintainer, /quest-audit): it waits on a Firefox release and the lite-07 cut ships Firefox unproven (2026-11-07, below), so it no longer gates lite-07 through the routes epic.

Decided 2026-11-06 (maintainer, /quest-complete while queueing
[moq-dev/moq#8022](https://github.com/moq-dev/moq/pull/8022)): cover Firefox
and Chromium, and never send a proof the relay will reject.

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ cut lite-07 with Firefox
unproven (rejected: the cut waits for a Firefox that proves; the relay also
accepting neqo's varint derivation, refused because it departs from the draft
and tries a second path at runtime; the relay picking the derivation from the
CONNECT's User-Agent, which is server-side UA sniffing). This replaces
2026-11-06's "a mismatch blocks the cut". An unproven tab is a supported
state, the neqo fix is one line, and the lane still fails loud if the relay
refuses a Firefox tab.

Reported upstream on 2026-11-07 as
[mozilla/neqo#4319](https://github.com/mozilla/neqo/issues/4319), with the
one-line fix (approved by the maintainer in /quest-unblock).

Once a Firefox release carries neqo's fix, gate `route_key.ts` on Firefox
majors before it, set the lane's table (`uncovered` in `route-key.ts`) to
require a proof from it on and to pass older majors unproven, and bump
Playwright to a release bundling it, the way the
[Playwright bump](/quest/m2/cluster-routes/playwright-pin.md) does for
Chromium. Verify the fixed build against the relay the way the
2026-11-06 check did: its export must match the relay's for the same session.

## Waiting

- Firefox: a release vendors the neqo fix for
  [mozilla/neqo#4319](https://github.com/mozilla/neqo/issues/4319), which
  writes the exporter context's session ID as 64 bits. neqo merged it on
  2026-11-09 in [mozilla/neqo#4323](https://github.com/mozilla/neqo/pull/4323),
  after neqo v0.34.0; Firefox main still vendored v0.34.0 that day. Check the
  neqo version in Firefox's `Cargo.lock` for each release.
