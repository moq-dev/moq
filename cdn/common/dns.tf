# DNS zone for CDN servers
resource "google_dns_managed_zone" "cdn" {
  name     = "relay-cdn"
  dns_name = "${var.domain}."
  dnssec_config {
    state = "on"
  }
}

moved {
  from = google_dns_managed_zone.relay
  to   = google_dns_managed_zone.cdn
}

# Grant DNS admin permissions to the service account
resource "google_project_iam_member" "dns_admin" {
  project = var.gcp_project
  role    = "roles/dns.admin"
  member  = "serviceAccount:${google_service_account.cdn.email}"
}
