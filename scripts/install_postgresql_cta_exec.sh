#!/usr/bin/env bash
set -Eeuo pipefail

readonly EXPECTED_HOST="el-jp-yf-srv-7-6"
readonly EXPECTED_USER="el01"
readonly EXPECTED_RUNTIME="/home/el01/binance_exec_trade01"
readonly PG_MAJOR="16"
readonly PG_VERSION="16.15"
readonly PG_PORT="15432"
readonly APP_USER="crypto_cta_manager"
readonly APP_DATABASE="crypto_cta_manager"
readonly PREFIX="/home/el01/.local/opt/postgresql/$PG_MAJOR"
readonly DATA_DIR="/home/el01/.local/var/lib/postgresql/$PG_MAJOR/cta"
readonly RUN_DIR="/home/el01/.local/run/postgresql"
readonly LOG_DIR="/home/el01/.local/var/log/postgresql"
readonly ENV_DIR="/home/el01/.config/crypto-cta-manager"
readonly ENV_FILE="$ENV_DIR/database.env"
readonly UNIT_DIR="/home/el01/.config/systemd/user"
readonly UNIT_FILE="$UNIT_DIR/postgresql-cta.service"

usage() {
    echo "Usage: $0 <postgresql-user-bundle.tar.gz>" >&2
    exit 2
}

