# MoQ infra

Cloudflare Workers and supporting scripts that back project-owned
infrastructure.

| Worker      | Domain          | R2 bucket       | What it serves                          |
| ----------- | --------------- | --------------- | --------------------------------------- |
| `infra/apt` | `apt.moq.dev`   | `apt-moq-dev`   | Debian/Ubuntu package repository        |
| `infra/rpm` | `rpm.moq.dev`   | `rpm-moq-dev`   | Fedora/RHEL package repository          |
| `infra/moq-sh` | `moq.sh`     |                 | The `moq` installer script              |
| `infra/mbx` |                 | `mbx-moq-dev`   | Private Rust build cache for CI         |

Each worker is a standalone bun package with a `wrangler.jsonc` and a
small TypeScript handler. The apt and rpm workers proxy GETs out of the R2
bucket with appropriate `Content-Type` and `Cache-Control` headers for the
file kind. The moq.sh worker serves `install.sh` from its bundle at every path,
with the newest `moq-cli` release filled in as the default version.
`infra/mbx` has no Worker: CI reaches its bucket directly through mbx's S3
client.

## Deploying a worker

From the repo root:

```bash
just infra apt deploy
just infra rpm deploy
just infra moq-sh deploy
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

5. **Configure GitHub Actions secrets** (Settings -> Secrets and variables
   -> Actions):

   - `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`: R2 API token with
     object read/write on both buckets.
   - `R2_ACCOUNT_ID`: the Cloudflare account id.
   - `SIGNING_KEY` and `SIGNING_PASSWORD`: already configured for the
     Maven Central release workflow; the apt/rpm publishers reuse them.

After this, every release that publishes a `.deb` or `.rpm` (one of the
`moq-relay-v*`, `moq-cli-v*`, `moq-gst-v*` tags)
triggers `apt-repo.yml` / `rpm-repo.yml`, which downloads the assets,
regenerates the repository metadata, signs it, and uploads the diff.

## Bootstrapping the Rust build cache

`.github/actions/rust-cache` points mbx at the private `mbx-moq-dev` bucket,
namespaced per runner OS and architecture. Every job that runs on `main` or in
its merge queue (`gh-readonly-queue/main/*`) writes what it compiles: a queued
pull request is about to become `main`, so its code is trusted like `main`'s.
Pull request jobs only read, and fork pull requests, which get no secrets,
build cold.

The write key is what enforces that: it lives in a GitHub Environment that
only those refs can deploy to, and each writer job names that Environment only
when it runs on one of them. mbx on its own trusts only a push to a protected
branch, which leaves out the queue and Nightly's schedule, so the action sets
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

3. **Create the `cache-write` Environment**, deployable from `main` and its
   merge queue only. A protected-branches policy would admit `release` as
   well, so name the branches instead:

   ```bash
   gh api -X PUT repos/moq-dev/moq/environments/cache-write --input - <<'EOF'
   {"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
   EOF
   gh api -X POST repos/moq-dev/moq/environments/cache-write/deployment-branch-policies \
     -f name=main -f type=branch
   gh api -X POST repos/moq-dev/moq/environments/cache-write/deployment-branch-policies \
     -f name='gh-readonly-queue/main/*' -f type=branch
   ```

   Without the queue policy, every merge queue job that writes fails before
   it starts, and the queue merges nothing.

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
