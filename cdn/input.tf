variable "linode_token" {
  description = "Linode API token"
  type        = string
  sensitive   = true
}

variable "gcp_project" {
  description = "GCP project ID for DNS management"
  type        = string
}

variable "domain" {
  description = "Relay domain name"
  type        = string
}

variable "email" {
  description = "Email address for LetsEncrypt notifications"
  type        = string
}

variable "ssh_keys" {
  description = "SSH public keys for root access"
  type        = list(string)
}

variable "webhook" {
  description = "Webhook URL for all alerts (Slack, Discord, etc.)"
  type        = string
  sensitive   = true
  default     = ""
}

# Relay node definitions
# regions: https://api.linode.com/v4/regions
# instance types: https://api.linode.com/v4/linode/types
locals {
  relays = {
    usc = {
      region = "us-central"    # Dallas, TX
      type   = "g6-standard-2" # 4GB RAM, 2 vCPU, $24/mo, 4TB out
    }
    usw = {
      region = "us-west" # Fremont, CA
      type   = "g6-standard-2"
    }
    use = {
      region = "us-east" # Newark, NJ
      type   = "g6-standard-2"
    }
    euc = {
      region = "eu-central" # Frankfurt, Germany
      type   = "g6-standard-2"
    }
    sea = {
      region = "ap-south" # Singapore
      type   = "g6-standard-2"
    }
  }
}
