# Stream credit harness

A browser tab reads 300 live tracks through a Rust relay over WebTransport,
three times the ~100 streams Chromium and Firefox let a server open to one
session. The relay hands the streams it can open to the highest-priority
groups and, on lite-07, makes room with a YIELDED reset that pauses a
lower-priority group for a later stream to continue.

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
  gave, so the relay must pause a lower group (YIELDED) to make room.
- **The reader** is a page built from `src/` with Vite, in Playwright's
  Chromium or Firefox. It dials the relay over WebTransport only and subscribes
  to every track in three tiers of subscriber priority:

  | Tier | Priority | Tracks | Delay budget |
  | --- | --- | --- | --- |
  | high | 2 | 30 | 500 ms |
  | mid | 1 | 120 | live edge |
  | low | 0 | 150 | live edge |

A cell passes when:

- the high tier read every group whole, every frame in order;
- every subscription ended cleanly within 10 s of the publisher finishing
  (the relay drains the last groups in under a second on loopback);
- on lite-07, every track read its final group whole, since its readers wait
  for every group the relay owes them. A lite-06 reader has no drops to wait
  on, so it ends at the end it was told, and a starved track may lose the
  final group still queued for credit;
- the tiers read frames in priority order, and the low tier lost some, so the
  cap bound;
- on lite-07, the relay log shows paused groups continued, the YIELDED path.

With every tier at one priority, the high tier misses groups past its budget
and the relay continues no paused group, so the run fails.

Both browsers mishandle a reset that lands just after its stream's data,
before the page read it, which a YIELDED reset of a freshly opened stream
does: Chromium loses the reset, leaving the stream open forever, and Firefox
reports it as a bare `TypeError` without its code. The `@moq/net` reader
copes, since a lite-07 publisher follows every reset with a continuation or a
drop, so a failure here that names a stream left open or an uncoded
`TypeError` is worth checking against those first.
