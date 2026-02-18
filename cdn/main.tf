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
