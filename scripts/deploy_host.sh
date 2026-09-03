#!/usr/bin/env bash
# Build Manager locally, then deploy one independent host stack.
# Usage: scripts/deploy_host.sh --target el01|jp-meta [--skip-build]
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=""
SKIP_BUILD=0
NODE_BIN="${CTA_NODE_BIN:-/home/u171/fanghaizhou/preprocess/.tools/node/bin}"

usage() {
    cat <<'EOF'
Usage: scripts/deploy_host.sh --target el01|jp-meta [--skip-build]

Compile cta_web and the frontend on this machine, then upload to one
independent host. el01 and jp-meta do not share binaries, config,
PostgreSQL, Redis, Nginx, or Exec accounts.

  --target el01      SSH cta_exec, /home/el01/crypto_cta_manager
  --target jp-meta   SSH jp-meta-elvpn, /home/ubuntu/crypto_cta_manager
  --skip-build       Reuse the already-built local artifacts
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            TARGET="${2:-}"
            shift 2
            ;;
        --skip-build)
            SKIP_BUILD=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$TARGET" in
    el01)
        SSH_HOST="cta_exec"
        REMOTE_ROOT="/home/el01/crypto_cta_manager"
        DEPLOY_DIR="$ROOT/deploy/crypto_cta_manager"
        UNIT_NAME="crypto-cta-manager-web.service"
        EXPECTED_USER="el01"
        RESTART_NGINX=1
        ;;
    jp-meta)
        SSH_HOST="jp-meta-elvpn"
        REMOTE_ROOT="/home/ubuntu/crypto_cta_manager"
        DEPLOY_DIR="$ROOT/deploy/jp_meta"
        UNIT_NAME="crypto-cta-manager-web.service"
        EXPECTED_USER="ubuntu"
        RESTART_NGINX=0
        ;;
    *)
        echo "--target must be el01 or jp-meta" >&2
        usage >&2
        exit 2
        ;;
esac

remote() {
    ssh -o BatchMode=yes "$SSH_HOST" "$@"
}

require_local_file() {
    local path="$1"
    if [[ ! -f $path ]]; then
        echo "missing local file: $path" >&2
        exit 1
    fi
}

LOCAL_TARGET_DIR="$(
    cd "$ROOT"
    cargo metadata --no-deps --format-version 1 |
        python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])'
)"
LOCAL_RELEASE_DIR="$LOCAL_TARGET_DIR/release"

# A stale repository-local target symlink is easy to use accidentally during a
# manual recovery. Refuse to deploy while it disagrees with Cargo's authority.
if [[ -e "$ROOT/target" || -L "$ROOT/target" ]]; then
    ROOT_TARGET_DIR="$(readlink -f "$ROOT/target")"
    if [[ "$ROOT_TARGET_DIR" != "$LOCAL_TARGET_DIR" ]]; then
        echo "repository target mismatch: target=$ROOT_TARGET_DIR cargo=$LOCAL_TARGET_DIR" >&2
        exit 1
    fi
fi

if [[ $SKIP_BUILD -eq 0 ]]; then
    echo "rebuilding Manager binaries from a clean package cache"
    (
        cd "$ROOT"
        cargo clean -p crypto_cta_manager
        cargo build --locked --release --bin cta_web --bin nav_rebuild --bin nav_snapshot --bin nav_strategy_snapshot
    )
    echo "building frontend locally"
    (
        cd "$ROOT/frontend"
        if [[ -x $NODE_BIN/npm ]]; then
            PATH="$NODE_BIN:$PATH"
        fi
        npm run build
    )
fi

require_local_file "$LOCAL_RELEASE_DIR/cta_web"
require_local_file "$LOCAL_RELEASE_DIR/nav_rebuild"
require_local_file "$LOCAL_RELEASE_DIR/nav_snapshot"
require_local_file "$LOCAL_RELEASE_DIR/nav_strategy_snapshot"
require_local_file "$ROOT/frontend/dist/index.html"
require_local_file "$DEPLOY_DIR/cta-manager.toml"
require_local_file "$DEPLOY_DIR/crypto-cta-manager-web.service"
require_local_file "$ROOT/scripts/manager_publish_client.py"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RELEASE="web-releases/${STAMP}"
SOURCE_COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
STAGE_CHECKSUMS="$(mktemp)"
FINAL_CHECKSUMS="$(mktemp)"
RELEASE_MANIFEST="$(mktemp)"
cleanup_local() {
    rm -f "$STAGE_CHECKSUMS" "$FINAL_CHECKSUMS" "$RELEASE_MANIFEST"
}
trap cleanup_local EXIT

BINARIES=(cta_web nav_rebuild nav_snapshot nav_strategy_snapshot)
for name in "${BINARIES[@]}"; do
    read -r artifact_hash _ < <(sha256sum "$LOCAL_RELEASE_DIR/$name")
    printf '%s  bin/%s.next.%s\n' "$artifact_hash" "$name" "$STAMP" >>"$STAGE_CHECKSUMS"
    printf '%s  bin/%s\n' "$artifact_hash" "$name" >>"$FINAL_CHECKSUMS"
    printf '%s_sha256=%s\n' "$name" "$artifact_hash" >>"$RELEASE_MANIFEST"
done
read -r frontend_hash _ < <(sha256sum "$ROOT/frontend/dist/index.html")
printf '%s  %s/manager/index.html\n' "$frontend_hash" "$RELEASE" >>"$STAGE_CHECKSUMS"
printf '%s  %s/manager/index.html\n' "$frontend_hash" "$RELEASE" >>"$FINAL_CHECKSUMS"
{
    printf 'source_commit=%s\n' "$SOURCE_COMMIT"
    printf 'target=%s\n' "$TARGET"
    printf 'release=%s\n' "$RELEASE"
    printf 'frontend_index_sha256=%s\n' "$frontend_hash"
} >>"$RELEASE_MANIFEST"

