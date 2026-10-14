# [S] Backport the subscribe-close fixes to 0.4.x

## Goal

On `release`, closing a `@moq/net` subscription never leaves an unhandled
rejection and never throws for an end of stream the client caused itself, so
a 0.17 patch after the pending cut (#5420) ships it. The repro below, run
against a `release` build of `@moq/net` and `@moq/signals`, reports
`unhandled=0` over 20 iterations; on 0.4.3 it reports 20.

## Plan

[The main fix](/quest/m1/subscribe-close-throw.md) explains the three
defects. `release` has all three.

Decided 2026-10-14:

- One PR onto `release` (CONTRIBUTING) that ports two fixes:
  - the `#runResponses` fix from the main quest;
  - the `raceUntil` loser fix from ca8995280, its `js/signals` hunk and test
    only (the rest of #5232 is unrelated).
- It does not hold #5420; it ships in a later 0.17 patch, like the
  [FETCH edge cases](/quest/m1/fetch-edge-cases.md). The `@moq/signals` and
  `@moq/net` patch bumps happen at that cut.
- `#openSubscribe` keeps sending a SUBSCRIBE for a producer closed during
  setup. With both fixes that is a SUBSCRIBE cancelled at once, and `main`
  avoids it through its retire logic, so the backport stays minimal.

Tests: port the main quest's tests. The close-during-setup test, which
withholds TRACK_INFO until the close, is the gate and fails on `release`
without this PR.

The repro below only sees the public API, so it times the close with
microtasks rather than observing the TRACK_INFO read. Run it on the unpatched
`release` build first and confirm 20 of 20, so a 0 on the patched build
means the fix and not a shifted race.

Repro, with `moq-relay` on this `relay.toml`:

```toml
[listen]
bind = "127.0.0.1:14443"
tls.generate = ["localhost"]
[web.http]
listen = "127.0.0.1:14443"
[auth]
public = "**"
```

and `bun repro.ts http://127.0.0.1:14443/ 20 micro`:

```ts
import * as Moq from "@moq/net";
const url = new URL(process.argv[2] ?? "http://127.0.0.1:14443/");
const iterations = Number(process.argv[3] ?? 20);
const delay = process.argv[4] ?? "0";
if (!process.env.VERBOSE) console.debug = () => {};
let unhandled = 0;
process.on("unhandledRejection", (err) => { unhandled++; console.log(`UNHANDLED #${unhandled}: ${err}`); });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const wait = () => delay === "micro"
  ? (async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); })()
  : sleep(Number(delay));
const path = Moq.Path.from(`repro/${crypto.randomUUID()}`);

// Publisher A: one track, one group left open.
const originA = new Moq.Origin.Producer();
const broadcast = originA.createBroadcast(path);
const track = broadcast.createTrack("room.json");
const group = track.appendGroup();
group.writeFrame({ payload: new TextEncoder().encode("snapshot"), timestamp: Moq.Time.Timestamp.now() });
broadcast.announce();
const connA = await Moq.Connection.connect({ url, publish: originA.consume() });

// Subscriber B.
const originB = new Moq.Origin.Producer();
const connB = await Moq.Connection.connect({ url, consume: originB });
for (let i = 0; i < iterations; i++) {
  const request = originB.request(path, { announced: true });
  let active = request.active.peek();
  while (!active) {
    await new Promise((r) => { const d = request.active.subscribe(() => { d(); r(undefined); }); });
    active = request.active.peek();
  }
  const sub = active.track("room.json").subscribe({ priority: 0 });
  await wait(); // close while the subscribe still waits for TRACK_INFO
  request.close();
  sub.close();
  await sleep(20);
}
await sleep(200); connB.close(); connA.close(); await sleep(100);
console.log(`delay=${delay} iterations=${iterations} unhandled=${unhandled}`);
process.exit(0);
```

## Required

- [A local close never throws](/quest/m1/subscribe-close-throw.md) - the `main` fix this ports
