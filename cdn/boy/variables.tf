variable "domain" {
  description = "Relay domain name"
  type        = string
}

variable "ssh_keys" {
  description = "SSH public keys for root access"
  type        = list(string)
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

variable "location" {
  description = "Human-readable server location (e.g. 'Dallas, TX')"
  type        = string
}
