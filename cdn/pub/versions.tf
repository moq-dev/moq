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

    local = {
      source  = "hashicorp/local"
      version = "~> 2.5"
    }
  }

  backend "local" {
    path = "tofu.tfstate"
  }

  required_version = ">= 1.6"
}
