# Generate systemd service files from templates
resource "local_file" "hang_bbb_service" {
  content = templatefile("${path.module}/pub/hang-bbb.service.tftpl", {
    domain = var.domain
  })
  filename = "${path.module}/gen/hang-bbb.service"
}

# Publisher instance
resource "linode_instance" "publisher" {
  label  = "publisher-moq"
  region = "us-central" # Dallas, TX
  type   = "g6-nanode-1"

  # Use Debian 12 as base, will be converted to NixOS via bootstrap
  image           = "linode/debian12"
  root_pass       = random_password.publisher_root.result
  authorized_keys = var.ssh_keys

  # Publisher only needs outbound, no special inbound
  firewall_id = linode_firewall.publisher.id

  # Bootstrap script - only installs Nix and creates directories
  stackscript_id = linode_stackscript.bootstrap.id
  stackscript_data = {
    hostname    = "pub.${var.domain}"
    gcp_account = google_service_account_key.relay.private_key
  }

  tags = ["publisher", "moq"]
}

# Generate random root password for publisher
resource "random_password" "publisher_root" {
  length  = 32
  special = true
}

# Firewall rules for publisher (SSH only)
resource "linode_firewall" "publisher" {
  label = "publisher-firewall"

  inbound {
    label    = "allow-ssh"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "22"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  inbound_policy  = "DROP"
  outbound_policy = "ACCEPT"

  tags = ["publisher"]
}
