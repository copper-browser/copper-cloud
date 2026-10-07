#!/bin/sh
# copper-cloud installer — one command on a fresh Linux VM:
#
#   curl -fsSL https://raw.githubusercontent.com/copper-browser/copper-cloud/main/install.sh | sudo sh
#   # or, from an extracted release tarball:
#   sudo ./install.sh
#
# Supported: Ubuntu 22.04/24.04, Debian 12, Amazon Linux 2023, Fedora (apt or dnf).
# Idempotent: re-running keeps the keys, config, certificate, database and admin accounts;
# it upgrades the binary, re-applies migrations and restarts the service.
#
# A fresh instance gets a portal admin account (https://HOST[:PORT]/) and access_mode
# "directory": only personal access keys minted in the portal pass the gate. The link code
# printed at the end is then a one-account access key for the first Copper.
#
# Inputs (environment, all optional):
#   DATABASE_URL                 use this Postgres instead of installing one locally
#   COPPER_CLOUD_PUBLIC_HOST     host/IP clients connect to (default: detected public IP)
#   COPPER_CLOUD_PORT            HTTPS port (default 443)
#   COPPER_CLOUD_DOMAIN          use Let's Encrypt (ACME) for this domain instead of self-signed
#   COPPER_CLOUD_ACME_EMAIL      contact email for ACME
#   COPPER_CLOUD_ALLOW_SIGNUP    true|false (default true; the first user can always sign up)
#   COPPER_CLOUD_INSTANCE_KEY    instance key (default: generated)
#   COPPER_CLOUD_MASTER_KEY      master key, base64url 32 bytes (default: generated)
#   COPPER_CLOUD_ADMIN_EMAIL     portal admin email (default: admin@<public host>)
#   COPPER_CLOUD_ADMIN_PASSWORD  portal admin password, 10+ chars (default: generated, printed
#                                once and saved to /etc/copper-cloud/admin-credentials, 0600)
#   COPPER_CLOUD_ACCESS_MODE     directory|open for a fresh instance (default directory)
#   COPPER_CLOUD_BINARY          local path to the copper-cloud binary or release .tar.gz
#   COPPER_CLOUD_BINARY_URL      URL of the binary or release .tar.gz
#   COPPER_CLOUD_VERSION         GitHub release to download (default: latest), e.g. 0.8.1
#   GITHUB_TOKEN                 token for downloading from a private GitHub repository
#
# Flags: --uninstall (remove service + binary, keep data), --purge (also delete config,
#        state and the local copper_cloud database), --print-unit, --help.

set -eu

REPO="copper-browser/copper-cloud"
BIN=/usr/local/bin/copper-cloud
ETC=/etc/copper-cloud
CONFIG=$ETC/copper-cloud.toml
TLS_DIR=$ETC/tls
LINK_FILE=$ETC/link-code
ADMIN_FILE=$ETC/admin-credentials
STATE_DIR=/var/lib/copper-cloud
UNIT=/etc/systemd/system/copper-cloud.service
SVC_USER=copper-cloud
DB_NAME=copper_cloud
DB_ROLE=copper_cloud

