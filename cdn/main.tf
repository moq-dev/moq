terraform {
  required_providers {
    linode = {
      source  = "linode/linode"
      version = "~> 3.4"
    }

    google = {
      source  = "hashicorp/google"
      version = "~> 5.0"
    }
  }

  backend "local" {
    path = "tofu.tfstate"
  }

  required_version = ">= 1.6"
}

provider "linode" {
  token = var.linode_token
}

provider "google" {
  project = var.gcp_project
}

# Look up the project to get the string project ID (vs numeric project number)
data "google_project" "current" {}

variable "gcp_service_list" {
  description = "The list of apis necessary for the project"
  type        = list(string)
  default = [
    "dns.googleapis.com",
    "monitoring.googleapis.com",
  ]
}

resource "google_project_service" "all" {
  for_each                   = toset(var.gcp_service_list)
  service                    = each.key
  disable_dependent_services = false
  disable_on_destroy         = false
}

# Shared monitor service (memory + health checks)
resource "local_file" "monitor_service" {
  content = templatefile("${path.module}/common/monitor.service.tftpl", {
    webhook = var.webhook
    domain  = var.domain
  })
  filename = "${path.module}/common/gen/monitor.service"
}

module "relay" {
  source          = "./relay"
  domain          = var.domain
  email           = var.email
  ssh_keys        = var.ssh_keys
  relays          = local.relays
  stackscript_id  = linode_stackscript.bootstrap.id
  gcp_account_key = google_service_account_key.relay.private_key
  dns_zone_name   = google_dns_managed_zone.relay.name
}

module "pub" {
  source          = "./pub"
  domain          = var.domain
  relay           = "use.${var.domain}"
  ssh_keys        = var.ssh_keys
  stackscript_id  = linode_stackscript.bootstrap.id
  gcp_account_key = google_service_account_key.relay.private_key
}
