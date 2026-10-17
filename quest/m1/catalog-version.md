# [M] Minimum reader version in the hang catalog

## Goal

The hang catalog carries a root `version`: the minimum moq-hang draft a
reader needs to read it correctly. A reader refuses a catalog whose
`version` is above the draft it implements, with a typed "unsupported
catalog version" error, instead of misreading it. Unknown fields are still
ignored, and additive changes never raise the number.

Non-goals: MSF catalogs (they carry their own `version`), the recording
format's `archive.version`, and version negotiation: a reader only checks
the number, and nothing is negotiated through track or broadcast names.

## Plan

Decided 2026-10-16 (maintainer interview):

- Named `version`, at the catalog root, mirroring MSF's root `version` and
  HLS `EXT-X-VERSION`. Root only: one number for the whole catalog, never
  per section, so a too-new video section can't be silently dropped into an
  audio-only play.
- The bare key is the spec's to claim: the spec already tells applications
  to namespace their own root keys so they can't collide with a section a
  later draft defines. Reserve `version` the way `rs/moq-msf` reserves its
  root key, so a flattened Rust extension or a JS passthrough field can't
  emit a duplicate.
- The number is the moq-hang draft number (`"version": 4` needs a
  moq-hang-04 reader). A missing field means 3, the catalog `release`
  ships today, so published catalogs stay readable without an update.
- Minimum-reader semantics, computed from the fields emitted: the
  serializer writes the highest draft whose breaking fields this catalog
  actually uses, and omits the field while that is 3. A catalog that only
  uses 03-shaped fields stays readable by older readers.
- A too-new `version` is a hard refusal through a distinct error in Rust
  (`rs/hang`) and JS (`js/hang`), so players and bindings can tell the user
  to upgrade, matching MSF's refusal of an unknown numeric version. Map it
  through moq-ffi and the wrappers per the Cross-Package Sync table.
- The spec gains one section stating both halves: readers MUST ignore
  unknown fields (only implied today), MUST refuse a `version` above what
  they implement, and a change raises the minimum only when a reader that
  ignored it would misread the catalog. Update `doc/concept` to match.
- Lands in m1, before the cut, so the first readers that check ship in the
  next release; published 03 readers ignore the field.

Audit the moq-hang-04 changelog entries that touch the catalog (e.g.
`enabled` replacing `stalled`, the root `clock`, untimed archive tracks) and
record in the spec which ones require `version: 4`; each maps to the field
that triggers it in the serializer. Keep the existing tolerance for
`archive` and the `text`/`json`/`binary` sections.
