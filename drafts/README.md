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
confirmation page. On a 200 or 201, the source gains an empty changelog
section above the one just published. A versioned heading such as
`moq-lite-07` stays put and the new heading ends in the next version. A
`Since name-00 (in progress)` heading is the section for version 01:
publishing drops `(in progress)` and opens `Since name-01 (in progress)`
above it. Commit that edit; it is the record of the publish. Anything short
of a 200 or 201 leaves the file untouched. Publishing the same version again
submits the text without the section that is already open.

## Contributing

All contributions are made under the IETF Standards Process; see
[`CONTRIBUTING.md`](CONTRIBUTING.md).
