# [L] On-demand runtime QA hosts

## Goal

An agent can submit an exact checkout to an available Linux or hardware test
host and retrieve results, logs, and symbols through a repeatable command.
Unsupported or inaccessible environments remain explicit verification gaps.

## Plan

Hosted Windows/macOS recipes compile platform code, while
`quest/m3/video-hardware.md` and `quest/m2/dart-ios.md` already own physical
validation work. This quest supplies the reusable access and execution contract,
not another list of codec or device bugs.

- Inventory existing authorized hosts and devices first: OS/architecture,
  kernel/io_uring capability, GPU/driver, media devices, display/session access,
  and debugger/packet-capture permissions. Installed SSH or LLDB is not proof
  that a suitable target is reachable. Do not provision paid infrastructure.
- Implement a host-side job runner plus a local submit/status/cancel/artifacts
  recipe. Address an immutable source snapshot and record its digest; do not
  test whichever branch the remote directory happens to contain.
- Start with one disposable Linux CPU runner for transport/runtime tests. Give
  jobs isolated directories, resource limits, leases, and bounded cleanup.
  Run untrusted PR code without production credentials or persistent access to
  other jobs. Use existing CI or SSH access rather than adding a public service.
- Add one available device profile as proof of extensibility. Keep camera,
  microphone, portal, signing, and interactive-session authorization explicit;
  report unavailable devices rather than silently using a software fallback.
- Support test-owned stack capture and read-only log/metrics/artifact retrieval.
  A separate debug session may retain a failed process with an expiring lease.
  Document the human handoff when a permission prompt or device action is needed.

Acceptance: submit a known SHA, recover its complete evidence, cancel a hung
job, and verify no child process or device lease remains. A mismatched source
digest or absent required backend must fail capability validation. Demonstrate
one real Linux runtime scenario and one available device scenario; if no device
is available, split that rollout into an explicit externally blocked follow-up.

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - owns real encoder/capture/zero-copy verdicts
- [Dart on iOS](/quest/m2/dart-ios.md) - owns simulator/device packaging validation
- [Transport failure drills](/quest/m0/transport-failure-drills.md) - consumes the Linux runner
- [Merge evidence](/quest/m0/merge-verification-evidence.md) - binds remote results to the reviewed source
