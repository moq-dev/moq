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

variable "webhook" {
  description = "Webhook URL for all alerts (Slack, Discord, etc.)"
  type        = string
  sensitive   = true
  default     = ""
}
