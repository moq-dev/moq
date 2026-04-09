# Individual DNS records for each relay node (for direct access)
resource "google_dns_record_set" "relay_node" {
  for_each = var.relays

  name         = "${each.key}.${var.domain}."
  managed_zone = var.dns_zone_name
  type         = "A"
  ttl          = 300
  rrdatas      = linode_instance.relay[each.key].ipv4
}

# Global Geo DNS, routing to the closest region
resource "google_dns_record_set" "relay_global" {
  name         = "${var.domain}."
  managed_zone = var.dns_zone_name
  type         = "A"
  ttl          = 300

  routing_policy {
    dynamic "geo" {
      for_each = local.relay_gcp_regions

      content {
        location = geo.value
        rrdatas  = linode_instance.relay[geo.key].ipv4
      }
    }
  }
}

# Region mapping for GCP geo routing
# GCP uses region codes like "us-east1", "us-west1", "europe-west3", "asia-southeast1"
locals {
  relay_gcp_regions = {
    usc = "us-central1"     # Dallas, TX -> closest GCP region
    usw = "us-west1"        # Fremont, CA -> closest GCP region
    use = "us-east4"        # Newark, NJ -> closest GCP region
    euc = "europe-west3"    # Frankfurt -> closest GCP region
    sea = "asia-southeast1" # Singapore -> closest GCP region
  }
}
