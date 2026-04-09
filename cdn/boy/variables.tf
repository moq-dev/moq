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
  description = "CDN domain name"
  type        = string
}

variable "ssh_keys" {
  description = "SSH public keys for root access"
  type        = list(string)
}

variable "location" {
  description = "Human-readable server location (e.g. 'Dallas, TX')"
  type        = string
}
