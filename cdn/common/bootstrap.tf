# Service account for all CDN instances
resource "google_service_account" "cdn" {
  account_id   = "moq-relay"
  display_name = "MoQ CDN"
  description  = "Service account for MoQ CDN instances"
}

# Keep the old terraform name mapped to the new one
moved {
  from = google_service_account.relay
  to   = google_service_account.cdn
}

# Generate service account key
resource "google_service_account_key" "cdn" {
  service_account_id = google_service_account.cdn.name
}

moved {
  from = google_service_account_key.relay
  to   = google_service_account_key.cdn
}

# Bootstrap script to install Nix on first boot
resource "linode_stackscript" "bootstrap" {
  label       = "moq-bootstrap"
  description = "Bootstrap Debian with Nix"
  script      = file("${path.module}/../bootstrap.sh")
  images      = ["linode/debian12", "linode/ubuntu25.10"]
}