log() { printf '==> %s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

print_unit() {
	cat <<'UNIT_EOF'
[Unit]
Description=Copper Cloud (sync + canvas server for the Copper browser)
Documentation=https://github.com/copper-browser/copper-cloud
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=copper-cloud
Group=copper-cloud
Environment=COPPER_CLOUD_CONFIG=/etc/copper-cloud/copper-cloud.toml
ExecStart=/usr/local/bin/copper-cloud serve
Restart=always
RestartSec=2
KillSignal=SIGTERM
TimeoutStopSec=20
LimitNOFILE=65536

# Bind :443 without root.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

# Sandboxing. The config + certificate are read-only; ACME state lives in
# /var/lib/copper-cloud (StateDirectory).
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
ProtectHostname=true
ProtectProc=invisible
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged
StateDirectory=copper-cloud
StateDirectoryMode=0750
ReadOnlyPaths=/etc/copper-cloud
UMask=0077

[Install]
WantedBy=multi-user.target
UNIT_EOF
}

usage() {
	sed -n '2,36p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//' || true
}

# Run copper-cloud against the installed config with a clean environment, so installer
# inputs (which double as COPPER_CLOUD_* config overrides) never shadow the config file.
cc() { env -i PATH="$PATH" HOME=/root LANG=C.UTF-8 "$BIN" --config "$CONFIG" "$@"; }

# psql as the postgres superuser over the local socket.
pg() { (cd /tmp && runuser -u postgres -- psql -X -v ON_ERROR_STOP=1 -qtA "$@"); }

random_alnum() { LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c "$1"; }

# ----------------------------------------------------------------------------------------
# Platform

PKG=""
detect_platform() {
	[ "$(id -u)" -eq 0 ] || die "run as root (sudo sh install.sh)"
	have systemctl || die "systemd is required"
	[ -r /etc/os-release ] || die "cannot detect the distribution (/etc/os-release missing)"
	# shellcheck disable=SC1091
	. /etc/os-release
	case " ${ID:-} ${ID_LIKE:-} " in
	*" ubuntu "* | *" debian "*) PKG=apt ;;
	*" amzn "* | *" fedora "* | *" rhel "* | *" centos "*) PKG=dnf ;;
	*) die "unsupported distribution: ${PRETTY_NAME:-$ID} (need apt or dnf)" ;;
	esac
	have dnf || [ "$PKG" != dnf ] || PKG=yum
	case "$(uname -m)" in
	x86_64 | amd64) ARCH=x86_64 ;;
	aarch64 | arm64) ARCH=aarch64 ;;
	*) die "unsupported CPU architecture: $(uname -m)" ;;
	esac
	log "platform: ${PRETTY_NAME:-$ID} ($ARCH, $PKG)"
}

APT_UPDATED=0
pkg_install() {
	case "$PKG" in
	apt)
		if [ "$APT_UPDATED" -eq 0 ]; then
			DEBIAN_FRONTEND=noninteractive apt-get update -qq
			APT_UPDATED=1
		fi
		DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "$@" >/dev/null
		;;
	dnf | yum) "$PKG" install -y -q "$@" >/dev/null ;;
	esac
}

ensure_tools() {
	missing=""
	have curl || missing="$missing curl"
	have tar || missing="$missing tar"
	[ -e /etc/ssl/certs/ca-certificates.crt ] || [ -e /etc/pki/tls/certs/ca-bundle.crt ] || missing="$missing ca-certificates"
	if [ -n "$missing" ]; then
		log "installing:$missing"
		# shellcheck disable=SC2086
		pkg_install $missing
	fi
}

# ----------------------------------------------------------------------------------------
# Binary

SCRIPT_DIR=""
case "$0" in
install.sh | */install.sh) if [ -f "$0" ]; then SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd); fi ;;
esac

TMP=""
cleanup() { if [ -n "$TMP" ]; then rm -rf "$TMP"; fi; }
trap cleanup EXIT INT TERM

http_get() { # url dest [extra curl args...]
	url=$1
	dest=$2
	shift 2
	curl -fsSL --retry 3 --connect-timeout 15 "$@" -o "$dest" "$url"
}

# Place a binary or release tarball ($1) at $BIN atomically.
install_artifact() {
	src=$1
	case "$src" in
	*.tar.gz | *.tgz)
		mkdir -p "$TMP/x"
		tar -xzf "$src" -C "$TMP/x"
		found=$(find "$TMP/x" -type f -name copper-cloud | head -n 1)
		[ -n "$found" ] || die "no copper-cloud binary inside $src"
		src=$found
		;;
	esac
	"$src" version >/dev/null 2>&1 || die "$src is not a runnable copper-cloud binary for this machine"
	install -m 0755 "$src" "$BIN.new"
	mv -f "$BIN.new" "$BIN"
	log "installed $("$BIN" version) at $BIN"
}

# Asset API URL for asset $2 in release JSON $1 (the "url" precedes "name" per asset).
asset_api_url() {
	awk -v name="$2" '
		/"url": *"https:\/\/api\.github\.com\/repos\/.*\/releases\/assets\// { u = $0; sub(/.*"url": *"/, "", u); sub(/".*/, "", u) }
		index($0, "\"name\": \"" name "\"") { print u; exit }
	' "$1"
}

