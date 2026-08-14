#!/usr/bin/env bash
set -Eeuo pipefail

readonly EXPECTED_HOST="el-powerleader-GPU2080ti-30-42"
readonly PG_MAJOR="16"
readonly CLUSTER_NAME="cta"
readonly PG_PORT="5432"
readonly SERVER_IP="172.16.30.42"
readonly CLIENT_IP="172.16.30.38"
readonly STORAGE_ROOT="/mnt/Data/postgresql"
readonly VERSION_ROOT="$STORAGE_ROOT/$PG_MAJOR"
readonly DATA_DIR="$VERSION_ROOT/$CLUSTER_NAME"
readonly RUN_DIR="$VERSION_ROOT/run"
readonly LOG_DIR="$VERSION_ROOT/log"
readonly LOG_FILE="$LOG_DIR/postgresql-$PG_MAJOR-$CLUSTER_NAME.log"
readonly APP_USER="crypto_cta_manager"
readonly APP_DATABASE="crypto_cta_manager"
readonly ENV_DIR="/home/fanghaizhou/.config/crypto-cta-manager"
readonly ENV_FILE="$ENV_DIR/database.env"
readonly PGDG_KEY="/etc/apt/keyrings/postgresql.gpg"
readonly PGDG_SOURCE="/etc/apt/sources.list.d/pgdg.list"
readonly PGDG_MIRROR="https://mirrors.cloud.tencent.com/postgresql/repos/apt"
readonly PGDG_FINGERPRINT="B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8"

if [[ $EUID -ne 0 ]]; then
    echo "Run this installer with sudo." >&2
    exit 1
fi

if [[ $(hostname) != "$EXPECTED_HOST" ]]; then
    echo "Refusing host $(hostname); expected $EXPECTED_HOST." >&2
    exit 1
fi

source /etc/os-release
if [[ $ID != "ubuntu" || $VERSION_CODENAME != "jammy" ]]; then
    echo "Expected Ubuntu jammy; found $ID $VERSION_CODENAME." >&2
    exit 1
fi

if [[ ! -d /mnt/Data ]] || ! mountpoint -q /mnt/Data; then
    echo "/mnt/Data is not mounted; refusing to use the root disk." >&2
    exit 1
fi

if ss -lnt | awk '{print $4}' | grep -Eq "(^|:)$PG_PORT$"; then
    echo "TCP port $PG_PORT is already in use." >&2
    exit 1
fi

if [[ -e $DATA_DIR ]] && find "$DATA_DIR" -mindepth 1 -print -quit | grep -q .; then
    echo "PostgreSQL data directory is not empty: $DATA_DIR" >&2
    exit 1
fi

for command_name in curl gpg openssl; do
    if ! command -v "$command_name" >/dev/null; then
        echo "Missing required command: $command_name" >&2
        exit 1
    fi
done

