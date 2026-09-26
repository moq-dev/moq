The IETF Internet-Drafts for MoQ. Each `draft-lcurley-*.md` is kramdown-rfc markdown with YAML frontmatter. The drafts are the normative spec that `rs/moq-net` and `js/net` implement, so a wire change lands with its draft update.

# Recipes

```bash
just drafts                                   # list drafts
just drafts build draft-lcurley-moq-lite      # render to .txt + .html (gitignored)
just drafts check                             # every draft still parses
just drafts publish <name> <version> <email>  # submit to the datatracker
```

`publish` POSTs to the datatracker; the version is final only after clicking the emailed confirmation link. For a new draft, set "Replaces" on the confirmation page. The first build needs network access to fetch bibxml into `.refcache/`.

# Conventions

- Brevity is the rule. State each rule once, normatively, where it belongs, and cross-reference instead of restating. Cut derivable consequences and motivation beyond a sentence. Prefer deleting a sentence over qualifying it.
- `docname` ends in `-latest`; `publish` rewrites it. Never hardcode a version in the source.
- A wire-format or semantic change adds a bullet to the draft's changelog appendix under the in-progress version. List what changed, not why.
- Don't add a section for an unpublished next version; edit the in-progress one.
- `remark` skips these files; a successful `kramdown-rfc` run is the syntax check.
- Follow IETF contribution rules (BCP 78/79), see `CONTRIBUTING.md` here.

# Documentation site

`doc/.vitepress/drafts.ts` translates each draft into a page under `/draft/` on doc.moq.dev. The sources stay canonical. kramdown-rfc is not CommonMark, so the translator has a case for each construct we use. A new construct needs a translator case in the same PR; `bun run --cwd doc check` renders every page and catches a table degrading into a paragraph.
