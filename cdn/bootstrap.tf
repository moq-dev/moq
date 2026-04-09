# Service account for relay instances
resource "google_service_account" "relay" {
  account_id   = "moq-relay"
  display_name = "MoQ Relay"
  description  = "Service account for MoQ relay instances"
}

# Generate service account key
resource "google_service_account_key" "relay" {
  service_account_id = google_service_account.relay.name
}

# Bootstrap script to install Nix on first boot
resource "linode_stackscript" "bootstrap" {
  label       = "moq-bootstrap"
  description = "Bootstrap Debian with Nix"
  script      = file("${path.module}/bootstrap.sh")
  images      = ["linode/debian12", "linode/ubuntu25.10"]
}
