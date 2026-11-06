The IETF drafts (kramdown-rfc markdown) are the normative spec for `rs/moq-net` and `js/net`.

- `just drafts check` validates every draft; `just drafts build <name>` renders one. The first build fetches bibxml over the network.
- Brevity is the rule. State each rule once, normatively, where it belongs, and cross-reference instead of restating. Prefer deleting a sentence over qualifying it.
- `docname` ends in `-latest`; publishing rewrites it.
- A wire-format or semantic change adds a changelog bullet under the in-progress version: what changed, not why. Never add a section for an unpublished next version.
- `doc/.vitepress/drafts.ts` renders the drafts on doc.moq.dev. A kramdown construct new to the drafts needs a translator case in the same PR; `bun run --cwd doc check` catches regressions.
- Follow IETF contribution rules (BCP 78/79); see `CONTRIBUTING.md` here.
