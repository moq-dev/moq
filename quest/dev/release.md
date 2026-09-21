# [M] Cut the release moq.pro adopts

## Goal

The first release from the merged tree is the one moq.pro pins: every
binding matches the moq-ffi surface it exposes, an upgrade page walks a
consumer from the last main release to this one, and the merged relay has
run on staging long enough that the origin and HLS rewrites are trusted.
Binding parity gates this release, not the merge.

## Plan

Write `doc/setup/upgrade.md`, one section per package group, each break
with its PR and the replacement call:

- net: announcements are prefix routes (#3225); serving folds into
  `origin::Producer::dynamic` and broadcasts announce themselves (#3400,
  #3581); `Reload`/`Shared` collapse into one `Connection` (#3614, #3636);
  `writeDatagram` is `insertDatagram` (#3666); reader group and frame limits
  are explicit (#3647); groups expire on timestamps alone; an oversized group
  aborts with GROUP_TOO_LARGE instead of shedding its head (#3585); the
  `Latency` type became `max_age` and delivery order became the `Ordered`
  handle (#2688, #2955); the four moq-lite stream codes sent from the
  reserved range moved to 0x36-0x39 in the draft's own range; subscriptions resume
  across routes sharing a first hop (#3312); the send estimate is split among
  JS publishers (#3616); @moq/net and @moq/pattern mirror Rust (`consume`, `Time.Milli`,
  one `readFrame()`, `InvalidPattern`); the announce and request names; and
  moq-tokio's names sit under their modules (`connection::Goaway`, `cli::Duration`,
  `transport::Session`, `watch::Files`, `resolve()`; #3745).
- hang and json: the catalog `timeline` is `archive` (#3612); `json` and
  `binary` catalog sections (#3109) take one options object (#3640); Rust
  `modify()` is fallible and a failed dropped edit aborts the track (#3644),
  paired with JS `update()`/`mutate()` and the Rust `mutate()` from
  [JSON mutate](/quest/next/json-mutate.md); an fMP4 export fragment is a group
  (#3573).
- watch and play: `latency` splits into `delay` and `buffer` and
  `--latency-max` is renamed (#3396); `moq play` has a real playout clock with
  `--delay` (#3528); Firefox hardware encoding and screen-source scaling
  (#3535).
- relay and CLI: embedders own listeners and workers (#3638); the cluster
  origin is constructed once (#3582); LAN discovery is partitioned by
  application (#3621) and meshes CLI and relay peers (#3648); config merges
  with provenance (#3587); auth is one contract, a `Request` in and a
  `Grant` with a lease out, and `--auth-api-mode` is gone (#3688); the CLI parses
  with usage-rs and refuses the flags it dropped (#3030); moq-native is
  moq-tokio (#2896). Released spellings refuse rather than warn or silently
  alias: `--cluster-linger` is gone; `--cluster-connect` needs a full URL;
  TOML `connect`/`failover_delay`/`listen`/`disable_verify`/`[server]`/`[client]`
  name `url`/`race`/`bind`/`insecure`/`[listen]`/`[connect]`; CLI `--origin`/
  `--name`/`--latency-max` and `publish`/`subscribe` name `--hop`/`--broadcast`/
  `--max-age` and `import`/`export`. Unused `#[deprecated]` items are gone.
  JS `announced()` always drops reflected announces (`ignoreSelf` is gone);
  an `oct` JWK without `kty` is refused. The gstmoq properties
  `estimated-send-bitrate`/`estimated-recv-bitrate` are `estimated-*-rate`
  with no alias, a runtime failure for a `gst-launch` line.
- bindings: the Go module is `moq.dev/moq` with `context.Context` on every
  blocking call (#2957); `MoqAudioCodec` is an `opus()` object (#3671); the
  configuration setters are fallible (#3642); durations are microseconds and
  the rate estimates are `estimated_*` (#3744); the decode format knob from
  [Decode format](/quest/next/ffi-decode-format.md).

Release-notes outline, the additions worth leading with, in order of value to
a consumer: prefix routes and wildcard Pattern events (#3225, #3649);
self-announcing broadcasts and `dynamic` handles (#3400, #3581); the archive
catalog and store (#3612); the delay/buffer split and playout clock (#3396,
#3528); GROUP_TOO_LARGE (#3585); explicit reader limits and timestamp-only
expiry (#3647); publish robustness (Firefox hardware encoding, file demux,
`stalled` on lagging renditions #3630, stream resets at boundaries #3580);
relay embedding and the LAN mesh (#3638, #3648, #3621, #3587); one auth
contract with leases (#3688, #3739); data tracks and captions (#3109, #3640); one `Connection` with URL
replacement (#3614, #3636); first-hop resume and the shared send estimate
(#3312, #3616). The six main release API quests settle archive, E2EE, sock, and
uring before release without making the independent Pronto or media tracks a
release prerequisite. The E2EE reshape changes the implemented profile and
derived names/keys; report that interoperability change explicitly. The
[E2EE](/quest/next/e2ee/README.md) questline owns its twin and interop.

The soak bullet below is cleared by hand: the merged relay serves moq.pro
staging with `/metrics` watched and a fresh viewer joining a days-old
`moq import ts` broadcast over HLS at the end. Then cut the release under the
existing release-plz and npm workflows; this quest bumps no versions itself.

Public API: none beyond the required quests. Wire: none.

## Required

- [E2EE API](/quest/main/e2ee-api.md) - expose epoch-scoped ownership and align the implemented profile
- [uring identity](/quest/main/uring-identity.md) - bind sockets, connections, workers, and steering identity together
- [Merge dev](/quest/dev/merge-dev.md) - the tree the release is cut from
- [Binding audio tests](/quest/next/binding-audio-tests.md) - every binding proves the audio config it exposes
- [Decode format](/quest/next/ffi-decode-format.md) - the C-only decode knob reaches every uniffi binding
- [JSON mutate](/quest/next/json-mutate.md) - Rust and JS share the closure edit
- [Binding parity](/quest/next/binding-parity.md) - every wrapper reaches every moq-ffi method
- [Binding docs](/quest/next/binding-docs.md) - the binding pages compile against the wrappers
- The merged relay has soaked on moq.pro staging and the maintainer has signed it off
