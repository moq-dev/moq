# [M] moq-mux: publish data tracks into an application's own catalog section

## Goal

An application lists a JSON or binary track in its own root section, with
its own per-track fields next to the reading rules, and publishes it with one
`moq-mux` call that handles framing, compression, the entry's lifecycle, and
a detected `bitrate`. JSON and binary entries gain optional `bitrate` and
`jitter`, matching video and audio.

The motivating case is MAVLink telemetry: the application's
`catalog["com.example.mavlink"].tracks[name]` flattens a `BinaryConfig`
(mode, compression, broadcast) beside `sysid`, `compids`, and `dialect`. One
entry per track, so nothing has to be kept in sync with a second listing.

Non-goals: hang defines no MAVLink (or other application) section, and
`BinaryConfig`/`JsonConfig` gain no generic or opaque extension field. A
single `tracks` map holds every data track, so one type parameter would force
every application track kind into one enum. Generic data-track tooling does
not list tracks in an application section; that is the trade for one source of
truth.

## Plan

- **Producers.** `Producer::binary_snapshot`/`binary_stream` and
  `json_snapshot`/`json_stream` accept any entry that implements
  `RenditionConfig<E>` and embeds a `BinaryConfig`/`JsonConfig`, exposed
  through a small trait (a `&mut` accessor to the embedded config). The
  producer fixes `mode`, applies `compression`, and writes the entry into
  whichever section the `RenditionConfig` names; dropping the handle removes
  it. The existing `binary::Config`/`json::Config` builders keep working
  unchanged, so this generalizes a concrete parameter (RFC 1105 minor) and
  lands on `main`. Name the trait by role; `catalog::Entry` is already the
  consumer-side pairing of a name and config.
- **Consumers** need nothing new: `Entry::new(name, &mavlink.binary)` already
  subscribes through `binary::Consumer`.
- **Bitrate/jitter.** Add optional `bitrate` (bits per second) and `jitter`
  (`MillisCeil`, same meaning as video and audio) to `BinaryConfig` and
  `JsonConfig` in `rs/hang`, `js/hang`, and the Binary and JSON sections of
  `drafts/draft-lcurley-moq-hang.md`. Opt the data producers into bitrate
  detection through the embedded config's `Estimate`. `jitter` is set by the
  publisher only here; detection is [data jitter](/quest/m1/data-jitter.md),
  because flush lateness needs a source timestamp the producers do not take
  yet.
- **Draft.** One line: application root sections SHOULD use a namespaced key
  (e.g. reverse-DNS), since `text`, `json`, and `binary` already needed
  lenient decoding for keys applications used first.
- **Docs.** A custom-section example in `doc/lib/rs/moq-mux.md` beside the
  existing data-track one, and the `RenditionConfig` doc example switched to a
  section that embeds a `BinaryConfig`. No new page.
- **Tests.** A custom section round-trips through a producer and a
  `Catalog<E>` consumer, drop removes the entry, a duplicate name is refused,
  and bitrate is detected. The existing `binary::Config` path stays covered.

Public API: additive in `moq-mux` (generalized data producer parameter, one
new trait) and `hang`/`@moq/hang` (two optional fields). Wire: two optional
catalog fields, additive.

## Related

- [Data jitter](/quest/m1/data-jitter.md) - detects the `jitter` this quest adds
- [MAVLink bridge](/quest/m2/teleop/mavlink.md) - the in-repo consumer of the same shape
- [Robot teleoperation primitive](/quest/m2/teleop/robot.md) - its telemetry section embeds data-track configs this way
