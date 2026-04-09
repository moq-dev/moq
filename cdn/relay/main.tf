data "terraform_remote_state" "common" {
  backend = "local"
  config = {
    path = "${path.module}/../common/tofu.tfstate"
  }
}

# Generate systemd service files from templates
resource "local_file" "moq_relay_service" {
  content = templatefile("${path.module}/moq-relay.service.tftpl", {
    domain = var.domain
  })
  filename = "${path.module}/gen/moq-relay.service"
}

resource "local_file" "moq_cert_service" {
  content = templatefile("${path.module}/moq-cert.service.tftpl", {
    domain = var.domain
    email  = var.email
  })
  filename = "${path.module}/gen/moq-cert.service"
}

# Create Linode instances
resource "linode_instance" "relay" {
  for_each = local.relays

  label  = "relay-${each.key}"
  region = each.value.region
  type   = each.value.type

  # Use Debian 12 as base, will be converted to NixOS via bootstrap
  image           = "linode/debian12"
  root_pass       = random_password.relay_root[each.key].result
  authorized_keys = var.ssh_keys

  # Open firewall for QUIC/WebTransport
  firewall_id = linode_firewall.relay.id

  # Bootstrap script - only installs Nix and creates directories
  stackscript_id = data.terraform_remote_state.common.outputs.stackscript_id
  stackscript_data = {
    hostname    = "${each.key}.${var.domain}"
    gcp_account = data.terraform_remote_state.common.outputs.gcp_account_key
  }

  tags = ["relay", "moq"]
}

# Generate random root passwords (store these securely!)
resource "random_password" "relay_root" {
  for_each = local.relays

  length  = 32
  special = true
}

# Firewall rules for relay servers
resource "linode_firewall" "relay" {
  label = "relay-firewall"

  inbound {
    label    = "allow-icmp"
    action   = "ACCEPT"
    protocol = "ICMP"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  inbound {
    label    = "allow-ssh"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "22"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  inbound {
    label    = "allow-quic-udp"
    action   = "ACCEPT"
    protocol = "UDP"
    ports    = "443"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  inbound {
    label    = "allow-quic-tcp"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "443"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  inbound_policy  = "DROP"
  outbound_policy = "ACCEPT"

  tags = ["relay"]
}
