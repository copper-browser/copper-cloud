data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}

data "aws_vpc" "default" {
  default = true
}

data "aws_subnets" "default" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.default.id]
  }
  filter {
    name   = "default-for-az"
    values = ["true"]
  }
}

data "aws_subnet" "default" {
  for_each = toset(data.aws_subnets.default.ids)
  id       = each.value
}

# Not every AZ offers every instance type (us-east-1e has no t3/t4g, for
# example), so the EC2 instance goes into the first AZ (sorted, deterministic)
# that has a default subnet AND offers var.instance_type. RDS picks its own AZ
# from the subnet group (it skips AZs where the class isn't orderable).
data "aws_ec2_instance_type_offerings" "app" {
  location_type = "availability-zone"
  filter {
    name   = "instance-type"
    values = [var.instance_type]
  }
}

locals {
  prefix = "copper-cloud-${var.name}"

  subnet_by_az = { for s in data.aws_subnet.default : s.availability_zone => s.id }
  usable_azs = sort(setintersection(
    toset(keys(local.subnet_by_az)),
    toset(data.aws_ec2_instance_type_offerings.app.locations),
  ))
  az        = local.usable_azs[0]
  subnet_id = local.subnet_by_az[local.az]
}

resource "aws_security_group" "app" {
  name        = "${local.prefix}-app"
  description = "copper-cloud ${var.name}: public HTTPS (and optional SSH)"
  vpc_id      = data.aws_vpc.default.id
  tags        = { Name = "${local.prefix}-app" }
}

resource "aws_vpc_security_group_ingress_rule" "app_https_v4" {
  security_group_id = aws_security_group.app.id
  description       = "HTTPS (IPv4)"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_ingress_rule" "app_https_v6" {
  security_group_id = aws_security_group.app.id
  description       = "HTTPS (IPv6)"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv6         = "::/0"
}

resource "aws_vpc_security_group_ingress_rule" "app_ssh" {
  count             = var.allow_ssh_cidr == "" ? 0 : 1
  security_group_id = aws_security_group.app.id
  description       = "SSH (optional)"
  ip_protocol       = "tcp"
  from_port         = 22
  to_port           = 22
  cidr_ipv4         = var.allow_ssh_cidr
}

resource "aws_vpc_security_group_egress_rule" "app_all_v4" {
  security_group_id = aws_security_group.app.id
  description       = "All egress (apt, snap, S3, SSM, ACME, RDS)"
  ip_protocol       = "-1"
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_egress_rule" "app_all_v6" {
  security_group_id = aws_security_group.app.id
  description       = "All egress (IPv6)"
  ip_protocol       = "-1"
  cidr_ipv6         = "::/0"
}

resource "aws_security_group" "db" {
  name        = "${local.prefix}-db"
  description = "copper-cloud ${var.name}: PostgreSQL from the app SG only"
  vpc_id      = data.aws_vpc.default.id
  tags        = { Name = "${local.prefix}-db" }
}

resource "aws_vpc_security_group_ingress_rule" "db_from_app" {
  security_group_id            = aws_security_group.db.id
  description                  = "PostgreSQL from copper-cloud app"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.app.id
}
