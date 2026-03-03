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

variable "relays" {
  description = "Map of relay node configurations"
  type = map(object({
    region = string
    type   = string
  }))
}

variable "stackscript_id" {
  description = "Linode StackScript ID for bootstrap"
  type        = number
}

variable "gcp_account_key" {
  description = "GCP service account private key"
  type        = string
  sensitive   = true
}

variable "dns_zone_name" {
  description = "Google Cloud DNS managed zone name"
  type        = string
}