github_download() {
	ver=${COPPER_CLOUD_VERSION:-latest}
	if [ "$ver" = latest ]; then
		api="https://api.github.com/repos/$REPO/releases/latest"
	else
		api="https://api.github.com/repos/$REPO/releases/tags/v${ver#v}"
	fi
	if [ -n "${GITHUB_TOKEN:-}" ]; then
		# Private repository: resolve assets through the API with the token.
		http_get "$api" "$TMP/release.json" -H "Authorization: Bearer $GITHUB_TOKEN" \
			-H "Accept: application/vnd.github+json"
	else
		http_get "$api" "$TMP/release.json" -H "Accept: application/vnd.github+json" ||
			die "could not query $api (private repository? set GITHUB_TOKEN, or use COPPER_CLOUD_BINARY)"
	fi
	tag=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$TMP/release.json" | head -n 1)
	[ -n "$tag" ] || die "could not resolve release $ver of $REPO"
	asset="copper-cloud-${tag#v}-linux-$ARCH.tar.gz"
	log "downloading $asset ($tag)"
	if [ -n "${GITHUB_TOKEN:-}" ]; then
		url=$(asset_api_url "$TMP/release.json" "$asset")
		sum_url=$(asset_api_url "$TMP/release.json" "$asset.sha256")
		[ -n "$url" ] || die "release $tag has no asset $asset"
		http_get "$url" "$TMP/$asset" -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/octet-stream"
		if [ -n "$sum_url" ]; then
			http_get "$sum_url" "$TMP/$asset.sha256" -H "Authorization: Bearer $GITHUB_TOKEN" \
				-H "Accept: application/octet-stream" || true
		fi
	else
		base="https://github.com/$REPO/releases/download/$tag"
		http_get "$base/$asset" "$TMP/$asset" || die "release $tag has no asset $asset"
		http_get "$base/$asset.sha256" "$TMP/$asset.sha256" 2>/dev/null || true
	fi
	if [ -s "$TMP/$asset.sha256" ]; then
		want=$(awk '{print $1}' "$TMP/$asset.sha256")
		got=$(sha256sum "$TMP/$asset" | awk '{print $1}')
		[ "$want" = "$got" ] || die "checksum mismatch for $asset"
		log "checksum ok"
	else
		warn "no checksum published for $asset"
	fi
	install_artifact "$TMP/$asset"
}

install_binary() {
	TMP=$(mktemp -d)
	if [ -n "${COPPER_CLOUD_BINARY:-}" ]; then
		[ -f "$COPPER_CLOUD_BINARY" ] || die "COPPER_CLOUD_BINARY=$COPPER_CLOUD_BINARY does not exist"
		install_artifact "$COPPER_CLOUD_BINARY"
	elif [ -n "$SCRIPT_DIR" ] && [ -x "$SCRIPT_DIR/copper-cloud" ] && [ ! -d "$SCRIPT_DIR/copper-cloud" ]; then
		install_artifact "$SCRIPT_DIR/copper-cloud"
	elif [ -n "${COPPER_CLOUD_BINARY_URL:-}" ]; then
		case "$COPPER_CLOUD_BINARY_URL" in
		*.tar.gz | *.tgz) dest="$TMP/copper-cloud.tar.gz" ;;
		*) dest="$TMP/copper-cloud" ;;
		esac
		log "downloading $COPPER_CLOUD_BINARY_URL"
		http_get "$COPPER_CLOUD_BINARY_URL" "$dest"
		chmod +x "$dest"
		install_artifact "$dest"
	else
		github_download
	fi
}

# ----------------------------------------------------------------------------------------
# PostgreSQL (local, only when DATABASE_URL is not given)

pg_service() {
	for s in postgresql postgresql-16 postgresql-15; do
		if systemctl list-unit-files "$s.service" >/dev/null 2>&1 &&
			systemctl list-unit-files "$s.service" | grep -q "^$s.service"; then
			echo "$s"
			return
		fi
	done
	echo postgresql
}

