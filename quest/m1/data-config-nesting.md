# [XS] Docs nest a data config instead of flattening it

## Goal

The docs and doctests for application data sections nest `BinaryConfig` or
`JsonConfig` in a `config` field instead of `#[serde(flatten)]`ing it, so a
field added to either config later can never collide with an application's own
fields.

```rust
struct Mavlink {
	config: BinaryConfig,
	sysid: u8,
}
```

## Plan

Requested by an external consumer (OneTooMany, Discord). No code flattens
these configs today; only docs and tests do. The built-in `json`/`binary`
sections serialize the config directly as the map value, so their wire shape
is unchanged. An application section becomes
`{"config":{"mode":...},"sysid":1}`.

Update:

- `doc/lib/rs/moq-mux.md` (the `Mavlink` example, its `AsMut` impl returning
  `&mut self.config`, and the `catalog::Entry::new(name, &entry.config)` line)
- `doc/concept/hang.md` (the "flattening the JSON or binary entry" prose)
- the `RenditionConfig` doctest in `rs/moq-mux/src/catalog/tracks.rs`
- `a_data_config_flattens_into_an_application_entry` in
  `rs/hang/src/catalog/root.rs` (rename; the `extra` assertion no longer
  applies) and `mod section` in `rs/moq-mux/src/binary.rs`

`doc/lib/rs/hang.md`'s flatten of a whole extension into `Catalog<E>` is a
different pattern and stays.

## Related

- [Robot teleoperation](/quest/m2/teleop/robot.md) - decides where robot fields ride in the catalog
