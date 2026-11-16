# moq-webrtc is a gateway on upstream str0m

## Goal

`moq-webrtc` is one published crate holding the WHIP and WHEP gateway and
its ICE driver, on crates.io str0m and its DTLS with rind as the crypto
provider. The places str0m does not fit are handled at moq-webrtc's
boundary or fixed upstream, and each is written down: the DTLS handshake
racing ICE, the DTLS timeout's type, one `b=` line per media section, and
retransmits that cannot be charged to a rate cap.

Non-goals: TURN; DTLS 1.3 as a requirement; data channels; a fork or a
vendored copy before a fix has been shown to need one.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅
return to upstream str0m (rejected: keep the vendored stack and gate the cut
on a browser test; keep as is). This reverses the 2026-10-30 decision below,
and [the switch](/quest/m2/str0m-upstream.md) lands before the cut. What
changed: none of the four fixes that motivated vendoring had landed by the
audit, peer-to-peer and data channels left on 2026-11-14
([moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771)), upstream
0.25 has the pluggable crypto provider, and a vendored copy of about 98,600
lines gets no upstream fix unless someone ports it.

History: decided 2026-10-30 (maintainer, planning interview) to vendor
str0m, dimpl, and `is` into private modules, after the workarounds piled
up: one `b=` line per SDP media section
([moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373)), DTLS
retransmits running out before ICE connects
([moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379)), no hook to
charge or serve NACK resends ([rtc-pacing](/quest/m3/rtc-pacing.md)), and
`std::time::Instant` in a `kio::clock` workspace. Vendoring landed on
2026-11-07 ([moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163))
with a trim of what MoQ did not use.

What still holds from that plan:

- **One crate.** The ICE driver lives in `moq-webrtc`, which depends on
  moq-tokio rather than the reverse, so moq-tokio needs no ICE.
- **No SCTP.** Decided 2026-11-14 (maintainer): an offered `m=application`
  line is answered rejected. On upstream this is a configuration, not a
  deletion; the switch keeps the test that proves it.
- **DTLS 1.2** is what the rind provider offers and every browser
  negotiates today.

What changes for the four fixes: each is first tried through str0m's public
API. One that needs a change inside str0m becomes an upstream report, filed
only once the maintainer approves it
([upstream reports](/quest/m4/upstream-reports.md)), and its workaround
stays until upstream ships. Vendoring again is the maintainer's call, made
on evidence that a needed fix cannot land any other way.

Docs a change makes stale (crate names, `doc/lib/rs`,
`doc/setup/upgrade.md`, other quests naming a crate or module) change in the
PR that makes them stale.

## Related

- [moq-webrtc runs on upstream str0m](/quest/m2/str0m-upstream.md) - deletes the vendored stack before the cut
- [moq-webrtc paces WHEP viewers](/quest/m3/rtc-pacing.md) - needs retransmits charged to the cap, which upstream has no hook for