install_postgres() {
	if ! have psql || ! { have pg_ctlcluster || have postgresql-setup || [ -x /usr/bin/postgres ]; }; then
		log "installing PostgreSQL"
		case "$PKG" in
		apt) pkg_install postgresql ;;
		dnf | yum)
			pkg_install postgresql16-server 2>/dev/null ||
				pkg_install postgresql15-server 2>/dev/null ||
				pkg_install postgresql-server
			;;
		esac
	fi
	if [ "$PKG" != apt ] && have postgresql-setup; then
		# shellcheck disable=SC2016 # expanded by the postgres user's shell
		data=$(runuser -u postgres -- sh -c 'echo ${PGDATA:-/var/lib/pgsql/data}' 2>/dev/null || echo /var/lib/pgsql/data)
		if [ ! -s "$data/PG_VERSION" ]; then
			log "initializing PostgreSQL data directory"
			postgresql-setup --initdb >/dev/null
		fi
	fi
	svc=$(pg_service)
	systemctl enable --now "$svc" >/dev/null 2>&1 || systemctl start "$svc"
	i=0
	until pg -c 'SELECT 1' >/dev/null 2>&1; do
		i=$((i + 1))
		[ "$i" -lt 30 ] || die "PostgreSQL did not come up (systemctl status $svc)"
		sleep 1
	done
}

configure_pg_hba() {
	hba=$(pg -c 'SHOW hba_file')
	[ -f "$hba" ] || die "cannot find pg_hba.conf"
	if ! grep -q 'copper-cloud (install.sh)' "$hba"; then
		log "allowing $DB_ROLE password logins on 127.0.0.1/::1 ($hba)"
		{
			echo "# copper-cloud (install.sh): local TCP only, SCRAM passwords"
			echo "host    $DB_NAME    $DB_ROLE    127.0.0.1/32    scram-sha-256"
			echo "host    $DB_NAME    $DB_ROLE    ::1/128         scram-sha-256"
			cat "$hba"
		} >"$hba.copper-cloud.tmp"
		cat "$hba.copper-cloud.tmp" >"$hba"
		rm -f "$hba.copper-cloud.tmp"
		pg -c 'SELECT pg_reload_conf()' >/dev/null
	fi
}

# Creates role + database; prints the DATABASE_URL.
setup_local_database() {
	install_postgres
	configure_pg_hba
	pw=$(random_alnum 40)
	verb=CREATE
	[ "$(pg -c "SELECT 1 FROM pg_roles WHERE rolname = '$DB_ROLE'")" = 1 ] && verb=ALTER
	# Password goes through stdin, never argv.
	printf "SET password_encryption = 'scram-sha-256';\n%s ROLE %s LOGIN PASSWORD '%s';\n" \
		"$verb" "$DB_ROLE" "$pw" | pg >/dev/null
	if [ "$(pg -c "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'")" != 1 ]; then
		pg -c "CREATE DATABASE $DB_NAME OWNER $DB_ROLE" >/dev/null
	fi
	port=$(pg -c 'SHOW port')
	log "database $DB_NAME ready (role $DB_ROLE)"
	printf 'postgres://%s:%s@127.0.0.1:%s/%s' "$DB_ROLE" "$pw" "${port:-5432}" "$DB_NAME"
}

# ----------------------------------------------------------------------------------------
# Config, TLS, service

detect_public_host() {
	if [ -n "${COPPER_CLOUD_PUBLIC_HOST:-}" ]; then
		echo "$COPPER_CLOUD_PUBLIC_HOST"
		return
	fi
	if [ -n "${COPPER_CLOUD_DOMAIN:-}" ]; then
		echo "$COPPER_CLOUD_DOMAIN"
		return
	fi
	# EC2 (IMDSv2).
	tok=$(curl -fsS -m 2 -X PUT http://169.254.169.254/latest/api/token \
		-H 'X-aws-ec2-metadata-token-ttl-seconds: 60' 2>/dev/null || true)
	if [ -n "$tok" ]; then
		ip=$(curl -fsS -m 2 -H "X-aws-ec2-metadata-token: $tok" \
			http://169.254.169.254/latest/meta-data/public-ipv4 2>/dev/null || true)
		case "$ip" in *.*.*.*) echo "$ip" && return ;; esac
	fi
	for u in https://api.ipify.org https://ifconfig.me/ip https://icanhazip.com; do
		ip=$(curl -fsS -m 5 "$u" 2>/dev/null | tr -d '[:space:]' || true)
		case "$ip" in *.*.*.* | *:*:*) echo "$ip" && return ;; esac
	done
	ip=$(hostname -I 2>/dev/null | awk '{print $1}')
	[ -n "$ip" ] || die "cannot detect the public IP; set COPPER_CLOUD_PUBLIC_HOST"
	warn "using local address $ip (set COPPER_CLOUD_PUBLIC_HOST if clients connect differently)"
	echo "$ip"
}

