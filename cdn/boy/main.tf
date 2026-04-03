locals {
  roms = {
    "big2small" = "big2small.gb"
    "dangan"    = "DanganGB2.gbc"
    "opossum"   = "OpossumCountry.gbc"
    "capybara"  = "Capybara-Village.gb"
    "fofk"      = "FoFK.gb"
    "gb-run"    = "gb-run.gbc"
  }
}

# Generate systemd service files from templates
resource "local_file" "boy_prepare_service" {
  for_each = local.roms
  content = templatefile("${path.module}/boy-prepare.service.tftpl", {
    name = each.key
    rom  = each.value
  })
  filename = "${path.module}/gen/boy-${each.key}-prepare.service"
}

resource "local_file" "boy_service" {
  for_each = local.roms
  content = templatefile("${path.module}/boy.service.tftpl", {
    domain = var.domain
    name   = each.key
    rom    = each.value
  })
  filename = "${path.module}/gen/boy-${each.key}.service"
}

# Boy instance (beefier than pub for concurrent emulation + encoding)
resource "linode_instance" "boy" {
  label  = "boy-moq"
  region = "us-central" # Dallas, TX
  type   = "g6-standard-2"

  image           = "linode/debian12"
  root_pass       = random_password.boy_root.result
  authorized_keys = var.ssh_keys

  firewall_id = linode_firewall.boy.id

  stackscript_id = var.stackscript_id
  stackscript_data = {
    hostname    = "boy.${var.domain}"
    gcp_account = var.gcp_account_key
  }

  tags = ["boy", "moq"]
}

resource "random_password" "boy_root" {
  length  = 32
  special = true
}

# Firewall rules (SSH only, outbound open)
resource "linode_firewall" "boy" {
  label = "boy-firewall"

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

  tags = ["boy"]
}
