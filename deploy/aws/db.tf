data "aws_rds_engine_version" "postgres" {
  engine  = "postgres"
  version = "16"
  latest  = true
}

resource "random_password" "db" {
  length  = 32
  special = false # alphanumerics only: safe inside a postgres:// URL without escaping
}

resource "aws_db_subnet_group" "this" {
  name       = local.prefix
  subnet_ids = data.aws_subnets.default.ids
  tags       = { Name = local.prefix }
}

resource "aws_db_instance" "this" {
  identifier     = local.prefix
  engine         = "postgres"
  engine_version = data.aws_rds_engine_version.postgres.version_actual
  instance_class = var.db_instance_class

  db_name  = local.db_name
  username = local.db_user
  password = random_password.db.result
  port     = 5432

  allocated_storage = 20
  storage_type      = "gp3"
  storage_encrypted = true

  db_subnet_group_name   = aws_db_subnet_group.this.name
  vpc_security_group_ids = [aws_security_group.db.id]
  publicly_accessible    = false
  multi_az               = false

  backup_retention_period    = 1
  skip_final_snapshot        = true
  deletion_protection        = false
  apply_immediately          = true
  auto_minor_version_upgrade = true

  # The minor version is picked by the data source at create time; don't fight
  # RDS auto minor upgrades afterwards.
  lifecycle {
    ignore_changes = [engine_version]
  }

  tags = { Name = local.prefix }
}

locals {
  db_name = "copper_cloud"
  db_user = "copper_cloud"
  database_url = format(
    "postgres://%s:%s@%s:%d/%s?sslmode=require",
    local.db_user, random_password.db.result, aws_db_instance.this.address, aws_db_instance.this.port, local.db_name,
  )
}