ensure_user() {
	if ! id "$SVC_USER" >/dev/null 2>&1; then
		nologin=/usr/sbin/nologin
		[ -x "$nologin" ] || nologin=/sbin/nologin
		useradd --system --user-group --home-dir "$STATE_DIR" --no-create-home \
			--shell "$nologin" --comment "copper-cloud server" "$SVC_USER"
		log "created system user $SVC_USER"
	fi
	install -d -m 0750 -o "$SVC_USER" -g "$SVC_USER" "$STATE_DIR"
	install -d -m 0750 -o root -g "$SVC_USER" "$ETC"
}

write_config() {
	if [ -f "$CONFIG" ]; then
		log "keeping existing config $CONFIG (keys and database unchanged)"
		return
	fi
	port=${COPPER_CLOUD_PORT:-443}
	host=$(detect_public_host)
	case "$host" in *:*:*) url_host="[$host]" ;; *) url_host=$host ;; esac
	if [ "$port" = 443 ]; then public_url=$url_host; else public_url="$url_host:$port"; fi
	tls_mode=self-signed
	set -- --tls-mode self-signed
	if [ -n "${COPPER_CLOUD_DOMAIN:-}" ]; then
		tls_mode=acme
		[ "$port" = 443 ] || warn "ACME (TLS-ALPN-01) needs port 443 reachable from the internet"
		set -- --tls-mode acme --domain "$COPPER_CLOUD_DOMAIN"
		[ -z "${COPPER_CLOUD_ACME_EMAIL:-}" ] || set -- "$@" --acme-email "$COPPER_CLOUD_ACME_EMAIL"
	fi
	if [ -n "${DATABASE_URL:-}" ]; then
		db_url=$DATABASE_URL
		log "using external database from DATABASE_URL"
	else
		db_url=$(setup_local_database)
	fi
	log "writing $CONFIG (public host $public_url, tls $tls_mode)"
	env -i PATH="$PATH" HOME=/root \
		COPPER_CLOUD_DATABASE_URL="$db_url" \
		COPPER_CLOUD_INSTANCE_KEY="${COPPER_CLOUD_INSTANCE_KEY:-}" \
		COPPER_CLOUD_MASTER_KEY="${COPPER_CLOUD_MASTER_KEY:-}" \
		"$BIN" init-config --write "$CONFIG" \
		--public-url "$public_url" \
		--listen "0.0.0.0:$port" \
		--allow-signup "${COPPER_CLOUD_ALLOW_SIGNUP:-true}" \
		--tls-dir "$TLS_DIR" \
		"$@" >/dev/null
}

secure_files() {
	chown "$SVC_USER:$SVC_USER" "$CONFIG"
	chmod 0600 "$CONFIG"
	if [ -d "$TLS_DIR" ]; then
		chown -R "root:$SVC_USER" "$TLS_DIR"
		chmod 0750 "$TLS_DIR"
		[ ! -f "$TLS_DIR/key.pem" ] || chmod 0640 "$TLS_DIR/key.pem"
		[ ! -f "$TLS_DIR/cert.pem" ] || chmod 0644 "$TLS_DIR/cert.pem"
	fi
}

open_firewall() {
	port=$(sed -n 's/^listen *= *"[^"]*:\([0-9]*\)".*/\1/p' "$CONFIG" | head -n 1)
	port=${port:-443}
	if have ufw && ufw status 2>/dev/null | grep -q '^Status: active'; then
		ufw allow "$port/tcp" >/dev/null && log "ufw: allowed $port/tcp"
	fi
	if have firewall-cmd && firewall-cmd --state >/dev/null 2>&1; then
		firewall-cmd --quiet --permanent --add-port="$port/tcp" && firewall-cmd --quiet --reload &&
			log "firewalld: allowed $port/tcp"
	fi
}

install_service() {
	tmp_unit=$(mktemp)
	if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/packaging/copper-cloud.service" ]; then
		cp "$SCRIPT_DIR/packaging/copper-cloud.service" "$tmp_unit"
	else
		print_unit >"$tmp_unit"
	fi
	install -m 0644 "$tmp_unit" "$UNIT"
	rm -f "$tmp_unit"
	systemctl daemon-reload
	systemctl enable copper-cloud.service >/dev/null 2>&1
}

# ----------------------------------------------------------------------------------------
# Admin portal + access mode

