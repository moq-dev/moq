# MoQ infra

Cloudflare Workers and supporting scripts that back project-owned
infrastructure.

| Worker      | Domain          | R2 bucket       | What it serves                          |
| ----------- | --------------- | --------------- | --------------------------------------- |
| `infra/apt` | `apt.moq.dev`   | `apt-moq-dev`   | Debian/Ubuntu package repository        |
| `infra/rpm` | `rpm.moq.dev`   | `rpm-moq-dev`   | Fedora/RHEL package repository          |
| `infra/moq-sh` | `moq.sh`     |                 | The `moq` installer script              |
| `infra/mbx` |                 | `mbx-moq-dev`   | Private Rust build cache for CI         |
| `infra/nix` | `nix.moq.dev`   | `nix-moq-dev`, `nix-ci-moq-dev` | Nix binary cache        |
| `infra/token` | `token.moq.dev` |               | Scoped moq-bot tokens for CI, by OIDC   |

Each worker is a standalone bun package with a `wrangler.jsonc` and a
small TypeScript handler. The apt and rpm workers proxy GETs out of the R2
bucket with appropriate `Content-Type` and `Cache-Control` headers for the
file kind, and the nix worker does the same across its two buckets. The
moq.sh worker serves `install.sh` from its bundle at every path, with the
newest `moq-cli` release filled in as the default version.
`infra/mbx` has no Worker: CI reaches its bucket directly through mbx's S3
client.

## Deploying a worker

From the repo root:

```bash
just infra apt deploy
just infra rpm deploy
just infra moq-sh deploy
just infra nix deploy
just infra token deploy
```

Each recipe runs `bun install` and `bun wrangler deploy`. The
`custom_domain: true` route entry in the wrangler config auto-provisions
the DNS record on first deploy.

moq.sh also deploys from CI (`.github/workflows/moq-sh.yml`): on a push to
`release` that touches `infra/moq-sh/`, and after every `moq-cli` release.
Its `CLOUDFLARE_API_TOKEN` secret has the account-wide Workers Editor role,
which can update an existing worker but not create one or change its route.
So the first deploy, and any change to the custom domain, runs by hand under a
maintainer's `wrangler login`. Whether Editor alone passes wrangler's
custom-domain check on every deploy is unproven until the first `release`
run; [the deploy quest](../quest/m2/release/moq-sh-deploy.md) tracks it.

## The token broker

`token.moq.dev` holds the moq-bot key so that no repository has to. A workflow
job with `id-token: write` asks GitHub for an ID token with audience
`token.moq.dev` and POSTs it to a policy's path. The worker verifies GitHub's
signature and issues a one-hour installation token, scoped to the calling
repository, only when the job runs that repository's policy workflow on
`main` in the moq-dev organization. A pushed branch, a pull request, a tag,
every other workflow, and a reusable workflow the policy workflow calls are
refused.

| Path | Workflow | Permissions |
| --- | --- | --- |
| `/land` | `.github/workflows/land.yml` | `contents`, `workflows`: write |

A repository opts in by adding the workflow; moq-bot is installed on every
moq-dev repository. Only main's copy of the workflow counts, and only the
gate or a ruleset bypass changes `main`, so the job that requests the ID
token must never run pull request code. The first deploy, and any key
rotation, run by hand:

```bash
just infra token deploy
just infra token secret ~/Downloads/moq-bot.<date>.private-key.pem
```

`secret` converts GitHub's PKCS#1 key to the PKCS#8 that WebCrypto imports
and pipes it straight to `wrangler secret put`.

## Write credentials

Any branch pushed to this repository runs its own workflows with the
repository's Actions secrets, so a credential that can write or publish lives
in an Environment whose deployment policy admits only protected refs. A job
names the Environment, and a branch that is not on the list fails to start it.

