terraform {
  # Local backend only. up.sh/down.sh pass `-state=state/<name>/terraform.tfstate`
  # so every named deployment has its own state file next to its tfvars and N
  # deployments can be applied in parallel from this one directory.
  required_version = ">= 1.6.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.6"
    }
  }
}

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      Project    = "copper-cloud"
      Deployment = var.name
      ManagedBy  = "terraform"
    }
  }
}
