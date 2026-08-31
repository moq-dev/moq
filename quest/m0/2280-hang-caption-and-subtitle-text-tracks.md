# [L] hang: caption and subtitle text tracks

## Goal

Implement and verify the behavior tracked in [#2280](https://github.com/moq-dev/moq/issues/2280)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Captions and subtitles: an accessibility and localization baseline that MoQ has no model for at all. Also a place where MoQ is structurally better than the incumbents rather than merely catching up.

In HLS, captions are segment-bound: a caption can't land until its segment does, so live captioning inherits the whole segment latency. In MPEG-TS they're smuggled inside video SEI (CEA-608/708). In MoQ a caption is just **a track with timestamps**, delivered independently, at whatever cadence the source produces. Sub-second live captioning and real-time translation become natural rather than a hack, and per-language tracks are subscribed only when selected.

#### What exists today

**Nothing in the hang catalog.** No text/subtitle/caption/webvtt/cea608/cea708/ttml section, schema, or track type in `rs/hang` or `js/hang`. `hang::catalog::Catalog` (`rs/hang/src/catalog/root.rs:16`) is exactly two sections:

```rust
pub struct Catalog {
    #[serde(default)] pub video: Video,
    #[serde(default)] pub audio: Audio,
}
```

Elsewhere in the tree, captions are modeled-then-dropped or carried-but-opaque:

- **MSF** (the other, IETF catalog) has the roles: `moq_msf::Role::{Caption, Subtitle}` (`rs/moq-msf/src/lib.rs:455-457`), and JS mirrors them (`js/msf/src/catalog.ts:15`  -  `z.enum([..., "caption", "subtitle", "signlanguage"])`). But `rs/moq-mux/src/catalog/msf/consumer.rs:87` **drops** them: "Tracks with no role, with an unsupported role (caption, subtitle, ...)".
- **fMP4 import rejects them**: `rs/moq-mux/src/container/fmp4/import.rs:207`  -  `b"sbtl" => Err(Error::UnsupportedSubtitle)`. MKV drops them (`mkv/import.rs:40`).
- **TS carries teletext/DVB subs verbatim and undecoded** via the `mpegts` section  -  bytes ride through, nothing reads them.
- **Aspirational only**: `doc/concept/use-case/ai.md:38` and `doc/concept/use-case/contribution.md:97` both describe a `captions` track (Whisper) as a use case. The docs promise something that doesn't exist.

So today: a broadcast with captions loses them at every import boundary except opaque TS.

#### Proposed shape

A `text` (or `captions`) root section mirroring `Audio`:

```rust
pub struct Text { pub renditions: BTreeMap<String, TextConfig> }
```

with `TextConfig` reusing the existing per-rendition machinery: `broadcast` (relative refs), `container`, `timeline`, `jitter`, plus `language` (BCP-47) and a `kind` (caption vs subtitle vs description  -  MSF's `Role` already enumerates these; reuse rather than invent).

Format: probably WebVTT cues or a simple typed JSON record. A `moq_json::stream` track is the obvious carrier (lossless, ordered, self-contained records  -  same shape as the timeline track), with each cue carrying its own start/end PTS. Note captions are sparse and bursty, unlike media, so the group/keyframe model needs thought: one cue per frame, one group per... what? Worth deciding explicitly.

**Prototype out of tree as a `CatalogExt` first**, exactly like `ts::catalog::Ext` (`rs/moq-mux/src/container/ts/catalog.rs`), which is the worked example of adding a section: `struct Ext { mpegts: Mpegts }` + `impl CatalogExt for Ext` + a `trait Catalog: CatalogExt` composition hook implemented for `()`/`Extra`/`Ext`. `RootSchema` in JS is a `z.looseObject`, so unknown sections already pass through untouched, and `rs/hang/src/catalog/root.rs:352` (`extension_roundtrip`) shows the serde-flatten pattern.

#### Open questions

1. **Cue format**: WebVTT (browser-native, `TextTrackCue` for free) vs. a typed JSON record (language-agnostic, no parsing) vs. TTML/IMSC (broadcast-grade, heavy). WebVTT for the browser path is tempting but a native consumer then has to parse it. Leaning typed JSON records that trivially render to WebVTT.
2. **CEA-608/708 extraction from SEI.** A huge amount of real content carries captions inside video SEI. Extracting them at import into a real text track is genuinely valuable, and genuinely fiddly. Probably a separate follow-up, but the catalog shape should anticipate it.
3. **Do MSF's `Caption`/`Subtitle` roles map cleanly onto the new section?** They should, so the MSF consumer can stop dropping them.
4. **Group/keyframe semantics for a sparse text track**  -  the container `Producer` requires a keyframe to open a group (`MissingKeyframe`, `rs/moq-mux/src/container/producer.rs:109`). A `moq_json::stream` sidesteps this; a `Container`-based text track wouldn't.
5. **Player UI**: `<moq-watch>` needs to expose track selection and render cues. `js/watch` has no text surface at all today.
6. Live translation and Whisper-style ASR are the interesting downstream use (and the docs already promise them), but they're applications on top of this, not part of it.

#### Branch

`dev` if it adds a field to base `hang::catalog::Catalog`: the struct has pub fields and is **not `#[non_exhaustive]`** (unlike `VideoConfig`/`AudioConfig`/`Timeline`, which are), so a new field breaks struct-literal construction. Adding `#[non_exhaustive]` to `Catalog` is itself breaking and could land first on `dev` as a standalone  -  worth doing regardless, since it's an inconsistency that will keep biting (E2EE hits the same wall on `catalog::Container`).

`main` if it stays an out-of-tree `CatalogExt` prototype.

#### Cross-package sync

`rs/hang` ↔ `js/hang`; `js/watch` (selection + rendering); `rs/moq-mux` (MSF consumer, fMP4/MKV/TS import); `doc/concept`. Catalog format change → `drafts/draft-lcurley-moq-hang.md` in the same PR.

## Closes

- [#2280](https://github.com/moq-dev/moq/issues/2280) - close this issue when the quest finishes