work_dir=$(mktemp -d)
createcluster_backup=""
cleanup() {
    if [[ -n $createcluster_backup && -f $createcluster_backup ]]; then
        install -m 0644 "$createcluster_backup" \
            /etc/postgresql-common/createcluster.conf
    fi
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

install -d -m 0755 /etc/apt/keyrings
curl --noproxy '*' --fail --silent --show-error --location \
    --connect-timeout 10 --max-time 60 \
    https://www.postgresql.org/media/keys/ACCC4CF8.asc \
    -o "$work_dir/postgresql.asc"

actual_fingerprint=$(
    gpg --batch --show-keys --with-colons "$work_dir/postgresql.asc" |
        awk -F: '$1 == "fpr" {print $10; exit}'
)
if [[ $actual_fingerprint != "$PGDG_FINGERPRINT" ]]; then
    echo "Unexpected PostgreSQL key fingerprint: $actual_fingerprint" >&2
    exit 1
fi

gpg --batch --dearmor --yes --output "$work_dir/postgresql.gpg" \
    "$work_dir/postgresql.asc"
install -m 0644 "$work_dir/postgresql.gpg" "$PGDG_KEY"
printf '%s\n' \
    "deb [signed-by=$PGDG_KEY] $PGDG_MIRROR jammy-pgdg main" \
    >"$PGDG_SOURCE"

apt-get update
apt-get -s install postgresql-common \
    "postgresql-$PG_MAJOR" "postgresql-client-$PG_MAJOR" \
    >"$work_dir/install-plan.txt"
if grep -Eq '^Remv ' "$work_dir/install-plan.txt"; then
    grep -E '^(Remv|Inst) ' "$work_dir/install-plan.txt" >&2
    echo "Package simulation proposed removals; aborting." >&2
    exit 1
fi

DEBIAN_FRONTEND=noninteractive apt-get install -y postgresql-common

createcluster_backup="$work_dir/createcluster.conf"
cp -a /etc/postgresql-common/createcluster.conf "$createcluster_backup"
if grep -Eq '^[[:space:]]*create_main_cluster[[:space:]]*=' \
    /etc/postgresql-common/createcluster.conf; then
    sed -Ei \
        's/^[[:space:]]*create_main_cluster[[:space:]]*=.*/create_main_cluster = false/' \
        /etc/postgresql-common/createcluster.conf
else
    printf '\ncreate_main_cluster = false\n' \
        >>/etc/postgresql-common/createcluster.conf
fi


DEBIAN_FRONTEND=noninteractive apt-get install -y \
    "postgresql-$PG_MAJOR" "postgresql-client-$PG_MAJOR"

install -m 0644 "$createcluster_backup" \
    /etc/postgresql-common/createcluster.conf
createcluster_backup=""

if pg_lsclusters --no-header 2>/dev/null |
    awk -v version="$PG_MAJOR" -v cluster="$CLUSTER_NAME" \
        '$1 == version && $2 == cluster {found = 1} END {exit !found}'; then
    echo "Cluster $PG_MAJOR/$CLUSTER_NAME already exists; refusing overwrite." >&2
    exit 1
fi

install -d -o root -g root -m 0755 "$STORAGE_ROOT" "$VERSION_ROOT"
install -d -o postgres -g postgres -m 0700 "$DATA_DIR"
install -d -o postgres -g postgres -m 0755 "$RUN_DIR"
install -d -o postgres -g postgres -m 0750 "$LOG_DIR"

pg_createcluster "$PG_MAJOR" "$CLUSTER_NAME" \
    --datadir="$DATA_DIR" \
    --socketdir="$RUN_DIR" \
    --logfile="$LOG_FILE" \
    --port="$PG_PORT" \
    --start-conf=auto \
    --locale=C.UTF-8 \
    -- --encoding=UTF8

install -d -o root -g postgres -m 0750 \
    "/etc/postgresql/$PG_MAJOR/$CLUSTER_NAME/conf.d"
cat >"/etc/postgresql/$PG_MAJOR/$CLUSTER_NAME/conf.d/crypto_cta_manager.conf" <<EOF
listen_addresses = '127.0.0.1,$SERVER_IP'
port = $PG_PORT
unix_socket_directories = '$RUN_DIR'
unix_socket_permissions = 0777
password_encryption = 'scram-sha-256'
timezone = 'UTC'
log_timezone = 'UTC'
EOF
chmod 0644 \
    "/etc/postgresql/$PG_MAJOR/$CLUSTER_NAME/conf.d/crypto_cta_manager.conf"

cat >"/etc/postgresql/$PG_MAJOR/$CLUSTER_NAME/pg_hba.conf" <<EOF
local   all                    postgres                              peer
local   all                    all                                   peer
host    all                    all              127.0.0.1/32          scram-sha-256
host    $APP_DATABASE          $APP_USER        $SERVER_IP/32         scram-sha-256
host    $APP_DATABASE          $APP_USER        $CLIENT_IP/32         scram-sha-256
EOF
chmod 0640 "/etc/postgresql/$PG_MAJOR/$CLUSTER_NAME/pg_hba.conf"
chown root:postgres "/etc/postgresql/$PG_MAJOR/$CLUSTER_NAME/pg_hba.conf"

systemctl enable postgresql.service
pg_ctlcluster "$PG_MAJOR" "$CLUSTER_NAME" start

database_password=$(openssl rand -hex 32)
runuser -u postgres -- "/usr/lib/postgresql/$PG_MAJOR/bin/psql" \
    -h "$RUN_DIR" -p "$PG_PORT" -d postgres -v ON_ERROR_STOP=1 <<EOF
SELECT 'CREATE ROLE $APP_USER LOGIN'
WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '$APP_USER')
\gexec
ALTER ROLE $APP_USER PASSWORD '$database_password';
SELECT 'CREATE DATABASE $APP_DATABASE OWNER $APP_USER'
WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_database WHERE datname = '$APP_DATABASE')
\gexec
ALTER DATABASE $APP_DATABASE SET timezone TO 'UTC';
EOF

install -d -o fanghaizhou -g fanghaizhou -m 0700 "$ENV_DIR"
umask 077
cat >"$ENV_FILE" <<EOF
CRYPTO_CTA_DATABASE_URL=postgresql://$APP_USER:$database_password@$SERVER_IP:$PG_PORT/$APP_DATABASE
EOF
chown fanghaizhou:fanghaizhou "$ENV_FILE"
chmod 0600 "$ENV_FILE"

runuser -u fanghaizhou -- env PGPASSWORD="$database_password" \
    "/usr/lib/postgresql/$PG_MAJOR/bin/psql" \
    -h 127.0.0.1 -p "$PG_PORT" -U "$APP_USER" -d "$APP_DATABASE" \
    -v ON_ERROR_STOP=1 -Atqc \
    "SELECT current_user, current_database();"

runuser -u postgres -- "/usr/lib/postgresql/$PG_MAJOR/bin/psql" \
    -h "$RUN_DIR" -p "$PG_PORT" -d postgres \
    -v ON_ERROR_STOP=1 -Atqc \
    "SHOW data_directory; SHOW unix_socket_directories;"

if command -v ufw >/dev/null &&
    ufw status 2>/dev/null | grep -q '^Status: active'; then
    ufw allow from "$CLIENT_IP" to "$SERVER_IP" port "$PG_PORT" proto tcp
fi

echo "PostgreSQL $PG_MAJOR cluster $CLUSTER_NAME is ready."
echo "Endpoint: $SERVER_IP:$PG_PORT"
echo "Credentials: $ENV_FILE (mode 0600)"
