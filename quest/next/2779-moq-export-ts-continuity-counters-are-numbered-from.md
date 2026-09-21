# [M] moq export ts: continuity counters are numbered from process state, so two exporters of the same broadcast emit streams that can never be compared

## Goal

Implement and verify the behavior tracked in [#2779](https://github.com/moq-dev/moq/issues/2779)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Two `moq export ts` processes subscribed to the same broadcast reconstruct the same programme, but number each PID's continuity counter from their own state. If they did not start together the two outputs are offset by a constant  -  measured at **+2 on the video PID and +8 on PAT/PMT**, unchanging across a 60 s run  -  so every transport packet differs in byte 3 while the payload underneath is identical.

Each output is internally correct: TSDuck reports 0 continuity errors on either. The divergence is only visible when the two are compared, which is exactly what a redundant pair does.

Measured on `moq-cli` 0.9.9 / `moq-relay` 0.14.9 release binaries, `moq-lite-05`.

#### Why it matters

SMPTE ST 2022-7 seamless protection switching requires the two legs of a 1+1 pair to be *packet-identical*, so a receiver can merge them by RTP sequence number and take whichever leg delivered each packet. This is how broadcast primary distribution survives the loss of a whole path without a visible artefact.

With continuity numbered per process, a MoQ-fed 1+1 pair works only while both legs have run continuously since the broadcast started. The moment one leg is restarted for maintenance, or recovers from an outage that outlived its subscription, it comes back carrying the right programme in the right place  -  and byte 3 of every packet disagrees with its partner. A receiver cannot repair this: rewriting a continuity counter at the receiver means changing a field the packet's own consistency depends on, in a stream it is merging from two sources.

Measured with both legs groomed to a constant bitrate and all packet placement derived from stream position, so that everything except this field is a function of the broadcast:

| Cell | Datagrams identical | Identical with CC masked |
|---|---|---|
| legs started together | 100.00 % | 100.00 % |
| leg B joins 20 s late | 0.09 % | 97.10 % |
| leg A blacked out 15 s, then recovers | 68.56 % | 98.22 % |

The pair is otherwise aligned: same slots, same RTP sequence numbers, same payloads, and a leg that joined 20 s late sends each shared sequence number a median of 10 ms from its partner. The continuity counter is the whole of the remaining difference.

#### Reproduction

Two independent chains from one source  -  `tee` a CBR TS into two `moq import ts` publishers, each to its own relay  -  then two `moq export ts` subscribers, the second started 20 s after the first. Compare the two outputs packet by packet at the same stream position, first raw and then with byte 3's low nibble masked. The masked comparison is the whole argument: the payloads agree, the counters do not.

#### Suggested direction

Derive each PID's continuity counter from stream position rather than from how many packets *this process* has emitted for that PID  -  for instance from the group sequence number and the packet's index within the reconstructed group, so any two exporters of the same broadcast agree without having to co-ordinate, and a subscriber joining mid-stream lands on the same numbering as one that has been running since the start.

The same argument applies to any other field the exporter mints per process rather than deriving from the stream.

#### What this is not asking for

Not a wire-format change, and not co-ordination between exporters. Only that a value carried in the emitted TS be a function of the broadcast rather than of the process that happened to render it.

## Closes

- [#2779](https://github.com/moq-dev/moq/issues/2779) - close this issue when the quest finishes
