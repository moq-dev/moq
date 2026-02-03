# Infrastructure

OpenTofu/Terraform configuration for deploying clustered MoQ relays to Linode.
There's nothing special about Linode, other cloud providers will work provided they support UDP and public IPs.

However, we do use GCP for GeoDNS because most providers don't support it or too expensive (Cloudflare).

## Setup

1. Copy `terraform.tfvars.example` to `terraform.tfvars` and fill in values
2. Run `tofu init`.
3. Run `tofu apply`.
4. Create a `secrets/` directory with JWT/JWK credentials:
  - ```bash
    mkdir -p secrets

    # generate the root key private key
    cargo run --bin moq-token -- --key secrets/root.jwk generate > secrets/root.jwk

    # to allow relay servers to connect to each other
    cargo run --bin moq-token -- --key secrets/root.jwk sign --publish "" --subscribe "" --cluster > secrets/cluster.jwt

    # to allow publishing to `demo/`
    cargo run --bin moq-token -- --key secrets/root.jwk sign --root "demo" --publish "" > secrets/demo-pub.jwt
    ```

## Deploy

1. `nix flake update` to update the `moq-relay` and `moq-cli` binaries.
  - **NOTE**: This pulls from `main` on github, not a local path or the latest release.
2. `just deploy-all` to deploy to all nodes in parallel.
  - This will take a while as the builds *currently* occur on the remote nodes.
  - Somebody should set up remote builders or cross-compilation.

## Monitor
Use `just` to see all of the available commands.

1. `just ssh <node>` to SSH into a specific node.
2. `just logs <node>` to view the logs of a specific node.
3. etc

## Costs
Change the number of nodes in [input.tf](input.tf).

- $25/month for `g6-standard-2` nodes.
- $5/month for `g6-nanode-1` nodes.

The default configuration is 3 `g6-standard-2` relay nodes and 1 `g6-nanode-1` publisher node. So $80/month.

**NOTE**: `moq-relay` does not scale particularly well right now.
- The current design is a mesh network, so more nodes means more unnecessary backbone traffic.
- Quinn currently uses a single UDP receive thread, so scaling to multiple cores won't help.
