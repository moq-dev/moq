# A model of video on a lossy stream

Experimental, like the forward error correction it was written for
([moq-quic's README](../README.md#forward-error-correction-experimental)).

`model.py` is a discrete-event model of live video on one reliable QUIC
stream over a lossy path, with repair packets and without. It is not
moq-quic. It has RFC 9002's loss detection (packet threshold, time
threshold, PTO), RFC 9000's delayed ACKs, a paced sender, and no congestion
controller, in a few hundred lines, so a repair design can be compared with
others before any is built into the transport. It also serves as a second
opinion on the transport's own measurements (`just rs quic-fec`), with
which it shares no code.

    python3 model.py                 # the default sweep, a few minutes
    python3 model.py --rtt 60 --links "2% single" --codes none frame:0.2
    python3 model.py --help

## What it models

- 30 frames a second of about 14 packets each (4 Mbit/s), written to one
  stream, so a frame waits for every frame before it. `alone` columns drop
  that wait, as if every frame had its own stream.
- A path that is clear or fading for exponentially distributed spells, in
  time rather than packets, so every sender meets the same fades however it
  paces. A fade of 0.25 ms is one packet at the default pacing.
- A sender that paces at `--pace` (20 Mbit/s by default, five times the
  video), retransmits what loss detection declares lost, and never learns
  what repairs rebuilt, so it retransmits that too (`wasted`).
- Ideal codes: a block of `k` packets and `r` repairs decodes once any `k`
  arrive, and a sliding-window code whenever its equations have full rank.

A frame is `late` when it completes more than a frame interval after the
path's one-way delay: the next frame was due first.

## Codes

| Name | What is sent |
|---|---|
| `none` | Nothing: retransmission alone |
| `block:K:1:flush` | An XOR repair after every `K` packets and at each frame's end |
| `interleave:M:K` | `M` XOR blocks filled in turn, each closed at a frame's end |
| `frame:R` | At each frame's end, repairs for it: `R` of its packets, at least one |
| `overlap:R:N` | As `frame:R`, but each block covers the last `N` frames |
| `sliding:N:W:T` | A repair every `N` packets over the last `W`, and `T` more at a frame's end |

## Paths

The `single` paths lose one packet at a time. The `spells` paths lose
everything for a few milliseconds. The named ones are a synthesis, not a
fit: no published Gilbert-Elliott fit to Wi-Fi or cellular packet traces
for real-time video was found, so these are built from reported loss rates
and burst lengths (cloud gaming over Wi-Fi at 1.15% loss, NSDI 2025; Wi-Fi
roaming stalls of about 55 ms, an Aalborg University thesis; cellular
handover interruptions, SIGCOMM 2022), converted from per-packet chains at 417 packets a second.

| Path | Clear, mean | Fade, mean | Loss in a fade | Loss when clear |
|---|---|---|---|---|
| wifi contended | 800 ms | 16 ms | 60% | 0.1% |
| cellular mobile | 3 s | 24 ms | 80% | 0.02% |
| wifi roam | 60 s | 55 ms | 100% | 0.05% |

## Findings

Frames late, of 20,000, at 20 Mbit/s pacing:

| Round trip | Path | none | `frame:0.2` | `frame:0.3` | `overlap:0.3:2` | `sliding:7:28:2` |
|---|---|---|---|---|---|---|
| 20 ms | 2% single | 2.54% | 0.04% | 0.01% | 0.01% | 0.03% |
| 60 ms | 1% single | 15.2% | 0.26% | 0.04% | 0.03% | 0.11% |
| 60 ms | 2% single | 28.8% | 0.68% | 0.11% | 0.10% | 0.32% |
| 60 ms | 5% single | 58.7% | 3.20% | 0.85% | 0.76% | 1.69% |
| 150 ms | 2% single | 64.8% | 2.45% | 0.40% | 0.19% | 0.34% |
| 60 ms | 2% in 2 ms spells | 11.3% | 6.02% | 4.45% | 3.79% | 4.67% |
| 60 ms | 2% in 8 ms spells | 6.75% | 5.90% | 5.51% | 4.81% | 5.04% |
| 60 ms | wifi contended | 6.38% | 3.64% | 3.19% | 2.66% | 2.87% |
| 60 ms | cellular mobile | 2.31% | 1.76% | 1.67% | 1.48% | 1.53% |
| 60 ms | wifi roam | 0.92% | 0.10% | 0.10% | 0.09% | 0.09% |

1. Against scattered loss, repairs at each frame's end turn most late
   frames into on-time ones, more so the longer the round trip, because a
   retransmission costs a round trip and a half and a repair costs nothing.
   20% overhead removes 97 to 98% of late frames at 1 to 2% loss.
2. Against fades, the same repairs do little. A frame's packets leave
   within 7 ms of each other and its repairs right behind them, so a fade
   takes both. Covering each frame again a frame later (`overlap`,
   `sliding`) helps a little. This is a limit of repairs sent with their
   data, not of streams: datagrams with the same repairs lose the same
   frames.
3. Spreading each frame over 17 ms (`--pace 8`) lets 30% repairs cut the
   2 ms fades to 2.7% late, from 4.5%, at 10 ms more delay on every frame.
   It does nothing for 8 ms fades.
4. One XOR repair per block is the weakest for its cost: `block:7:1:flush`
   at 17% overhead leaves 6.5% late on 2% scattered loss, where `frame:0.2`
   leaves 0.7%.
5. At a 20 ms round trip only 1 to 9% of frames are late without repairs,
   since a retransmission lands within a frame interval or so. Repairs
   matter on long paths.
6. A sender that never learns what was rebuilt retransmits it anyway: 90%
   or more of its retransmissions are wasted on scattered loss, which is
   about the path's loss rate in extra bytes.

`--pace` must leave room for the repairs: at 5 Mbit/s the video, 20%
repairs, and retransmissions no longer fit, and every frame is late. Repair
needs headroom, which is RFC 6363's rule that it adapts with the source.
