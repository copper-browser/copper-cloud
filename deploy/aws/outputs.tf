output "public_ip" {
  description = "Public IP of the copper-cloud VM (Elastic IP when use_eip = true; point your domain's A record here)."
  value       = var.use_eip ? aws_eip.this[0].public_ip : aws_instance.this.public_ip
}

output "url" {
  description = "Public HTTPS URL of the instance."
  value       = "https://${local.public_host != "" ? local.public_host : aws_instance.this.public_ip}"
}

output "instance_id" {
  description = "EC2 instance id."
  value       = aws_instance.this.id
}

output "db_endpoint" {
  description = "RDS endpoint (host:port); reachable only from the app security group."
  value       = aws_db_instance.this.endpoint
  sensitive   = false
}

output "link_code_command" {
  description = "Prints the link code (paste into Copper › Settings › Cloud)."
  value       = "aws ssm get-parameter --region ${var.region} --name ${aws_ssm_parameter.link_code.name} --with-decryption --query Parameter.Value --output text"
}

output "status_command" {
  description = "Prints the cloud-init install status (pending|installing|ready|failed: <step>)."
  value       = "aws ssm get-parameter --region ${var.region} --name ${aws_ssm_parameter.status.name} --query Parameter.Value --output text"
}

output "ssm_session_command" {
  description = "Opens a shell on the VM via SSM Session Manager (needs the session-manager-plugin)."
  value       = "aws ssm start-session --region ${var.region} --target ${aws_instance.this.id}"
}