[[ $# -eq 1 ]] || usage
bundle_path=$(realpath "$1")
checksum_path="$bundle_path.sha256"

if [[ $(id -un) != "$EXPECTED_USER" ]]; then
    echo "Run this installer as $EXPECTED_USER without sudo." >&2
    exit 1
fi
if [[ $(hostname -f) != "$EXPECTED_HOST" ]]; then
    echo "Refusing host $(hostname -f); expected $EXPECTED_HOST." >&2
    exit 1
fi
if [[ ! -d $EXPECTED_RUNTIME ]]; then
    echo "Expected Exec runtime is missing: $EXPECTED_RUNTIME" >&2
    exit 1
fi

source /etc/os-release
if [[ $ID != "ubuntu" || $VERSION_ID != "24.04" || $(uname -m) != "x86_64" ]]; then
    echo "Expected Ubuntu 24.04 x86_64; found $ID $VERSION_ID $(uname -m)." >&2
    exit 1
fi

for command_name in install ldd openssl realpath sha256sum ss systemctl tar; do
    if ! command -v "$command_name" >/dev/null; then
        echo "Missing required command: $command_name" >&2
        exit 1
    fi
done
if [[ ! -r $bundle_path || ! -r $checksum_path ]]; then
    echo "Bundle or checksum file is missing." >&2
    exit 1
fi
(
    cd "$(dirname "$bundle_path")"
    sha256sum --check --status "$(basename "$checksum_path")"
)

if [[ -e $PREFIX || -e $DATA_DIR || -e $UNIT_FILE || -e $ENV_FILE ]]; then
    echo "A PostgreSQL installation already exists; refusing to overwrite it." >&2
    exit 1
fi
if ss -H -ltn | awk '{print $4}' | grep -Eq "(^|:)$PG_PORT$"; then
    echo "TCP port $PG_PORT is already in use." >&2
    exit 1
fi

install -d -m 0755 "$(dirname "$PREFIX")"
stage_dir=$(mktemp -d "$(dirname "$PREFIX")/.postgresql-install.XXXXXX")
cleanup() {
    rm -rf -- "$stage_dir"
}
trap cleanup EXIT

tar -xzf "$bundle_path" -C "$stage_dir"
if [[ ! -x $stage_dir/$PG_MAJOR/bin/postgres ]]; then
    echo "Bundle does not contain PostgreSQL $PG_MAJOR." >&2
    exit 1
fi
mv "$stage_dir/$PG_MAJOR" "$PREFIX"
chmod -R go-w "$PREFIX"

for binary_name in initdb pg_ctl pg_isready postgres psql; do
    binary_path="$PREFIX/bin/$binary_name"
    if ldd "$binary_path" | grep -q 'not found'; then
        echo "Unresolved runtime library for $binary_path:" >&2
        ldd "$binary_path" >&2
        exit 1
    fi
done
if [[ $($PREFIX/bin/postgres --version) != "postgres (PostgreSQL) $PG_VERSION" ]]; then
    echo "Unexpected PostgreSQL binary version." >&2
    exit 1
fi

install -d -m 0700 "$DATA_DIR" "$RUN_DIR" "$LOG_DIR" "$ENV_DIR"
install -d -m 0755 "$UNIT_DIR"

"$PREFIX/bin/initdb" \
    --pgdata="$DATA_DIR" \
    --username="$EXPECTED_USER" \
    --encoding=UTF8 \
    --locale=C.UTF-8 \
    --auth-local=peer \
    --auth-host=scram-sha-256 \
    --data-checksums \
    --no-instructions

cat >>"$DATA_DIR/postgresql.conf" <<EOF

# crypto_cta_manager local Exec database
listen_addresses = '127.0.0.1'
port = $PG_PORT
unix_socket_directories = '$RUN_DIR'
unix_socket_permissions = 0700
password_encryption = 'scram-sha-256'
ssl = off
timezone = 'UTC'
log_timezone = 'UTC'

max_connections = 30
shared_buffers = '128MB'
effective_cache_size = '1GB'
work_mem = '4MB'
maintenance_work_mem = '64MB'
checkpoint_completion_target = 0.9
max_wal_size = '1GB'
jit = off

fsync = on
synchronous_commit = on
full_page_writes = on

logging_collector = on
log_directory = '$LOG_DIR'
log_filename = 'postgresql-%Y-%m-%d_%H%M%S.log'
log_rotation_age = '1d'
log_rotation_size = '100MB'
log_line_prefix = '%m [%p] %q%u@%d '
log_min_duration_statement = '1s'
log_checkpoints = on
log_lock_waits = on
EOF

cat >"$DATA_DIR/pg_hba.conf" <<EOF
local   all                    $EXPECTED_USER                         peer
local   all                    all                                    scram-sha-256
host    $APP_DATABASE          $APP_USER        127.0.0.1/32          scram-sha-256
EOF
chmod 0600 "$DATA_DIR/postgresql.conf" "$DATA_DIR/pg_hba.conf"

cat >"$UNIT_FILE" <<EOF
[Unit]
Description=PostgreSQL $PG_MAJOR for local CTA Exec persistence
After=network.target

[Service]
Type=simple
Environment=LC_ALL=C.UTF-8
Environment=PGDATA=$DATA_DIR
ExecStart=$PREFIX/bin/postgres -D $DATA_DIR
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
RestartSec=5s
KillSignal=SIGINT
TimeoutStopSec=120s
SendSIGKILL=no
Nice=10
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
LimitNOFILE=65536

[Install]
WantedBy=default.target
EOF
chmod 0644 "$UNIT_FILE"

systemctl --user daemon-reload
systemctl --user enable --now postgresql-cta.service

wait_until_ready() {
    for _ in {1..30}; do
        if "$PREFIX/bin/pg_isready" -q \
            -h "$RUN_DIR" -p "$PG_PORT" -U "$EXPECTED_USER" -d postgres; then
            return 0
        fi
        sleep 1
    done
    return 1
}
wait_until_ready

database_password=$(openssl rand -hex 32)
"$PREFIX/bin/psql" \
    -h "$RUN_DIR" -p "$PG_PORT" -U "$EXPECTED_USER" -d postgres \
    -v ON_ERROR_STOP=1 -v app_password="$database_password" <<'SQL'
SELECT format(
    'CREATE ROLE crypto_cta_manager LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION PASSWORD %L',
    :'app_password'
)
WHERE NOT EXISTS (
    SELECT FROM pg_catalog.pg_roles WHERE rolname = 'crypto_cta_manager'
)
\gexec
SELECT 'CREATE DATABASE crypto_cta_manager OWNER crypto_cta_manager'
WHERE NOT EXISTS (
    SELECT FROM pg_catalog.pg_database WHERE datname = 'crypto_cta_manager'
)
\gexec
ALTER DATABASE crypto_cta_manager SET timezone TO 'UTC';
SQL

umask 077
cat >"$ENV_FILE" <<EOF
CRYPTO_CTA_LOCAL_DATABASE_URL=postgresql://$APP_USER:$database_password@127.0.0.1:$PG_PORT/$APP_DATABASE
EOF
chmod 0600 "$ENV_FILE"

PGPASSWORD="$database_password" "$PREFIX/bin/psql" \
    -h 127.0.0.1 -p "$PG_PORT" -U "$APP_USER" -d "$APP_DATABASE" \
    -v ON_ERROR_STOP=1 -Atqc \
    "SELECT current_user, current_database(), current_setting('data_checksums');"

systemctl --user restart postgresql-cta.service
wait_until_ready
PGPASSWORD="$database_password" "$PREFIX/bin/psql" \
    -h 127.0.0.1 -p "$PG_PORT" -U "$APP_USER" -d "$APP_DATABASE" \
    -v ON_ERROR_STOP=1 -Atqc "SELECT 1;"

echo "PostgreSQL $PG_VERSION is ready on 127.0.0.1:$PG_PORT."
echo "Data: $DATA_DIR"
echo "Credentials: $ENV_FILE (mode 0600)"
