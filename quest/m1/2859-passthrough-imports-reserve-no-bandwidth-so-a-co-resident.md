# [M] Passthrough imports reserve no bandwidth, so a co-resident encoder over-targets

## Goal

Implement and verify the behavior tracked in [#2859](https://github.com/moq-dev/moq/issues/2859)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to [#2854](https://github.com/moq-dev/moq/pull/2854), which divides a connection's send estimate among the tracks sharing it.

#### Problem

Only tracks that *encode* register a reservation today: `moq-video` and `moq-audio`'s `publish_capture`. A passthrough import (`moq import rtmp`, `srt`, `hls`) republishes media that arrived already encoded, and those tracks are minted by the container importers, which register nothing.

So a capture publish and an rtmp import sharing one connection are invisible to each other: the capture encoder targets the whole uplink while the rtmp stream independently consumes a chunk of it, and the encoder over-targets by exactly the rtmp stream's bitrate. It's [#2815](https://github.com/moq-dev/moq/issues/2815) one layer up, and [#2809](https://github.com/moq-dev/moq/pull/2809) is what makes running the two together routine.

#### The wrinkle, and why it's resolvable

A passthrough track has **no configured ceiling**. Nobody here chose its bitrate; the upstream encoder did. The only number available is measured, which normally fails the rule the allocator is built on: reserve the maximum a track can ever send, never what it happens to be sending. A VBR source sitting on a black screen at 1 Mbps can jump to 6 Mbps between one frame and the next, and a reservation that had followed it down would already have handed that room to somebody else.

`moq_mux::catalog::Estimate::bitrate` is the exception. It is documented as *the maximum* bitrate, measured over a 1s window, so it's a peak-hold rather than an instantaneous rate, and it satisfies the ceiling rule as-is. The importers already feed it (`catalog::Estimator`), so the number is sitting there.

#### What to build

Register each passthrough track with its catalog-estimated bitrate, in the reserve-only mode `moq-audio` already uses: claim the budget, never follow the grant. A passthrough track can't be asked to back off, so its entry means "subtract this from everyone else's budget" rather than a ceiling anyone will respect. That's the same shape PCM audio needs, so no new mode.

Two known limits, neither fatal:

- **A peak-hold starts at zero.** Until the source's first peak, the reservation understates and a co-resident encoder over-targets by the difference. It converges within seconds, and it is strictly better than the reservation of zero these tracks hold today.
- **The estimate updates as the source's peak grows**, so the reservation ratchets up over the life of the stream and never down. That's the conservative direction, which is the right one here.

Priority falls out of [#2854](https://github.com/moq-dev/moq/pull/2854): the importers now stamp `hang::catalog::PRIORITY` per kind, so an imported audio track already outranks an imported video one.

## Closes

- [#2859](https://github.com/moq-dev/moq/issues/2859) - close this issue when the quest finishes

