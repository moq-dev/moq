output "instance_ips" {
  description = "Map of relay node IPs"
  value = {
    for key, instance in linode_instance.relay : key => instance.ipv4
  }
}
