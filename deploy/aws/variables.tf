variable "name" {
  description = "Deployment name. Prefixes every resource (copper-cloud-<name>), so several deployments can coexist in one account/region."
  type        = string

  validation {
    # RDS identifiers forbid "--"; the S3 bucket name copper-cloud-<name>-<account_id> must stay <= 63 chars.
    condition     = can(regex("^[a-z][a-z0-9-]{0,22}[a-z0-9]$", var.name)) && !strcontains(var.name, "--")
    error_message = "name must be 2-24 chars of lowercase letters, digits and single hyphens, start with a letter and end with a letter or digit."
  }
}

variable "region" {
  description = "AWS region."
  type        = string
  default     = "us-east-1"
}

variable "instance_type" {
  description = "EC2 instance type. The Ubuntu 24.04 AMI architecture (arm64 vs amd64) is derived from it; the release tarball must match."
  type        = string
  default     = "t3.small"
}

variable "db_instance_class" {
  description = "RDS instance class for PostgreSQL 16."
  type        = string
  default     = "db.t4g.micro"
}

variable "binary_path" {
  description = "Local path to the release tarball copper-cloud-<ver>-linux-<x86_64|aarch64>.tar.gz (contains copper-cloud, install.sh, packaging/copper-cloud.service). Uploaded to S3 and installed by cloud-init."
  type        = string

  validation {
    condition     = can(regex("\\.tar\\.gz$", var.binary_path))
    error_message = "binary_path must point at the release .tar.gz."
  }
}

variable "domain" {
  description = "Optional DNS name (e.g. cloud.example.com). When set, TLS uses ACME and the link code has no fingerprint. Point an A record at the EIP BEFORE setting this."
  type        = string
  default     = ""
}

variable "acme_email" {
  description = "Contact email for ACME (Let's Encrypt). Used only when domain is set."
  type        = string
  default     = ""
}

variable "allow_ssh_cidr" {
  description = "Optional CIDR allowed to reach 22/tcp (e.g. 203.0.113.4/32). Empty = no SSH ingress; use SSM Session Manager."
  type        = string
  default     = ""

  validation {
    condition     = var.allow_ssh_cidr == "" || can(cidrhost(var.allow_ssh_cidr, 0))
    error_message = "allow_ssh_cidr must be empty or a valid CIDR."
  }
}

variable "ssh_key_name" {
  description = "Optional existing EC2 key pair name (only useful together with allow_ssh_cidr)."
  type        = string
  default     = ""
}

variable "allow_signup" {
  description = "Allow self-service account signup on the instance (COPPER_CLOUD_ALLOW_SIGNUP)."
  type        = bool
  default     = true
}

variable "use_eip" {
  description = "Allocate an Elastic IP (stable address). Set false when the account is at its EIP quota; the VM then uses its auto-assigned public IPv4, which changes if the instance is stopped and started."
  type        = bool
  default     = true
}
