# DNS record for boy node
resource "google_dns_record_set" "boy" {
  name         = "boy.${var.domain}."
  managed_zone = data.terraform_remote_state.common.outputs.dns_zone_name
  type         = "A"
  ttl          = 300
  rrdatas      = linode_instance.boy.ipv4
}