echo "checking ${TARGET} as ${EXPECTED_USER}@${SSH_HOST}"
remote "test \"\$(id -un)\" = '${EXPECTED_USER}'"
remote "test -d '${REMOTE_ROOT}'"
remote "install -d -m 0755 '${REMOTE_ROOT}/bin' '${REMOTE_ROOT}/config' '${REMOTE_ROOT}/${RELEASE}/manager' '${REMOTE_ROOT}/web-releases'"

echo "uploading binaries and frontend to ${TARGET}"
# Upload beside the live binaries. Overwriting a running cta_web fails.
scp -q \
    "$LOCAL_RELEASE_DIR/cta_web" \
    "${SSH_HOST}:${REMOTE_ROOT}/bin/cta_web.next.${STAMP}"
scp -q \
    "$LOCAL_RELEASE_DIR/nav_rebuild" \
    "${SSH_HOST}:${REMOTE_ROOT}/bin/nav_rebuild.next.${STAMP}"
scp -q \
    "$LOCAL_RELEASE_DIR/nav_snapshot" \
    "${SSH_HOST}:${REMOTE_ROOT}/bin/nav_snapshot.next.${STAMP}"
scp -q \
    "$LOCAL_RELEASE_DIR/nav_strategy_snapshot" \
    "${SSH_HOST}:${REMOTE_ROOT}/bin/nav_strategy_snapshot.next.${STAMP}"
scp -q \
    "$DEPLOY_DIR/crypto-cta-manager-web.service" \
    "${SSH_HOST}:${REMOTE_ROOT}/"
if [[ $TARGET == el01 ]]; then
    scp -q "$DEPLOY_DIR/nginx.conf" "${SSH_HOST}:${REMOTE_ROOT}/nginx/nginx.conf.next"
    scp -q "$DEPLOY_DIR/crypto-cta-nginx.service" "${SSH_HOST}:${REMOTE_ROOT}/"
else
    scp -q "$DEPLOY_DIR/crypto-cta-nginx-snippet.conf" "${SSH_HOST}:${REMOTE_ROOT}/"
fi
# Do not overwrite the live host toml. New keys stay in the template.
scp -q "$DEPLOY_DIR/cta-manager.toml" "${SSH_HOST}:${REMOTE_ROOT}/config/cta-manager.toml.template"
rsync -a --delete \
    "$ROOT/frontend/dist/" \
    "${SSH_HOST}:${REMOTE_ROOT}/${RELEASE}/manager/"
scp -q "$STAGE_CHECKSUMS" "${SSH_HOST}:${REMOTE_ROOT}/.deploy-stage-${STAMP}.sha256"
scp -q "$FINAL_CHECKSUMS" "${SSH_HOST}:${REMOTE_ROOT}/.deploy-final-${STAMP}.sha256"
scp -q "$RELEASE_MANIFEST" "${SSH_HOST}:${REMOTE_ROOT}/${RELEASE}/RELEASE-MANIFEST.txt"

echo "verifying uploaded artifacts on ${TARGET}"
remote "cd '${REMOTE_ROOT}' && sha256sum -c '.deploy-stage-${STAMP}.sha256'"

echo "switching ${TARGET} to ${RELEASE}"
remote "bash -s" <<EOF
set -Eeuo pipefail
umask 0022
cd '${REMOTE_ROOT}'
chmod 0755 bin/cta_web.next.${STAMP} bin/nav_rebuild.next.${STAMP} bin/nav_snapshot.next.${STAMP} bin/nav_strategy_snapshot.next.${STAMP}
mv -f bin/cta_web.next.${STAMP} bin/cta_web
mv -f bin/nav_rebuild.next.${STAMP} bin/nav_rebuild
mv -f bin/nav_snapshot.next.${STAMP} bin/nav_snapshot
mv -f bin/nav_strategy_snapshot.next.${STAMP} bin/nav_strategy_snapshot
ln -sfn '${RELEASE}' webroot.next
mv -Tf webroot.next webroot
test -f webroot/manager/index.html
sha256sum -c '.deploy-final-${STAMP}.sha256'
rm -f '.deploy-stage-${STAMP}.sha256' '.deploy-final-${STAMP}.sha256'
install -d -m 0700 "\$HOME/.config/systemd/user"
install -m 0644 crypto-cta-manager-web.service "\$HOME/.config/systemd/user/${UNIT_NAME}"
systemctl --user daemon-reload
systemctl --user restart '${UNIT_NAME}'
systemctl --user --quiet is-active '${UNIT_NAME}'
EOF

if [[ $RESTART_NGINX -eq 1 ]]; then
    remote "bash -s" <<'EOF'
set -Eeuo pipefail
cd /home/el01/crypto_cta_manager
if [[ -f nginx/nginx.conf.next ]]; then
    mv -f nginx/nginx.conf.next nginx/nginx.conf
fi
if [[ -f crypto-cta-nginx.service ]]; then
    install -m 0644 crypto-cta-nginx.service "$HOME/.config/systemd/user/crypto-cta-nginx.service"
    systemctl --user daemon-reload
fi
if systemctl --user --quiet is-active crypto-cta-nginx.service; then
    systemctl --user reload crypto-cta-nginx.service || systemctl --user restart crypto-cta-nginx.service
fi
EOF
fi

echo "deployed Manager to ${TARGET} (${REMOTE_ROOT}, ${RELEASE})"
echo "host toml was not overwritten; compare config/cta-manager.toml.template if needed"
