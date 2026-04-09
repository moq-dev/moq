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

Each module can be deployed independently via `just cdn <module> deploy-infra`, or all at once via `just cdn deploy`.

## Setup

1. Create a `secrets/` directory with JWT/JWK credentials:
   ```bash
   mkdir -p secrets

   # generate the root key private key
   cargo run --bin moq-token-cli -- generate --key secrets/root.jwk > secrets/root.jwk

   # to allow relay servers to connect to each other
   cargo run --bin moq-token-cli -- sign --key secrets/root.jwk --publish "" --subscribe "" --cluster > secrets/cluster.jwt

   # to allow publishing to `demo/`
   cargo run --bin moq-token-cli -- sign --key secrets/root.jwk --root "demo" --publish "" > secrets/demo-pub.jwt

   # to allow subscribing to `demo/` (used by health checks and the website)
   cargo run --bin moq-token-cli -- sign --key secrets/root.jwk --root "demo" --subscribe "" > secrets/demo-sub.jwt

   # to allow moq-boy to publish to `demo/boy` and subscribe to `anon/boy`
   cargo run --bin moq-token-cli -- sign --key secrets/root.jwk --root "" --publish "demo/boy" --subscribe "anon/boy" > secrets/boy.jwt
   ```
2. Create `terraform.tfvars` in each module directory (see `terraform.tfvars.example` for reference).
3. Initialize and apply each module:
   ```bash
   cd common && tofu init && tofu apply
   cd relay && tofu init && tofu apply
   cd pub && tofu init && tofu apply
   cd boy && tofu init && tofu apply
   ```

## Deploy

1. `just cdn relay pin` / `just cdn pub pin` / `just cdn boy pin` to pin to the latest release tags.
2. `just cdn deploy` to deploy everything, or deploy individually:
   - `just cdn relay deploy-all` to deploy software to all relay nodes
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

The default configuration is 5 `g6-standard-2` relay nodes, 1 `g6-standard-2` boy node, and 1 `g6-nanode-1` publisher node. So ~$154/month.
