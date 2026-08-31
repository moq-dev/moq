# [M] moq-relay: PUBLISH is unimplemented by design - make the rejection fail fast for clients that wait

## Goal

Implement and verify the behavior tracked in [#3076](https://github.com/moq-dev/moq/issues/3076)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: implement item 1 only, a crisp PUBLISH
rejection at every supported draft version. Item 2 (working-group advocacy) is
not repository work and is dropped from this quest.

### Issue context

#### Observation (interop pre-run, 2026-08-26)

Running the interop client matrix against a relay built from the #3025 release, with moq-go's draft-19 client  -  the first client in the registry to exercise PUBLISH:

- The PUBLISH itself is rejected crisply: `moqt request rejected: PUBLISH is not supported (code 0x3)`, in ~263 ms. That rejection is fine and deliberate.
- But the client's announce-wait case then hangs to its own timeout: after the rejected PUBLISH, its `subscribe-before-announce` test waits for the broadcast to appear and dies 8 s later on `context deadline exceeded`.

So interop matrices read "broken relay" when the truth is "deliberate posture, slow failure shape".

<details>
<summary>moq-go client vs moq-relay (#3025 release)  -  TAP excerpt (trimmed)</summary>

```
  Client: ghcr.io/englishm/moq-interop-runner-moq-go-client:latest
TAP version 14
# moq interop-client
1..6
ok 1 - setup-only
ok 2 - announce-only
ok 3 - publish-namespace-done
ok 4 - subscribe-error
not ok 5 - announce-subscribe
  ---
  duration_ms: 263
  message: "publisher PUBLISH: moqt request rejected: PUBLISH is not supported (code 0x3)"
  ...
not ok 6 - subscribe-before-announce
  ---
  duration_ms: 8002
  message: "timeout waiting for announcement: context deadline exceeded"
  ...
```

</details>

#### The posture (paraphrasing Luke's remarks on the MoQ Slack today)

- The relay has never supported PUBLISH at any version  -  this is not a draft-19 gap.
- The WG clarified that PUBLISH is not an implicit PUBLISH\_NAMESPACE, so rejecting it does not take the announce path down with it.
- N-way downstream push has no clear CDN use case: supporting PUBLISH would mean forwarding it to every node in the cluster, burning backbone bandwidth on data nobody asked for. He'd rather the WG adopt "wait for the peer to ask for something", and draws the analogy to HTTP/2 server push  -  which failed and was removed for the same reason.

#### What this issue tracks

1. **Fail fast in moq-relay**: keep rejecting PUBLISH, but make the rejection path crisp at every draft version the relay speaks, so a client that waits on a consequence of its PUBLISH (e.g. the broadcast becoming visible to its own subscription) gets a prompt, unambiguous signal instead of running out a multi-second timeout. The rejection is the feature; the hang is the bug.
2. **Implementation-experience feedback for draft-20 / the Seattle interim**: anchor this thread as a relay/CDN operator position  -  unsolicited-data fan-out is economically unviable at CDN scale, and HTTP/2 server push is the precedent for how that story ends (shipped, unused, removed). The useful protocol posture is pull-based: a relay should be able to decline PUBLISH cleanly, and clients should be able to tell "declined by design" from "broken" without waiting.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## Closes

- [#3076](https://github.com/moq-dev/moq/issues/3076) - close this issue when the quest finishes