# public_url from the config: host[:port] as clients see it.
config_public_url() { sed -n 's/^public_url *= *"\([^"]*\)".*/\1/p' "$CONFIG" | head -n 1; }

default_admin_email() {
	host=$(config_public_url)
	case "$host" in
	\[*) host="" ;; # IPv6 literal: not usable as an email domain
	*) host=${host%:*} ;;
	esac
	case "$host" in
	*.*) printf 'admin@%s' "$host" ;;
	*) printf 'admin@copper-cloud.local' ;;
	esac
}

# "N thing(s)" summary line of an admin listing → N.
count_of() { sed -n 's/^\([0-9][0-9]*\) [a-z]*(s)$/\1/p' | tail -n 1; }

# Creates the portal admin unless one exists. On a fresh instance (no admin and no user
# yet) also sets the access mode. Sets ADMIN_CREATED / ADMIN_EMAIL / ADMIN_PASSWORD /
# ADMIN_GENERATED for the summary.
ADMIN_CREATED=0
ADMIN_GENERATED=0
setup_admin() {
	ADMIN_EMAIL=${COPPER_CLOUD_ADMIN_EMAIL:-$(default_admin_email)}
	admins=$(cc admin list-admins)
	admin_count=$(printf '%s\n' "$admins" | count_of)
	if printf '%s\n' "$admins" | awk -v e="$ADMIN_EMAIL" 'NR > 1 && tolower($1) == tolower(e) { f = 1 } END { exit !f }'; then
		log "portal admin $ADMIN_EMAIL exists (password unchanged)"
		return
	fi
	if [ "${admin_count:-0}" != 0 ] && [ -z "${COPPER_CLOUD_ADMIN_EMAIL:-}" ]; then
		log "keeping the existing portal admin account(s)"
		return
	fi
	user_count=$(cc admin users | count_of)
	if [ -n "${COPPER_CLOUD_ADMIN_PASSWORD:-}" ]; then
		ADMIN_PASSWORD=$COPPER_CLOUD_ADMIN_PASSWORD
	else
		ADMIN_PASSWORD=$(random_alnum 24)
		ADMIN_GENERATED=1
	fi
	printf '%s\n' "$ADMIN_PASSWORD" |
		cc admin create-admin --email "$ADMIN_EMAIL" --password-stdin --if-missing >/dev/null
	ADMIN_CREATED=1
	(
		umask 077
		printf 'url=https://%s/\nemail=%s\npassword=%s\n' \
			"$(config_public_url)" "$ADMIN_EMAIL" "$ADMIN_PASSWORD" >"$ADMIN_FILE.tmp"
		mv -f "$ADMIN_FILE.tmp" "$ADMIN_FILE"
	)
	chmod 0600 "$ADMIN_FILE"
	log "created portal admin $ADMIN_EMAIL (credentials in $ADMIN_FILE)"
	if [ "${admin_count:-0}" = 0 ] && [ "${user_count:-0}" = 0 ]; then
		mode=${COPPER_CLOUD_ACCESS_MODE:-directory}
		case "$mode" in
		open | directory) ;;
		*) die "COPPER_CLOUD_ACCESS_MODE must be open or directory" ;;
		esac
		cc admin set-access-mode "$mode" >/dev/null
		log "access mode: $mode"
	elif [ "${user_count:-0}" != 0 ]; then
		log "existing users found: access mode left as $(cc admin access-mode) (switch in the portal › Settings)"
	fi
}

# The link code to hand to the first Copper: the instance link code in open mode; in
# directory mode a one-account access key (kept across re-runs, revocable in the portal).
write_link_code() {
	umask 077
	if [ "$(cc admin access-mode)" = directory ]; then
		if [ -f "$LINK_FILE" ] && grep -q '#k=ck_' "$LINK_FILE"; then
			return
		fi
		cc admin create-access-key --label "Installer link code" --max-uses 1 2>/dev/null |
			tail -n 1 >"$LINK_FILE.tmp"
		grep -q '^copper-cloud://' "$LINK_FILE.tmp" || die "could not mint the installer access key"
	else
		cc link-code >"$LINK_FILE.tmp"
	fi
	mv -f "$LINK_FILE.tmp" "$LINK_FILE"
	chmod 0600 "$LINK_FILE"
}

