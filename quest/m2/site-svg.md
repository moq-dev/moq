# [M] Hand-drawn blog art as SVG

## Goal

The hand-drawn diagrams and sketches in the moq.dev blog under `site/` are
served as SVG instead of PNG, on a transparent background in the site
palette, so each sits on the page itself at any resolution. Social cards
still get a PNG. Screenshots, memes, photos, charts, and logos stay raster.
A just recipe turns a newly drawn PNG into the same kind of SVG.

## Plan

Decided 2026-10-20 in planning:

- **Hand-drawn only.** As of 2026-10-20 moq.dev holds 55 hand-drawn PNGs
  across the blog posts plus the author avatar. They are white (`#FAFAFA`),
  green (`#00BE2D`), and grey (`#A3A5A8`) strokes, some with yellow, red, or
  orange accents. Vectorizing the screenshots, memes, photos, and charts gains
  nothing.
- **Transparent background, palette colours, no light mode.** The site is
  dark-only (`color-scheme: dark` on slate-900, no `prefers-color-scheme`) and
  embeds images as markdown `<img>`, which CSS variables cannot reach. Drop
  the baked background, most often `#0F172A`, but `#292929` and `#212121` in
  quic-powers, which show as grey boxes today. Snap the antialiased stroke
  colours to the palette. `currentColor` was rejected because it only works
  for inlined SVG and the site has no light theme to serve.
- **Per-colour potrace, keeping the hand-drawn look.** Quantize to the
  palette, then blur, threshold, and potrace each colour layer, the pipeline
  the site's existing drawn SVGs already come from. vtracer was rejected for
  noisier stroke output that does not match them. Redrawing by hand was
  rejected as 55 images of manual work that changes the look. No editable
  sources exist anywhere in moq.dev's history, only the flattened PNGs.
- **Mixed images stay raster:** `to-wasm/slide.png` (pasted Ferris and
  TypeScript bitmaps), `on-a-boat/kraken.png` (a photo grid), and
  `update-00/globe.png` (a screenshot). Filled drawings such as
  `quic-powers/vegeta.png`, `kixelCat.png`, `be-the-outlier/narwhal.png`,
  `on-a-boat/boat.png`, `first-cdn/cloudflare.png`, and `moqbs/moqbs.png` are
  traced too. Any that comes out visibly worse or larger than its PNG stays
  PNG, and the PR lists each one. Embedding bitmaps inside an SVG was
  rejected because it defeats the point.
- **Covers are rasterized at build time.** About a third of the drawings are
  a post's frontmatter `cover:`, which becomes `og:image`. Social crawlers
  cannot render SVG, so `scripts/check-og.ts` rejects it. The Astro build
  renders each SVG cover to PNG on the slate background for `og:image`, so the
  SVG is the only committed source and nothing can drift. Committing a PNG
  copy per cover, and leaving covers raster, were both rejected.
- **One tool.** Generalize moq.dev's `tools/icons/smooth.sh`, which renders,
  blurs, thresholds, and potraces the homepage icons
  ([moq-dev/moq.dev#163](https://github.com/moq-dev/moq.dev/pull/163)), so it
  traces drawn PNGs as well. Do not keep two near-duplicate pipelines. Expose
  it as a `site` just recipe, with potrace in the Nix dev shell. Hand-drawn
  posts keep coming, and the binary cap in `sh/large-files.sh`
  pushes them toward SVG anyway.
- **Docs: one line in `site/AGENTS.md`** on the drawn-PNG recipe. A doc page
  was rejected because readers of the docs site never see this.
- **Lands after the move**, as its own quest outside the site epic, so the
  move stays a faithful snapshot and this does not hold the epic open. The
  PNGs the move commits stay in history, which the cap already accepts.
  Converting in moq.dev first, and folding it into the move, were rejected.

Guidance:

- Delete each replaced PNG and update every reference, including posts that
  reuse another post's image (`robotics.mdx` reuses five), the cover, and
  `components/author.astro` for the avatar.
- The drawings were made at fixed sizes (960x540, 1920x1080, 1920x640). Keep
  the `viewBox` at the original size so layout does not shift.
- Compare each SVG against its PNG in the dev server at 1x and 2x before
  calling it done. Check a cover's built `og:image` too.

## Required

- [Move moq.dev into site/](/quest/m2/site/move.md) - the drawings, `smooth.sh`, and `site/AGENTS.md` must be in this tree

## Related

- [moq.dev in the monorepo](/quest/m2/site/README.md) - the epic this follows, kept separate so it can close without this
