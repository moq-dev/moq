# [L] moq-webrtc runs on upstream str0m

## Goal

moq-webrtc depends on crates.io str0m 0.25 and its DTLS, with rind as the
crypto provider through the published `rind-str0m` and `rind-dimpl`
adapters, and `rs/moq-webrtc/src/vendor/` is gone: about 98,600 vendored
lines and 1.6 MB of packet captures. The WHIP and WHEP gateway behaves as it
does today, an offered data channel is still refused, and `cargo deny` sees
the stack's advisories again.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Return to upstream str0m (rejected: keep the vendored stack and gate
  the cut on a browser test, which the audit recommended; keep as is). This
  reverses the 2026-10-30 decision to own the stack
  ([the WebRTC epic](/quest/m3/webrtc/README.md)). What changed since: none
  of the four fixes that motivated vendoring has landed; peer-to-peer and
  data channels left on 2026-11-14
  ([moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771)), so the
  stack serves one WHIP and WHEP gateway; upstream 0.25 has the pluggable
  crypto provider the rind port needed; and nobody merges upstream fixes
  into a vendored copy. The vendored modules are private, so the switch
  breaks no caller.

The choice is unverified: nobody has built moq-webrtc against 0.25. So the
PR starts by finding out, and reports before it deletes.

Facts found by the audit:

- Vendored size, by `find rs/moq-webrtc/src/vendor -name '*.rs' | xargs wc
  -l`: 98,582 lines (str0m 62,368; dimpl 23,998; `is` 8,962; netem 1,467;
  `str0m_proto` 1,360; `str0m_rind` 372). Hand-written moq-webrtc is 6,997
  lines.
- The vendored revision is str0m "0.24.1+" at `dc317173` and dimpl 0.9.0
  (`rs/moq-webrtc/src/vendor/mod.rs:14-20`). crates.io has str0m 0.25.0,
  dimpl 0.9.0, `rind-str0m` 0.25.2, and `rind-dimpl` 0.9.3
  (`cargo search`, 2026-11-16).
- Six commits touched the vendored tree
  (`git log --first-parent -- rs/moq-webrtc/src/vendor`): the vendoring and
  trim ([moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163)), the
  move to rind (#8229, #8248), a doc sweep (#8392, #8608), and the SCTP cut
  (#8771). None is one of the four planned fixes; their quests are all open.
- moq-webrtc reaches the stack through `crate::vendor::str0m` in 13 files
  outside `vendor/` (`grep -rln 'vendor::str0m' rs/moq-webrtc/src`).
- The workarounds vendoring was meant to remove are still in moq-webrtc and
  work against upstream as written: the `b=TIAS` splice
  (`rs/moq-webrtc/src/sdp.rs:49`), the early DTLS failure reported as a
  timeout ([moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379)),
  and eleven `into_std()` clock conversions.

For the PR:

1. Build against str0m 0.25 with the two adapters on a branch that keeps
   `vendor/` in place, and write down what differs. Check at least:
   - rind: the adapters must target the rind the workspace pins.
     [rind 0.4](/quest/m0/rind-adapters.md) is about to move that pin, and
     its Non-goal says the vendored providers "depend on neither rind-dimpl
     nor rind-str0m"; after this quest they do, so land after it or carry
     the adapter bump here.
   - Data channels: the vendored copy cut SCTP and answers an offered
     `m=application` rejected (the test at `rs/moq-webrtc/src/sdp.rs:117`).
     Upstream compiles data channels in, so find how 0.25 refuses one (a
     feature or a config) and keep that test green.
   - DTLS 1.2 only: the vendored dimpl dropped 1.3 and version
     auto-selection. Pin 1.2 through configuration, or record that 1.3 is
     now offered and that a browser run covered it.
   - The typed DTLS timeout: the vendored `dtls.rs` carries dimpl's error
     through where upstream flattened it to a string
     ([its quest](/quest/m3/webrtc/dtls-timeout.md)). See what 0.25 returns.
   - Anything str0m's API renamed between `dc317173` and 0.25.0.
2. If something the gateway needs cannot be had from upstream, stop and
   report it on the PR with the evidence, for the maintainer to weigh
   against the decision, before deleting anything.
3. Otherwise switch those imports, delete `vendor/` with its `LICENSE-*`
   files and the `exclude` in `rs/moq-webrtc/Cargo.toml:15`, and drop the
   module's lint allows. Run moq-webrtc's own tests, moq-cli's WHIP and WHEP
   tests, and one manual Chrome publish and play, since
   [no test drives a browser yet](/quest/m3/whip-whep-browser.md).
4. Leave the quest tree accurate. Delete the quests that exist only for the
   vendored copy: [the vendored stack's sync](/quest/m3/vendor-sync/webrtc-stack.md)
   (with its paragraph in `quest/m3/vendor-sync/README.md`),
   [kio clock time](/quest/m4/webrtc-clock.md),
   [the dead-API trim](/quest/m4/webrtc-dead-api.md), and
   [the browser-only trim](/quest/m4/webrtc-browser-only.md), and the
   vendored-tree line in `quest/m4/test-inventory-followups.md`. The four
   fixes become work at moq-webrtc's boundary or upstream reports; their
   quests already say so ([DTLS waits for ICE](/quest/m3/webrtc/dtls-after-ice.md),
   [the typed DTLS timeout](/quest/m3/webrtc/dtls-timeout.md),
   [several bandwidth lines](/quest/m4/webrtc-sdp-bandwidth.md),
   [WHEP pacing](/quest/m3/rtc-pacing.md)).

Docs: `doc/lib/rs` and `rs/moq-webrtc/README.md` where they say the stack
is vendored.

Public API: none (`mod vendor` is private). Dependencies: str0m, dimpl,
`rind-str0m`, and `rind-dimpl` join the published graph. Wire: none intended
toward browsers; record any change step 1 finds.

## Related

- [MoQ builds against rind 0.4](/quest/m0/rind-adapters.md) - moves the rind pin the adapters must match, and edits vendored files this deletes
- [moq-webrtc's server binds at startup](/quest/m2/webrtc-bind.md) - changes the same crate's server
