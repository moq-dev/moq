# MoQ infra

Cloudflare Workers and supporting scripts that back project-owned
infrastructure.

| Worker      | Domain          | R2 bucket       | What it serves                          |
| ----------- | --------------- | --------------- | --------------------------------------- |
| `infra/apt` | `apt.moq.dev`   | `apt-moq-dev`   | Debian/Ubuntu package repository        |
| `infra/rpm` | `rpm.moq.dev`   | `rpm-moq-dev`   | Fedora/RHEL package repository          |

Each worker is a standalone bun package with a `wrangler.jsonc` and a
small TypeScript handler that proxies GETs out of the R2 bucket with
appropriate `Content-Type` and `Cache-Control` headers for the file kind.

## Deploying a worker

From the repo root:

```bash
just infra apt deploy
just infra rpm deploy
```

Each recipe runs `bun install` and `bun wrangler deploy`. The
`custom_domain: true` route entry in the wrangler config auto-provisions
the DNS record on first deploy.

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
   repository signature:

   ```bash
   gpg --export --armor admin@moq.dev > moq-archive-keyring.gpg
   bun wrangler r2 object put apt-moq-dev/moq-archive-keyring.gpg \
     --file moq-archive-keyring.gpg --remote
   bun wrangler r2 object put rpm-moq-dev/moq-archive-keyring.gpg \
     --file moq-archive-keyring.gpg --remote
   ```

5. **Configure GitHub Actions secrets** (Settings -> Secrets and variables
   -> Actions):

   - `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`: R2 API token with
     object read/write on both buckets.
   - `R2_ACCOUNT_ID`: the Cloudflare account id.
   - `SIGNING_KEY` and `SIGNING_PASSWORD`: already configured for the
     Maven Central release workflow; the apt/rpm publishers reuse them.

After this, every release that publishes a `.deb` or `.rpm` (one of the
`moq-relay-v*`, `moq-cli-v*`, `moq-token-cli-v*`, `moq-gst-v*` tags)
triggers `apt-repo.yml` / `rpm-repo.yml`, which downloads the assets,
regenerates the repository metadata, signs it, and uploads the diff.

## Rotating the signing key

If the key needs to be rotated, repeat steps 3 through 5 with a new key.
Upload the new `moq-archive-keyring.gpg` alongside the old one (use a
versioned filename, e.g. `moq-archive-keyring-2026.gpg`) and update the
install docs at `doc/setup/linux.md` to point users at the new URL.
Existing installations keep validating against the old key until they
re-import.

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
