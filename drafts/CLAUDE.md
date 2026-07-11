# CLAUDE.md

Guidance for AI coding agents working on the IETF Internet-Drafts under `drafts/`.

## Overview

These are the IETF Internet-Draft specifications for Media over QUIC (MoQ),
authored by Luke Curley. Each `draft-lcurley-*.md` is a standalone draft in
[kramdown-rfc](https://github.com/cabo/kramdown-rfc) markdown with YAML
frontmatter. This is a standards-documentation area, not a software
implementation: the wire protocol and formats specified here are implemented by
the Rust and JS code elsewhere in the monorepo (see the Cross-Package Sync
table in the root `CLAUDE.md`).

Current drafts include `draft-lcurley-moq-lite` (the simplified MoQ transport),
`draft-lcurley-moq-hang` (the media layer), and extension drafts
(`-moq-timestamp`, `-moq-relay-hops`, `-compressed-mp4`, ...). Run
`just drafts` to list them.

## Build and publish

The toolchain (`kramdown-rfc`, `xml2rfc`, `mmark`) is provided by the nix dev
shell, so run recipes through it for reproducible tool versions:

```bash
# List drafts
nix develop --command just drafts

# Render one draft to <name>.txt + <name>.html (gitignored editor's copy)
nix develop --command just drafts build draft-lcurley-moq-lite

# Validate that every draft still parses
nix develop --command just drafts check

# Submit a version to the IETF datatracker (emails you a confirmation link)
nix develop --command just drafts publish draft-lcurley-moq-lite 05 you@example.com
```

Publishing is deliberate and local: `publish` builds `<name>-<version>.xml` and
POSTs it to the datatracker submission API. The datatracker emails the submitter
a confirmation link, and the version is not final until that link is clicked.
There is no CI tag-trigger and no API secret. For a brand-new draft (`-00`), set
"Replaces" on the datatracker confirmation page.

`kramdown-rfc` fetches bibxml references into `.refcache/` on first build, so
the initial build needs network access.

## Conventions

- Draft sources are kramdown-rfc markdown; `remark` (the repo's CommonMark
  linter) skips them. A successful `kramdown-rfc` run also validates syntax.
- The `docname` frontmatter field ends in `-latest`; `publish` rewrites it to
  the versioned name at submission time. Don't hardcode a version in the source.
- When making a wire-format or semantic change to a draft, add a bullet to its
  changelog appendix (e.g. `# Appendix A: Changelog`) under the in-progress
  version's section. Drafts without a changelog section (typically unreleased
  ones) don't need one. Keep bullets concise and factual: list what changed,
  not the motivation or design reasoning.
- Follow IETF contribution guidelines (BCP 78/79); see `CONTRIBUTING.md`.
