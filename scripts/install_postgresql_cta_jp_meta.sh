#!/usr/bin/env bash
set -Eeuo pipefail

readonly EXPECTED_HOST="ip-172-31-35-228.ap-northeast-1.compute.internal"
readonly EXPECTED_USER="ubuntu"
readonly APP_USER="crypto_cta_manager"
readonly APP_DATABASE="crypto_cta_manager"
readonly ENV_DIR="/home/ubuntu/.config/crypto-cta-manager"
readonly ENV_FILE="$ENV_DIR/database.env"

if [[ $(id -un) != "$EXPECTED_USER" ]]; then
    echo "Run this installer as $EXPECTED_USER." >&2
    exit 1
fi
if [[ $(hostname -f) != "$EXPECTED_HOST" ]]; then
    echo "Refusing host $(hostname -f); expected $EXPECTED_HOST." >&2
    exit 1
fi
if [[ -e $ENV_FILE ]]; then
    echo "Refusing to overwrite existing $ENV_FILE." >&2
    exit 1
fi

password=$(openssl rand -base64 48 | tr -d '\n+/=' | head -c 48)
sudo -n -u postgres psql -v ON_ERROR_STOP=1 <<SQL
DO \$\$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '$APP_USER') THEN
        CREATE ROLE $APP_USER LOGIN PASSWORD '$password';
    END IF;
END
\$\$;
SELECT 'CREATE DATABASE $APP_DATABASE OWNER $APP_USER'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = '$APP_DATABASE')\gexec
GRANT ALL PRIVILEGES ON DATABASE $APP_DATABASE TO $APP_USER;
SQL

install -d -m 0700 "$ENV_DIR"
umask 0077
cat >"$ENV_FILE" <<EOF
CRYPTO_CTA_LOCAL_DATABASE_URL=postgresql://$APP_USER:$password@127.0.0.1:5432/$APP_DATABASE
EOF
chmod 0600 "$ENV_FILE"
echo "[OK] created $APP_DATABASE and wrote $ENV_FILE"
