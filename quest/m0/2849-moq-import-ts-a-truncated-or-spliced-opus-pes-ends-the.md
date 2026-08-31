# [M] moq import ts: a truncated or spliced Opus PES ends the session

## Goal

Implement and verify the behavior tracked in [#2849](https://github.com/moq-dev/moq/issues/2849)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

##### Summary

A truncated or spliced Opus PES ends the whole session, the same way a damaged AC-3 header used to before #2751. Found while reviewing #2823; it predates that PR and is not introduced by it.

##### Mechanism

`OpusStream::write` treats any malformed packet as fatal, and the error travels up out of `Import::decode`, which the callers treat as terminal:

```rust
let (header_len, size) = parse_opus_control_header(&data[offset..])?;
let start = offset + header_len;
let end = start + size;
anyhow::ensure!(end <= data.len(), "Opus access unit exceeds PES payload");
```

A PES does not have to be damaged in transit to reach this. `handle_pes_start` flushes whatever is pending whenever the next PES begins, so a PES left short by a wrap, a dropped packet, or a capture that starts mid-stream is handed to `write` as though it were complete.

##### Reproduction

Loop the in-tree fixture at each packet boundary and import each result:

```rust
let data = include_bytes!("test_data/opus.ts");
for cut in (188..data.len()).step_by(188) {
    let mut looped = data[..cut].to_vec();
    looped.extend_from_slice(data);
    // ... Import::new(...).decode(&looped) must not error
}
```

Two distinct failures, both on `main`:

- wrap at packet 21: `Opus access unit exceeds PES payload` (the `ensure!` above, a PES cut short of the length its control header promises)
- wrap at packet 22: `invalid Opus control header sync (0x7d6a)` (the post-splice bytes are not a control header)

##### Why it is not fixed in #2823

That PR stops a continuity break from handing a spliced buffer to any codec, and excludes Opus from the salvage path so it adds no new abort. Neither addresses this, because the truncated PES arrives through the ordinary `handle_pes_start` flush with continuity intact.

Making the first case graceful is a few lines (drop the truncated tail rather than erroring). The second is the harder half: it needs what #2751 built for the legacy codecs, a scan to the next plausible boundary with confirmation before publishing, and Opus has none of that machinery. Doing only the first leaves a half-fix that the reproduction above still fails, which is why #2823 ships neither.

##### Worth deciding alongside

The legacy path deliberately keeps a byte budget so that a PID whose payload never parses still fails loudly rather than publishing nothing forever. Opus should not simply swallow every parse error, or a PMT that mislabels a PID as Opus becomes a silent dead track. The budget in `Resync` is the precedent to follow.

## Closes

- [#2849](https://github.com/moq-dev/moq/issues/2849) - close this issue when the quest finishes
