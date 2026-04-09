# Infrastructure

OpenTofu/Terraform configuration for deploying clustered MoQ relays to Linode.
There's nothing special about Linode, other cloud providers will work provided they support UDP and public IPs.

However, we do use GCP for GeoDNS because most providers don't support it or too expensive (Cloudflare).

## Structure

The infrastructure is split into four independent tofu root modules, each with its own state:

- **`common/`** - Shared infrastructure: DNS zone, GCP service account, Linode bootstrap script, monitoring
- **`relay/`** - Relay server instances, firewalls, and geo-DNS records
- **`pub/`** - Publisher instance and DNS record
- **`boy/`** - MoQ Boy emulator instance and DNS record

The `common/` module must be deployed first, as `relay/`, `pub/`, and `boy/` read its outputs via `terraform_remote_state` (from `../common/tofu.tfstate`). Each service module's `deploy` recipe handles this automatically. Deploy individually via `just cdn relay deploy`, `just cdn pub deploy`, or `just cdn boy deploy`, or all at once via `just cdn deploy`.

## Setup

1. Create a `secrets/` directory with JWT/JWK credentials:
   ```bash
   mkdir -p secrets
   cargo run --bin moq-token-cli -- generate --key secrets/root.jwk > secrets/root.jwk
   ```
2. Copy `terraform.tfvars.example` to `terraform.tfvars` and fill in your values. Each subdirectory symlinks to this shared file.
3. Deploy (init and apply are handled automatically):
   ```bash
   just cdn deploy
   ```

## Deploy

1. `just cdn relay pin` / `just cdn pub pin` / `just cdn boy pin` to pin to the latest release tags.
2. `just cdn deploy` to deploy everything, or deploy individually:
   - `just cdn relay deploy` to deploy all relay nodes (or `just cdn relay deploy usc` for a single node)
   - `just cdn pub deploy` to deploy the publisher
   - `just cdn boy deploy` to deploy the boy emulator

## Monitor

Use `just cdn` to see all of the available commands.

1. `just cdn relay ssh <node>` to SSH into a specific relay node.
2. `just cdn relay logs <node>` to view the logs of a specific node.
3. `just cdn health` to run health checks against all relay nodes.

## Costs

Change the relay nodes in [relay/variables.tf](relay/variables.tf).

- $25/month for `g6-standard-2` nodes.
- $5/month for `g6-nanode-1` nodes.

The default configuration is 5 `g6-standard-2` relay nodes, 1 `g6-standard-1` boy node, and 1 `g6-nanode-1` publisher node. So ~$142/month.
