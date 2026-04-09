output "domain" {
  description = "Base domain for all nodes"
  value       = var.domain
}

output "stackscript_id" {
  description = "Linode StackScript ID for bootstrap"
  value       = linode_stackscript.bootstrap.id
}

output "gcp_account_key" {
  description = "GCP service account private key"
  value       = google_service_account_key.cdn.private_key
  sensitive   = true
}

output "dns_zone_name" {
  description = "Google Cloud DNS managed zone name"
  value       = google_dns_managed_zone.cdn.name
}