| Environment | Deployable from | Holds |
| --- | --- | --- |
| `bot` | `main`, `release`, `moq-ffi-v*` | The moq-bot key (`APP_CLIENT_ID`, `APP_PRIVATE_KEY`) alone |
| `publish` | `main`, `release`, `moq-ffi-v*`, `moq-relay-v*`, `moq-cli-v*` | Registry, signing, deploy, and alert credentials, and the moq-bot key for jobs that also need one of those |
| `cache-write` | `main`, its merge queue, binary release tags | The mbx and Nix cache write keys ([below](#bootstrapping-the-rust-build-cache)) |
| `npm` | `release` | `JSR_TOKEN`; npm's trusted publisher names it |
| `pypi` | `main`, `release`, `moq-ffi-v*` | Nothing; PyPI's trusted publisher names it |
| `pub.dev` | `moq-dart-v*`, `moq-ffi-v*` | Nothing; pub.dev's automated publishing names it |
| `github-pages` | `main` | Nothing |

`main` is on the `publish` list for `workflow_run`, schedule, and dispatch jobs,
which run there, until [those publishers run on their tag](../quest/m0/ci/publish-release-only.md).
A job that needs only the moq-bot key, such as Dependabot's, names `bot`; the land gate holds no key
and gets its token from [the token broker](#the-token-broker). A job that also runs dry on other refs names the Environment
only when it publishes, as in `environment: ${{ github.ref_type == 'tag' &&
'publish' || '' }}`. Read-only secrets, such as the mbx read key, stay at the
repository level.

A tag policy only helps if the tag itself is protected, so the `release tags`
ruleset ([below](#bootstrapping-the-nix-cache)) also covers `moq-ffi-v*`
and `moq-dart-v*`. A `workflow_run` job that checks out the triggering commit
first requires its `head_branch` to be such a tag at that commit, since a
branch can share a release's name. JSR's GitHub trust checks only the repository, not an
Environment, so `release-js` publishes to JSR with `JSR_TOKEN` instead and the
JSR packages do not trust the repository.

## Bootstrapping a new package repository

The CI workflows (`.github/workflows/apt-repo.yml`,
`.github/workflows/rpm-repo.yml`) assume the R2 bucket, Cloudflare custom
domain, and signing key already exist. To stand them up the first time:

1. **Create the R2 buckets** in the existing Cloudflare account
   (`dd618f5dbd5da77b8296f1613c301f5c`):

   ```bash
   bun wrangler r2 bucket create apt-moq-dev
   bun wrangler r2 bucket create rpm-moq-dev
   ```

2. **Deploy the workers** so the custom domains come up:

   ```bash
   just infra deploy
   ```

3. **Reuse the project GPG signing key** that's already stored in the
   `SIGNING_KEY` / `SIGNING_PASSWORD` Actions secrets (also used by the
   Maven Central / Kotlin release workflow). The apt/rpm publish scripts
   import the key into an ephemeral keyring and auto-detect the long key
   id, so no separate `KEY_ID` secret is needed.

4. **Upload the public key** to both buckets so users can verify the
   repository signature. The two ecosystems need different encodings of the
   *same* key, so export it twice:

   - **apt** verifies repos with `gpgv`, which reads **binary** keyrings only
     and rejects ASCII armor (`gpgv: invalid packet (ctb=2d)` -> `NO_PUBKEY`).
     Export without `--armor` (or `gpg --dearmor` an armored copy).
   - **rpm**/dnf imports the key via `gpgkey=` and wants the conventional
     **ASCII-armored** form.

   The install docs do a plain `curl | tee` with no client-side dearmor, so the
   bytes we serve have to be usable as-is.

   ```bash
   # apt: binary / dearmored
   gpg --export admin@moq.dev > moq-keyring.gpg
   bun wrangler r2 object put apt-moq-dev/moq-keyring.gpg \
     --file moq-keyring.gpg --remote

   # rpm: ASCII-armored
   gpg --export --armor admin@moq.dev > moq-keyring.asc
   bun wrangler r2 object put rpm-moq-dev/moq-keyring.gpg \
     --file moq-keyring.asc --remote
   ```

5. **Configure GitHub Actions secrets** in the `publish` Environment (see
   [Write credentials](#write-credentials)):

   - `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`: R2 API token with
     object read/write on both buckets.
   - `R2_ACCOUNT_ID`: the Cloudflare account id.
   - `SIGNING_KEY` and `SIGNING_PASSWORD`: already configured for the
     Maven Central release workflow; the apt/rpm publishers reuse them.

After this, every release that publishes a `.deb` or `.rpm` (one of the
`moq-relay-v*`, `moq-cli-v*`, `moq-gst-v*` tags)
triggers `apt-repo.yml` / `rpm-repo.yml`, which downloads the assets,
regenerates the repository metadata, signs it, and uploads the diff.

## Bootstrapping the landing gate

`land.yml` separates queue resolution, the self-hosted build, and a hosted
push. The worker receives only a read credential and local compiler caches;
the hosted push reconstructs the tested chain with Quest from `main`.

A maintainer coordinates the cutover. The land job gets its moq-bot token
from [the token broker](#the-token-broker), so it reads no secret. The
workflow cannot replace GitHub's queue until its ruleset is installed with it:

1. Deploy the token broker, and delete the organization's `APP_CLIENT_ID` and
   `APP_PRIVATE_KEY` and this repository's `APP_ID` and `APP_PRIVATE_KEY`, so
   only the `bot` and `publish` Environments hold the key. Any branch's
   workflow can read the others.
2. Add moq-bot to the main ruleset's bypass actors, retain the administrator
   bypass, remove the `merge_queue` rule, and replace `Check` with required
   context `quest land`, pinned to an app that never reports it on PR heads.
   Keep the deletion and non-fast-forward protections. Other required
   checks or review rules must be ones the pinned Quest engine supports.
3. Install the workflow source and ruleset in a coordinated maintenance
   window. A queued PR that removes its own `merge_group` handler cannot
   supply the old queue's required check; changing the ruleset first also
   stops the old queue. Confirm the exact installation path before starting
   that window. This setup does not dispatch a gate or request any landing.

`MOQ_QUEUE_RUNNER` keeps selecting the existing self-hosted worker. After
cutover, request landing with the pinned-head command in
[`CONTRIBUTING.md`](../CONTRIBUTING.md). A new push invalidates that request.
The Land run links any ejection reason on the PR; failures do not retry a
batch. Measure a busy day's throughput and queue latency before declaring
the migration complete.

## Bootstrapping the Rust build cache

`.github/actions/rust-cache` points mbx at the private `mbx-moq-dev` bucket,
namespaced per runner OS and architecture. Hosted jobs on `main` write what
they compile. The landing worker uses its persistent local store and has no R2
credentials.
Release pull request jobs only read (main pull requests run no cloud CI), and
fork pull requests, which get no secrets, build cold.

The write key is what enforces that: it lives in a GitHub Environment that
only those refs can deploy to, and each writer job names that Environment only
when it runs on one of them. mbx on its own trusts only a push to a protected
branch, which leaves out Nightly's schedule, so the action sets
`MBX_REMOTE_TRUST_WRITES` for writers and the Environment is the only boundary.

All of this runs once, by a maintainer. Steps 1, 2, and 4 are done, and step 3
is done for `main`.

1. **Create the bucket and its lifecycle rule** under your `wrangler login`:

   ```bash
   just infra mbx create
   ```

   `lifecycle.json` deletes every object 30 days after upload and aborts
   multipart uploads left incomplete for a day. mbx writes each compilation
   once and never refreshes it, so a result still in use expires too; the
   next writer that misses it compiles and uploads it again. After editing
   the rules, `just infra mbx lifecycle` applies them.

2. **Create two R2 API tokens** in the dashboard (R2 -> Manage API tokens ->
   Create Account API token), both scoped to `mbx-moq-dev` only. Wrangler
   cannot create R2 tokens. Each shows an Access Key ID and a Secret Access
   Key once.

   - `mbx-read`: **Object Read only**.
   - `mbx-write`: **Object Read & Write**.

3. **Create the `cache-write` Environment**, deployable from `main` and the
   protected release tags configured below.
   A protected-branches policy would admit `release` as
   well, so name the branches instead:

   ```bash
   gh api -X PUT repos/moq-dev/moq/environments/cache-write --input - <<'EOF'
   {"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
   EOF
   gh api -X POST repos/moq-dev/moq/environments/cache-write/deployment-branch-policies \
     -f name=main -f type=branch
   ```

   Remove an existing `gh-readonly-queue/main/*` branch policy after the
   batched landing gate replaces GitHub's queue.

4. **Store the keys under the same two names at both levels.** An Environment
   secret shadows a repository secret of the same name, so writers get the
   write key and every other job the read key, all through one pair of names:

   ```bash
   # mbx-write, readable only by jobs in the cache-write Environment
   gh secret set MBX_R2_ACCESS_KEY_ID --env cache-write --repo moq-dev/moq
   gh secret set MBX_R2_SECRET_ACCESS_KEY --env cache-write --repo moq-dev/moq

   # mbx-read, for every other job
   gh secret set MBX_R2_ACCESS_KEY_ID --repo moq-dev/moq
   gh secret set MBX_R2_SECRET_ACCESS_KEY --repo moq-dev/moq
   ```

Without the repository secrets, readers warn and build cold. A writer without
its key fails rather than publish nothing, and so does a key the bucket
refuses, in any job.

## Rotating the signing key

If the key needs to be rotated, repeat steps 3 through 5 with a new key
(keeping the apt-binary / rpm-armored split). Upload the new keyring under a
versioned filename, e.g. `moq-keyring-2026.gpg`, alongside the existing
`moq-keyring.gpg`, and update the install docs at `doc/setup/install.md` to point
users at the new URL. Existing installations keep validating against the old
key until they re-import.

## Manual regeneration

If a release was missed or the repository state needs to be rebuilt
from scratch, the publish scripts can be invoked locally:

```bash
gh release download moq-relay-v1.2.3 --dir artifacts --pattern '*.deb'
ARTIFACTS_DIR=artifacts \
  R2_ACCESS_KEY_ID=... R2_SECRET_ACCESS_KEY=... R2_ACCOUNT_ID=... \
  SIGNING_KEY="$(cat private.asc)" SIGNING_PASSWORD=... \
  ./infra/apt/publish.sh
```

Or trigger the GitHub Actions workflow manually:

```bash
gh workflow run apt-repo.yml -f tag=moq-relay-v1.2.3
gh workflow run rpm-repo.yml -f tag=moq-relay-v1.2.3
```

## Bootstrapping the Nix cache

`nix.moq.dev` serves two buckets as one binary cache. `nix-moq-dev` holds
released packages, pushed by `release-nix.yml` on a release tag, and never
expires. `nix-ci-moq-dev` holds the dev shells `cache.yml` pushes after every
merge to `main`, for x86 Linux, ARM Linux, and ARM macOS. It publishes the full
build closure from `nix develop --profile`, uploading only missing paths.
Its lifecycle rule (`infra/nix/lifecycle.json`) deletes each object 30 days
after upload; the next warm re-uploads what it still needs. `flake.nix`
sets `fallback = true` because that expiry can remove a path before one that
depends on it.

Nix only trusts paths signed by the key in `flake.nix`, so the signing key is
the trust boundary, not the bucket keys. It lives in the `cache-write`
Environment beside one R2 token for both buckets. Only `main` and the five
release tag patterns can deploy there; anything that can reach
the key can sign any path, so a second Environment for releases would only
guard the bucket. Readers need no secret.

All of this runs once, by a maintainer, in this order. All five steps are
done.

1. **Restrict the release tags.** Anyone with write access can push a tag
   today, and `cache-write` admits any commit carrying one. Limit creating,
   moving, and deleting them to moq-bot, the GitHub App release-plz tags
   with, plus the role the `release` branch ruleset already lets bypass:

   ```bash
   gh api -X POST repos/moq-dev/moq/rulesets --input - <<'EOF'
   {
     "name": "release tags",
     "target": "tag",
     "enforcement": "active",
     "conditions": {"ref_name": {"exclude": [], "include": [
       "refs/tags/moq-relay-v*", "refs/tags/moq-cli-v*", "refs/tags/moq-boy-v*",
       "refs/tags/moq-c-v*", "refs/tags/moq-gst-v*",
       "refs/tags/moq-ffi-v*", "refs/tags/moq-dart-v*"
     ]}},
     "rules": [{"type": "creation"}, {"type": "update"}, {"type": "deletion"}],
     "bypass_actors": [
       {"actor_type": "Integration", "actor_id": 1039466, "bypass_mode": "always"},
       {"actor_type": "RepositoryRole", "actor_id": 2, "bypass_mode": "always"}
     ]
   }
   EOF
   ```

2. **Create the buckets and deploy the Worker** under your `wrangler login`:

   ```bash
   just infra nix create   # both buckets, plus the CI lifecycle rule
   just infra nix deploy   # nix.moq.dev
   ```

   After editing the rules, `just infra nix lifecycle` applies them.

3. **Admit the release tags to `cache-write`**, which the Rust build cache
   bootstrap created:

   ```bash
   for tag in 'moq-relay-v*' 'moq-cli-v*' 'moq-boy-v*' 'moq-c-v*' 'moq-gst-v*'; do
     gh api -X POST repos/moq-dev/moq/environments/cache-write/deployment-branch-policies \
       -f name="$tag" -f type=tag
   done
   ```

4. **Create one R2 API token** in the dashboard (R2 -> Manage API tokens ->
   Create Account API token) with **Object Read & Write** on `nix-moq-dev` and
   `nix-ci-moq-dev` only. Wrangler cannot create R2 tokens.

   ```bash
   gh secret set NIX_R2_ACCESS_KEY_ID --env cache-write --repo moq-dev/moq
   gh secret set NIX_R2_SECRET_ACCESS_KEY --env cache-write --repo moq-dev/moq
   ```

5. **Generate the signing key.** This stores it as `NIX_SIGNING_KEY` in
   `cache-write` without writing it to disk, and prints the public key:

   ```bash
   just infra nix keygen
   ```

   Put the printed `nix.moq.dev-1:...` key in `flake.nix`'s `nixConfig`.

A job missing its secrets fails rather than skip the push. A job naming an
Environment that does not exist yet creates it with no deployment policy, so
if a workflow ran before the Rust bootstrap, check the policy before adding
secrets.

To rotate the key, run `keygen` again and add the new public key next to the
old one in `flake.nix`. Drop the old key once nothing signed with it matters:
30 days for CI paths, but never for released ones unless they are re-signed.
