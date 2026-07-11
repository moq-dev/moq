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

## Contributing

All contributions are made under the IETF Standards Process; see
[`CONTRIBUTING.md`](CONTRIBUTING.md).
