# [L] Move moq.dev into site/

## Goal

moq-dev/moq.dev's three sites live under `site/`: the moq.dev Astro blog and
demos, moq.pub, and moq.watch. They depend on the in-tree `@moq/*` packages,
are checked and built in the merge queue, and deploy from this repository.
moq.dev's quests and open draft posts move with them.

## Plan

Decided 2026-10-20 in planning:

- **`site/`**, chosen over `demo/site/` and `www/`: the sites are the
  project's public face, not a demo.
- **A fresh snapshot, not history-preserving.** The old history stays in the
  archived moq.dev repository, and a history merge would carry every image
  version the size cap exists to keep out.
- **Recompress every image to fit the [500 KB cap](/quest/m2/site/binary-size.md).**
  Lossless first (oxipng, gifsicle), then convert or shrink whatever is still
  over. As of 2026-10-20 moq.dev holds roughly 17 MB of binaries, 9 of them
  over 500 KB, the largest a 1 MB gif. OG and social covers stay PNG or JPEG,
  because the site's `scripts/check-og.ts` rejects formats social crawlers
  cannot render. Nothing goes on the allowlist.
- **A bun workspace member.** Add `site` to the root workspaces and depend on
  `@moq/*` with `workspace:^`, so the site builds against the packages beside
  it.
- **A `site` module in the CI impact map**, `sh/dispatch.sh`, running check,
  test, the Astro build, and the OG check, plus `mod site` just recipes. Its
  pattern should include the `js/` packages the site consumes, so a player
  change that breaks the site fails the queue.
- **Deploys stay manual.** Live (moq.dev, moq.pub, moq.watch) deploys from the
  `release` branch, so the player matches the production relays. Staging
  (new.moq.dev, new.moq.pub, new.moq.watch) deploys from `main`. Reason: main
  carries the next breaking release and could ship a client production relays
  do not speak. Deploying `main` everywhere and pinning npm versions in the
  monorepo were rejected.
- **Site changes reach live by backport.** A site-only change such as a blog
  post lands on `main`, then a backport PR ports it onto `release`, like a
  serious fix. Chosen over a split build (site from main, packages from
  release) and over targeting site PRs at `release`. Document this flow in
  CONTRIBUTING.md.
- **Adopt the monorepo's tooling.** Drop the site's `flake.nix` and its Bun pin
  parity assertions (Nix, `package.json`, and Dockerfile agree), its
  Dockerfile (the Workers deploy does not use it), `biome.json`, and the stray
  `pnpm-workspace.yaml`. Porting the Bun pin check was rejected: the
  monorepo's own pins govern.
- **A short `site/AGENTS.md`** keeps only site-specific gotchas: the
  `/<project>/<name>` URL scheme, the load-bearing
  `assets.run_worker_first: ["/"]` in each `wrangler.jsonc`, the asset-versus-
  page 404 fallback, never deploying without the maintainer's explicit
  permission, and the deploy and backport flow. Drop the rules the root
  `AGENTS.md` already carries, and the rule that player fixes belong upstream.
- **Port moq.dev's quests** (m0/assets-release, m0/dogfood-assets,
  m1/voice-buffers, m1/voice-timeline-track, m1/watch-arrivals-release). Ones
  that waited on an npm release now wait on the feature reaching `release`.
  Reconcile with this tree rather than duplicate it: the npm `assets()`
  condition already exists as
  [Published assets](/quest/m2/release/assets-release.md), and
  [Hosted worklet adoption](/quest/m2/release/dogfood-assets.md) links
  moq.dev's half, which can fold back into it. Rank the voice demo quests in
  m2 on their own, not under this epic, so they do not hold it open.
- **Port the open draft posts** by re-applying their MDX under `site/` as draft
  PRs here: moq-dev/moq.dev#148 (Update 02), #145 (Web Datagrams), #132 (AI
  Influencer outline), and #124 (More Hops, Lower Latency). Close the stale #16
  and #13. Landing or closing all of them first, or leaving them on the archive,
  were rejected.
- **Update links to github.com/moq-dev/moq.dev**, such as the source page and
  footer, to point at `site/` here.

Guidance:

- `main`'s packages carry breaking changes `release` lacks, so the snapshot may
  need porting to main's APIs while `release` builds it as-is. Live deploys
  switch over only once `site/` is on `release`, through a backport of the move.
  Until then live keeps deploying from moq.dev.
- Verify each site in the dev server and a staging deploy (with the
  maintainer's permission) before calling the move done; `just check` alone
  is not verification.

## Required

- [Cap committed binary size](/quest/m2/site/binary-size.md) - the cap exists before the images arrive

## Related

- [Resolve demo and site overlap](/quest/m2/site/demo-overlap.md) - decides which duplicated demo pages survive after the move
