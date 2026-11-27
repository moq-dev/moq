# MoQ Internet-Drafts

The IETF Internet-Draft specifications for Media over QUIC (MoQ), in
[kramdown-rfc](https://github.com/cabo/kramdown-rfc) markdown. Each
`draft-lcurley-*.md` is a standalone draft; the protocols and formats they
specify are implemented by the Rust and JS code elsewhere in this repo.

## Building

The toolchain comes from the nix dev shell, so no submodule or venv bootstrap
is needed.

```bash
# List the drafts
nix develop --command just drafts

# Render one draft to <name>.txt and <name>.html
nix develop --command just drafts build draft-lcurley-moq-lite

# Render all of them
nix develop --command just drafts all
```

The rendered `.txt`/`.html` are gitignored; the canonical rendered copies live
on the [IETF datatracker](https://datatracker.ietf.org/).

## Publishing a new version

```bash
nix develop --command just drafts publish draft-lcurley-moq-lite 05 you@example.com
```

This builds `draft-lcurley-moq-lite-05.xml` and submits it to the datatracker,
which emails you a confirmation link. The submission is final only once you
click that link. For a brand-new draft (`-00`), set "Replaces" on the
confirmation page.

The script uploads the source as it is and refuses a heading that still ends
in `(in progress)`. Before publishing, close the changelog heading for the
version you submit and commit it as the record of the publish. Open the next
version's section only after the upload.

## Allocated Extension Code Points

All provisional MOQT code points stay outside the Mandatory Track Property range (`0x4000`–`0x7FFF`) and avoid MOQT GREASE values (`0x7F * N + 0x9D`). Even values are bare varints; odd values are length-prefixed byte strings.

| Registry | Value | Name | Draft |
|:---------|:------|:-----|:------|
| MOQT Setup Options | `0x40B54` | `HOP_ID` | `draft-lcurley-moq-cluster` |
| MOQT Setup Options | `0x40B56` | `RELAY_COST` | `draft-lcurley-moq-cluster` |
| MOQT Message Parameters | `0x40B57` | `HOP_PATH` | `draft-lcurley-moq-cluster` |
| MOQT Message Parameters | `0x40B58` | `ROUTE_COST` | `draft-lcurley-moq-cluster` |
| MOQT Setup Options | `0x40B5A` | `SOLICIT` | `draft-lcurley-moq-solicit` |
| MOQT Setup Options | `0x40B5C` | `HIDDEN` | `draft-lcurley-moq-hidden` |
| MOQT Message Parameters | `0x40B5E` | `HIDDEN` | `draft-lcurley-moq-hidden` |
| MOQT Setup Options | `0x40B64` | `ACTIVE_COUNT` | `draft-lcurley-moq-active-count` |
| MOQT Message Parameters | `0x40B66` | `ACTIVE_COUNT` | `draft-lcurley-moq-active-count` |
| MOQT Setup Options | `0x40B6A` | `FLATE` | `draft-lcurley-moq-flate` |
| MOQ Properties (Track) | `0x40B6C` | `COMPRESSION` | `draft-lcurley-moq-flate` |
| MOQ Properties (Object) | `0x40B6E` | `INFLATED_SIZE` | `draft-lcurley-moq-flate` |
| moq-lite Setup Parameters | `0x49455446` | `IETF` | `draft-lcurley-moq-ietf` |

## Contributing

All contributions are made under the IETF Standards Process; see
[`CONTRIBUTING.md`](CONTRIBUTING.md).
