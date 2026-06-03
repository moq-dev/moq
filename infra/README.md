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
`moq-relay-v*`, `moq-cli-v*`, `moq-token-cli-v*`, `moq-gst-v*` tags)
triggers `apt-repo.yml` / `rpm-repo.yml`, which downloads the assets,
regenerates the repository metadata, signs it, and uploads the diff.

## Rotating the signing key

If the key needs to be rotated, repeat steps 3 through 5 with a new key
(keeping the apt-binary / rpm-armored split). Upload the new keyring under a
versioned filename, e.g. `moq-keyring-2026.gpg`, alongside the existing
`moq-keyring.gpg`, and update the install docs at `doc/setup/linux.md` to point
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