uninstall() {
	purge=$1
	detect_platform
	log "stopping and removing the service"
	systemctl disable --now copper-cloud.service >/dev/null 2>&1 || true
	rm -f "$UNIT"
	systemctl daemon-reload
	rm -f "$BIN"
	if [ "$purge" = 1 ]; then
		if have psql && id postgres >/dev/null 2>&1 && pg -c 'SELECT 1' >/dev/null 2>&1; then
			if [ "$(pg -c "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'")" = 1 ]; then
				pg -c "DROP DATABASE $DB_NAME" >/dev/null && log "dropped local database $DB_NAME"
			fi
			if [ "$(pg -c "SELECT 1 FROM pg_roles WHERE rolname = '$DB_ROLE'")" = 1 ]; then
				pg -c "DROP ROLE $DB_ROLE" >/dev/null && log "dropped role $DB_ROLE"
			fi
		fi
		rm -rf "$ETC" "$STATE_DIR"
		userdel "$SVC_USER" >/dev/null 2>&1 || true
		log "purged config, keys, certificate and state"
	else
		log "kept $ETC (config + keys) and the database; use --purge to delete them"
	fi
}

main() {
	case "${1:-}" in
	--help | -h)
		usage
		exit 0
		;;
	--print-unit)
		print_unit
		exit 0
		;;
	--uninstall)
		uninstall 0
		exit 0
		;;
	--purge)
		uninstall 1
		exit 0
		;;
	"") ;;
	*) die "unknown option: $1 (see --help)" ;;
	esac

	detect_platform
	ensure_tools
	upgrading=0
	[ -f "$CONFIG" ] && upgrading=1
	install_binary
	ensure_user
	write_config
	if [ "$upgrading" = 1 ] && [ -z "${DATABASE_URL:-}" ] && grep -q '@127.0.0.1:' "$CONFIG"; then
		svc=$(pg_service)
		systemctl start "$svc" 2>/dev/null || true
	fi
	cc tls-init >/dev/null
	secure_files
	log "applying database migrations"
	cc migrate
	setup_admin
	install_service
	open_firewall
	log "starting copper-cloud"
	systemctl restart copper-cloud.service

	wait_secs=60
	grep -q '^mode = "acme"' "$CONFIG" && wait_secs=180
	if ! cc healthcheck --wait "$wait_secs" >/dev/null 2>&1; then
		journalctl -u copper-cloud.service -n 30 --no-pager >&2 || true
		if grep -q '^mode = "acme"' "$CONFIG"; then
			warn "not answering yet — ACME issuance can take a few minutes (journalctl -u copper-cloud -f)"
		else
			die "copper-cloud did not become healthy (systemctl status copper-cloud)"
		fi
	fi

	write_link_code
	cc doctor >/dev/null 2>&1 || warn "copper-cloud doctor reported problems: run 'sudo copper-cloud --config $CONFIG doctor'"
	mode=$(cc admin access-mode)

	echo
	echo "copper-cloud is running."
	echo
	echo "Admin portal: https://$(config_public_url)/"
	if [ "$ADMIN_CREATED" = 1 ]; then
		echo "  email:    $ADMIN_EMAIL"
		if [ "$ADMIN_GENERATED" = 1 ]; then
			echo "  password: $ADMIN_PASSWORD   (shown once; also in $ADMIN_FILE)"
		else
			echo "  password: as given in COPPER_CLOUD_ADMIN_PASSWORD (also in $ADMIN_FILE)"
		fi
	else
		echo "  sign in with your existing admin account (reset: sudo copper-cloud admin reset-admin-password --email …)"
	fi
	grep -q '^mode = "self-signed"' "$CONFIG" &&
		echo "  (self-signed certificate: your browser will warn once; fingerprint below in the link code)"
	echo
	echo "Access mode: $mode"
	if [ "$mode" = directory ]; then
		echo "  Only personal access keys pass the gate: mint one per person in the portal › Access keys."
		echo "  Link code for your first Copper (Settings › Cloud › Connect; creates one account):"
	else
		echo "  Anyone with the instance link code may connect. Link code (Settings › Cloud › Connect):"
	fi
	echo
	echo "  $(cat "$LINK_FILE")"
	echo
	echo "Saved to $LINK_FILE (root only). Keep it secret: it contains a gate key."
	echo "Health: sudo copper-cloud --config $CONFIG doctor"
}

main "$@"
