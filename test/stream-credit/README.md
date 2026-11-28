# Stream credit harness

A browser tab reads 300 live tracks through a Rust relay over WebTransport,
three times the ~100 streams Chromium and Firefox let a server open to one
session. The relay hands the streams it can open to the highest-priority
groups and makes room by resetting a lower-priority group's stream with
TOO\_FAR\_BEHIND, which gives that group up; the tab fetches it whole instead.

## Running

```bash
just test stream-credit
```

```bash
just test stream-credit --version moq-lite-07-wip --engine chromium --timeout 120
```

Each run covers lite-07 and lite-06 in Chromium and Firefox, about 15 seconds
per cell. `cargo build -p moq-relay` builds the relay from this checkout.
`STREAM_CREDIT_PROFILE` picks its cargo profile, and `RELAY_BIN` points at a
prebuilt relay instead. A failing run keeps its run directory: the relay log
(with the publisher's scheduling at debug level), and per cell the page's
console, `report.json` with every group each track read, or `pending.json`
with the tracks still open.

## Shape

`run.sh` starts the relay, then runs `driver.ts` once per version and engine.
The driver plays every other part:

- **The publisher** is `@moq/net` in Bun, over WebSocket, whose qmux stream
  limit is far above 300, so the browser's cap is the only one in play. Each
  track keeps one group open at a time, as a live video track keeps its GOP
  open: a frame at its start, one halfway through, and its end as the next
  group starts, 40 groups at 200 ms. The high tier starts its groups half an
  interval after the others, while their streams hold every credit the browser
  gave, so the relay must reset a lower group's stream to make room.
- **The reader** is a page built from `src/` with Vite, in Playwright's
  Chromium or Firefox. It dials the relay over WebTransport only and subscribes
  to every track in three tiers of subscriber priority. A group that stops
  early for any reason but going stale (`OLD`) is fetched, as a reader that
  still wants it would. A lite-07 fetch is a one-shot subscription whose group
  competes for the same cap, so the relay may reset it with TOO\_FAR\_BEHIND
  as well, and the tab asks again, as the draft tells a subscriber that still
  wants a preempted group to:

  | Tier | Priority | Tracks | Delay budget |
  | --- | --- | --- | --- |
  | high | 2 | 30 | 500 ms |
  | mid | 1 | 120 | live edge |
  | low | 0 | 150 | live edge |

A cell passes when:

- the high tier read every group whole, every frame in order;
- every subscription ended cleanly within 10 s of the publisher finishing
  (the relay drains the last groups in under a second on loopback). A
  lite-07 reader waits for every group the relay owes it, so ending shows
  each group it missed was named in a drop;
- the tiers read frames in priority order, and the low tier lost some, so the
  cap bound;
- some group stopped early, so the relay reset a stream to make room, and
  every group the tab fetched came back whole.

With every tier at one priority, the high tier misses groups past its budget,
so the run fails.

Both browsers mishandle a reset that lands just after its stream's data,
before the page read it, which a reset of a freshly opened stream does:
Chromium loses the reset, leaving the stream open forever, and Firefox reports
it as a bare `TypeError` without its code. The `@moq/net` reader copes, since
the relay also names every group it resets in a drop, so a failure here that
names a stream left open or an uncoded `TypeError` is worth checking against
those first.
