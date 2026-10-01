data "aws_ec2_instance_type" "app" {
  instance_type = var.instance_type
}

locals {
  # Graviton (t4g, m7g, c7g, ...) → arm64; everything else → amd64.
  is_arm64     = contains(data.aws_ec2_instance_type.app.supported_architectures, "arm64")
  ami_arch     = local.is_arm64 ? "arm64" : "amd64"
  tarball_arch = local.is_arm64 ? "aarch64" : "x86_64"

  # Without an EIP the address is only known at boot: user_data detects it via IMDS.
  eip_ip      = var.use_eip ? aws_eip.this[0].public_ip : ""
  public_host = var.domain != "" ? var.domain : local.eip_ip

  user_data = templatefile("${path.module}/user_data.sh.tftpl", {
    name          = var.name
    region        = var.region
    bucket        = aws_s3_bucket.artifacts.id
    binary_key    = local.binary_key
    binary_sha256 = local.binary_sha256
    eip           = local.eip_ip
    public_host   = local.public_host
    domain        = var.domain
    acme_email    = var.acme_email
    allow_signup  = var.allow_signup ? "true" : "false"
    ssm_prefix    = local.ssm_prefix
  })
}

data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"] # Canonical

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-${local.ami_arch}-server-*"]
  }
  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }
  filter {
    name   = "root-device-type"
    values = ["ebs"]
  }
}

# Allocated before the instance so its address can be baked into the TLS cert
# SAN / link code (COPPER_CLOUD_PUBLIC_HOST) at boot.
resource "aws_eip" "this" {
  count  = var.use_eip ? 1 : 0
  domain = "vpc"
  tags   = { Name = local.prefix }
}

resource "aws_instance" "this" {
  ami                    = data.aws_ami.ubuntu.id
  instance_type          = var.instance_type
  subnet_id              = local.subnet_id
  vpc_security_group_ids = [aws_security_group.app.id]
  iam_instance_profile   = aws_iam_instance_profile.instance.name
  key_name               = var.ssh_key_name != "" ? var.ssh_key_name : null
  # Default subnets auto-assign a public IPv4 already; make it explicit so the
  # no-EIP path (use_eip = false) always gets an address.
  associate_public_ip_address = true

  user_data                   = local.user_data
  user_data_replace_on_change = true

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required" # IMDSv2 only
    http_put_response_hop_limit = 1
  }

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 20
    encrypted             = true
    delete_on_termination = true
  }

  # A newer AMI must not silently replace a running server on the next apply.
  lifecycle {
    ignore_changes = [ami]

    precondition {
      condition     = var.domain == "" || var.acme_email != ""
      error_message = "acme_email is required when domain is set (ACME/Let's Encrypt)."
    }
    precondition {
      condition     = !can(regex("linux-(x86_64|aarch64)", basename(var.binary_path))) || strcontains(basename(var.binary_path), "linux-${local.tarball_arch}")
      error_message = "binary_path architecture does not match instance_type (${var.instance_type} needs linux-${local.tarball_arch})."
    }
  }

  # The VM needs the database, its secrets, the tarball and the IAM grants to
  # exist before cloud-init runs, and the status/link-code parameters to exist
  # (reset to "pending") before it overwrites them.
  depends_on = [
    aws_db_instance.this,
    aws_ssm_parameter.database_url,
    aws_ssm_parameter.instance_key,
    aws_ssm_parameter.master_key,
    aws_ssm_parameter.status,
    aws_ssm_parameter.link_code,
    aws_s3_object.binary,
    aws_iam_role_policy.instance,
    aws_iam_role_policy_attachment.ssm_core,
  ]

  tags = { Name = local.prefix }
}

resource "aws_eip_association" "this" {
  count         = var.use_eip ? 1 : 0
  allocation_id = aws_eip.this[0].id
  instance_id   = aws_instance.this.id
}
