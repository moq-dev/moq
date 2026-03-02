output "instance_ip" {
  description = "Publisher instance IP"
  value       = linode_instance.publisher.ipv4
}
