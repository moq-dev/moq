provider "linode" {
  token = var.linode_token
}

provider "google" {
  project = var.gcp_project
}
