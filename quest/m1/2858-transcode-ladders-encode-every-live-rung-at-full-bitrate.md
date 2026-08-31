# [XL] Transcode ladders encode every live rung at full bitrate over one connection

## Goal

Implement and verify the behavior tracked in [#2858](https://github.com/moq-dev/moq/issues/2858)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

### Congestion-aware transcode ladders

Follow-up to [#2854](https://github.com/moq-dev/moq/pull/2854), which divides a connection's send estimate among the tracks sharing it.

#### Problem

`moq-transcode` creates one encoder per demanded rung, opens each at its configured maximum bitrate, and publishes every rung over the same first-mile connection. It does not receive a bandwidth estimate or call live encoder rate control, so congestion makes every active rung continue producing at its ceiling.

Simply assigning every rung an allocator share is not enough. A ladder also needs coherent answers for:

- how far a rendition may adapt before it becomes worse than the next lower rendition;
- how the catalog tells players to switch;
- how lower renditions outrank higher renditions without overriding subscriber priority;
- how FETCH, idle rungs, legacy players, and encoders without live rate control behave; and
- which catalog changes actually require a decoder rebuild.

This issue is the umbrella for the policy and its staged implementation.

#### Resolved semantics

##### One ladder controller per output bandwidth domain

One central controller owns all generated rungs for one dedicated publisher uplink. The controller receives an optional connection bandwidth consumer and subdivides it across the ladder.

- Supplying no bandwidth consumer preserves today's fixed-rate behavior and never publishes congestion-induced `stalled` state.
- Only demanded rungs consume allocation. An idle rung remains known to the controller so its catalog state can recover without periodically encoding probe traffic.
- The built-in CLI passes its publisher session's send-bandwidth handle. This is first-mile adaptation, not per-viewer adaptation.
- Sharing one output broadcast across several independently constrained output sessions is outside this model because one encoder and one catalog bit cannot represent per-session state.

##### Canonical ladder order and priority

Resolve renditions into strictly ascending configured maximum bitrate. Reject duplicate ceilings and configurations whose coded resolutions decrease as bitrate increases when dimensions are available.

The lower configured maximum receives the higher publisher-side transmission priority. Subscriber priority remains primary; rendition order only breaks ties among subscriptions at the same subscriber priority.

This requires scheduler work beyond #2854. That PR uses publisher priority for allocation but explicitly does not yet align local transmission order with it.

##### Adaptive bands

`VideoConfig.bitrate` remains the configured maximum bitrate. It does not follow the instantaneous encoder target.

For a rendition with configured maximum `max` and the next lower rendition's configured maximum `lower`, define:

```text
stall = (max + 2 * lower) / 3
```

The lowest rendition uses `lower = 0`, so its stall boundary is `max / 3`.

A live-rate-control encoder may adapt within `[stall, max]`. When its allocation reaches the boundary, clamp the encoder at the boundary and publish `stalled: true`. Existing and direct subscribers may continue receiving it, but an updated player should move to a lower rendition. Clear `stalled` only after a target above the same boundary is successfully applied.

Start with the existing rate controller's 5 percent movement hysteresis, immediate decreases, and gradual upward ramp. Do not add a second re-entry threshold or dwell timer until measurements show that catalog state flaps.

Catalog state must follow the last target successfully accepted by the encoder, not merely the controller's requested target. Transient rate-control failures retain the last applied target and retry on a later material control movement.

##### Encoders without live rate control

`BitrateUnsupported` is an explicit best-effort fallback:

- continue encoding at the configured maximum;
- publish `stalled: true` whenever the allocation is below that fixed maximum;
- clear it only when the full configured maximum fits again; and
- rely on transmission priority and transport shedding to protect lower renditions.

This preserves existing and legacy subscribers but deliberately does not reclaim encoder work. It must be visible in logs and tests rather than silently pretending the requested target was applied.

##### Catalog signal

Add optional `stalled: boolean` state to each HANG video rendition and the equivalent optional field to MSF video track entries.

- Missing and `false` mean selectable.
- `true` means the publisher's first-mile allocation cannot currently sustain this rendition's adaptive band.
- It is advisory. The media track remains addressable and existing subscriptions are not closed merely because the bit changes.
- It is current state only. Cumulative stall counts and durations belong in stats telemetry, tracked by [#2734](https://github.com/moq-dev/moq/issues/2734).
- Publisher-originated rendition stall and receiver-observed playback stall must remain distinguishable in QoS metrics.

The HANG draft, Rust and JavaScript schemas, MSF Rust and JavaScript schemas, generated/public catalog bindings, and user-facing catalog documentation must remain synchronized. MSF should document this as a non-standard extension unless it is adopted by the upstream draft. Do not grow `moq_video_config` in place if doing so would break its C ABI; expose the state through an additive accessor or a versioned shape.

Catalog mutations currently publish full HANG, HANGZ, and MSF snapshots. Coalesce all rung state changes from one controller iteration into one publication. Later source catalog snapshots must compose with, rather than overwrite, current generated-rung state.

A stalled rung that loses all updated-player demand must not become permanently stalled merely because it no longer receives an allocator share. The central controller should evaluate that idle rung's hypothetical share against the current connection estimate and active lower-priority reservations, without making the idle rung consume real allocation or encode probe traffic.

##### Player selection and decoder lifecycle

Updated players exclude stalled renditions whenever at least one decoder-supported unstalled rendition exists. If every decoder-supported rendition is stalled, select the lowest one. This keeps video alive while allowing an application to inspect the catalog and disable video according to its own policy.

Separate three reactive identities in `@moq/watch`:

1. Routing identity: resolved broadcast plus track name. A change requires a subscription handoff.
2. Decoder/pipeline identity: container and only the effective inputs actually needed to construct the demuxer and configure WebCodecs, such as effective codec, description/init data, and latency mode.
3. Selection and presentation metadata: bitrate, stalled state, coded/display dimensions, framerate, jitter, and timeline.

Metadata-only changes must not create a second `DecoderTrack`, resubscribe locally, rebuild WebCodecs, or rerun codec support probing unnecessarily. Dimension changes are handled as selection/presentation metadata rather than decoder identity.

The existing make-before-break handoff remains for a real routing or decoder/pipeline identity change.

##### FETCH

An uncached FETCH uses the rung's current shared applied target at request time and participates in the same allocation. A stalled rung remains manually fetchable. Cache hits remain unaffected.

The FETCH path currently opens a fresh encoder at the configured maximum for every requested group, so it must consume controller state rather than maintain an independent bitrate policy.

#### Delivery plan

Keep this issue as the umbrella and land independently testable PRs.

Phase 1 can target `main` if every public binding change remains additive. If a generated UniFFI record change is source-breaking, that binding portion targets `dev`. Phases 2 and 3 depend on #2854 and should stack on its `dev` line until that allocator work lands.

##### Phase 1: catalog and player semantics

- \[ ] Add optional HANG rendition `stalled` state in Rust and JavaScript.
- \[ ] Update `draft-lcurley-moq-hang.md` and catalog documentation.
- \[ ] Add the equivalent MSF track extension in Rust and JavaScript and document its extension status.
- \[ ] Carry the field through public catalog consumers and language bindings without breaking the C ABI.
- \[ ] Filter stalled renditions with the all-stalled lowest-rendition fallback.
- \[ ] Split routing, decoder/pipeline, and metadata identities in `@moq/watch`.
- \[ ] Prove bitrate, stalled state, and dimension-only updates do not rebuild the decoder or create a new subscription.
- \[ ] Prove real codec, description/init, container, or track changes still perform the required handoff.

##### Phase 2: publisher scheduling

- \[ ] Define canonical rung ordering from configured maximum bitrate and validate custom ladders.
- \[ ] Make lower renditions win the publisher-priority tie-break among equal subscriber priorities.
- \[ ] Preserve subscriber priority as the primary scheduling authority.
- \[ ] Test equal subscriber priority, conflicting subscriber priority, custom ladder order, and rejected ambiguous ladders.

##### Phase 3: transcode controller

- \[ ] Add an optional bandwidth input to the transcode configuration and wire the CLI publisher session into it.
- \[ ] Give one controller ownership of all rung shares, targets, and catalog state.
- \[ ] Apply the weighted boundary formula, including the lowest-rung `max / 3` case.
- \[ ] Track requested and successfully applied targets separately.
- \[ ] Implement and test the `BitrateUnsupported` fallback.
- \[ ] Keep idle rungs allocation-free while allowing catalog recovery without encoder probes.
- \[ ] Apply the shared target and allocation to live and uncached FETCH encoders.
- \[ ] Coalesce catalog publications and prevent source updates from resetting rung state.

#### Acceptance scenarios

- \[ ] One demanded rung can use its configured maximum when the uplink permits it.
- \[ ] Multiple demanded rungs share one uplink, with lower rungs protected before higher rungs at equal subscriber priority.
- \[ ] A supported higher rung adapts down, clamps at its boundary, becomes stalled, and recovers without changing its advertised maximum.
- \[ ] The 5 Mbps / 2.5 Mbps boundary is approximately 3.33 Mbps.
- \[ ] The default 350 kbps lowest rung stalls at approximately 117 kbps.
- \[ ] A stalled catalog update moves an updated player down without rebuilding WebCodecs for metadata alone.
- \[ ] If every rendition is stalled, the updated player continues with the lowest decoder-supported rendition.
- \[ ] A legacy or direct subscriber can continue requesting a stalled track.
- \[ ] An unsupported encoder remains at its fixed maximum, reports stalled below that maximum, and does not recover early.
- \[ ] A fresh FETCH uses the shared applied target; a cache hit performs no new encoding.
- \[ ] No bandwidth input preserves existing fixed-rate catalog and encoding behavior.
- \[ ] One source catalog refresh cannot erase current generated-rung stalled state.

#### Explicit non-goals

- Per-viewer or per-downstream-session rendition state.
- Dynamically rewriting the catalog's advertised maximum bitrate.
- Closing or removing stalled tracks from the catalog.
- An application-level `unselectable` state when every rendition is stalled.
- Stall counters or duration telemetry in the catalog.
- Rebuilding unsupported encoders at every target change.
- Additional stall/recovery dwell timers before real measurements justify them.

## Required

- [Plan: demand-driven ladder bitrate](/quest/m1/plan-ladder-bitrate.md) - split into implementable quests first

## Closes

- [#2858](https://github.com/moq-dev/moq/issues/2858) - close this issue when the quest finishes
