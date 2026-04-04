# DNS zone for relay servers
resource "google_dns_managed_zone" "relay" {
  name     = "relay-cdn"
  dns_name = "${var.domain}."
  dnssec_config {
    state = "on"
  }
}

# DNS record for publisher node
resource "google_dns_record_set" "publisher" {
  name         = "pub.${google_dns_managed_zone.relay.dns_name}"
  managed_zone = google_dns_managed_zone.relay.name
  type         = "A"
  ttl          = 300
  rrdatas      = module.pub.instance_ip
}

# DNS record for boy node
resource "google_dns_record_set" "boy" {
  name         = "boy.${google_dns_managed_zone.relay.dns_name}"
  managed_zone = google_dns_managed_zone.relay.name
  type         = "A"
  ttl          = 300
  rrdatas      = module.boy.instance_ip
}

# Grant DNS admin permissions to the service account
resource "google_project_iam_member" "dns_admin" {
  project = var.gcp_project
  role    = "roles/dns.admin"
  member  = "serviceAccount:${google_service_account.relay.email}"
}
