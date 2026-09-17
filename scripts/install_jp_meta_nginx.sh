#!/usr/bin/env bash
# Install or refresh the CTA manager routes on the jp-meta :4191 gateway.
#
# The 4191 site (sites-available/crypto_proxy_4191.conf) is regenerated whole
# from $HOME/nginx_locations.txt by setup_nginx_4191.sh. This script is
# idempotent and safe to run standalone or from deploy_host.sh:
#   1. install deploy/jp_meta/crypto-cta-nginx-snippet.conf as
#      /etc/nginx/snippets/crypto_cta_manager.conf (sudo),
#   2. upsert the "# BEGIN/END managed: crypto cta manager" block in
#      nginx_locations.txt with the single external: row from
#      deploy/jp_meta/nginx-locations.fragment.txt,
#   3. ensure the live site conf includes the snippet: drop the generated
#      plain-proxy duplicates of snippet-owned locations, insert the include,
#      then nginx -t && reload (restoring the backup on test failure).
#
# Usage: scripts/install_jp_meta_nginx.sh [--ssh jp-meta-elvpn] [--no-reload]
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SSH_HOST="${CTA_JP_META_SSH:-jp-meta-elvpn}"
DO_RELOAD=1
SNIPPET_LOCAL="$ROOT/deploy/jp_meta/crypto-cta-nginx-snippet.conf"
FRAGMENT_LOCAL="$ROOT/deploy/jp_meta/nginx-locations.fragment.txt"
SNIPPET_REMOTE="/etc/nginx/snippets/crypto_cta_manager.conf"
CONF_REMOTE="/etc/nginx/sites-available/crypto_proxy_4191.conf"

usage() {
    sed -n '2,15p' "${BASH_SOURCE[0]}" >&2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ssh)
            SSH_HOST="${2:?--ssh needs a host}"
            shift 2
            ;;
        --no-reload)
            DO_RELOAD=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

for f in "$SNIPPET_LOCAL" "$FRAGMENT_LOCAL"; do
    [[ -f "$f" ]] || { echo "[ERROR] missing $f" >&2; exit 1; }
done

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
TMP_SNIPPET="/tmp/crypto_cta_manager.snippet.${STAMP}"
TMP_FRAGMENT="/tmp/crypto_cta_manager.fragment.${STAMP}"

echo "[INFO] upload snippet + mapping fragment to ${SSH_HOST}"
scp -q "$SNIPPET_LOCAL" "${SSH_HOST}:${TMP_SNIPPET}"
scp -q "$FRAGMENT_LOCAL" "${SSH_HOST}:${TMP_FRAGMENT}"

echo "[INFO] install snippet, upsert mapping, patch site conf"
# shellcheck disable=SC2087
ssh "$SSH_HOST" \
    TMP_SNIPPET="$TMP_SNIPPET" TMP_FRAGMENT="$TMP_FRAGMENT" \
    SNIPPET_REMOTE="$SNIPPET_REMOTE" CONF_REMOTE="$CONF_REMOTE" \
    DO_RELOAD="$DO_RELOAD" \
    'bash -s' <<'REMOTE_EOF'
set -Eeuo pipefail

MAP="$HOME/nginx_locations.txt"
INCLUDE_LINE="    include ${SNIPPET_REMOTE};"

# 1) Install the snippet owned by this deployment.
sudo -n install -m 0644 "$TMP_SNIPPET" "$SNIPPET_REMOTE"
rm -f "$TMP_SNIPPET"
echo "[remote] installed $SNIPPET_REMOTE"

# 2) Upsert the managed block in nginx_locations.txt from the fragment file.
#    Marker lines in the fragment delimit the block; inner lines are copied
#    verbatim so comments documenting the contract survive regeneration.
tmp_map="$(mktemp)"
trap 'rm -f "$tmp_map"' EXIT
awk -v begin='# BEGIN managed: crypto cta manager' \
    -v end='# END managed: crypto cta manager' '
    NR == FNR {
        if ($0 != begin && $0 != end) frag = frag $0 "\n"
        next
    }
    $0 == begin { in_block = 1; replaced = 1; printf "%s\n%s", begin, frag; next }
    in_block && $0 == end { in_block = 0; print; next }
    in_block { next }
    { print }
    END {
        if (in_block) { print "[ERROR] unterminated cta manager block" > "/dev/stderr"; exit 42 }
        if (!replaced) { print ""; printf "%s\n%s%s\n", begin, frag, end }
    }
' "$TMP_FRAGMENT" "$MAP" > "$tmp_map"
cat "$tmp_map" > "$MAP"
rm -f "$tmp_map" "$TMP_FRAGMENT"
trap - EXIT
chmod 600 "$MAP"
echo "[remote] upserted cta manager block in $MAP"

# 3) Ensure the live site conf pulls the snippet in. Plain-proxy duplicates of
#    snippet-owned locations (left behind by older mapping rows) must be
#    dropped first or nginx -t fails on duplicate locations.
if sudo -n grep -Eq "include[[:space:]]+${SNIPPET_REMOTE}" "$CONF_REMOTE"; then
    echo "[remote] $CONF_REMOTE already includes the snippet"
else
    work="$(mktemp)"
    patched="$(mktemp)"
    sudo -n cat "$CONF_REMOTE" > "$work"
    awk -v inc="$INCLUDE_LINE" '
        /^    location = \/manager \{$/ { skip = 1; next }
        /^    location \/manager\/ \{$/ { skip = 1; next }
        /^    location \/manager\/api\/ \{$/ { skip = 1; next }
        /^    location = \/cta-api \{$/ { skip = 1; next }
        /^    location \/cta-api\/ \{$/ { skip = 1; next }
        /^    location = \/exec_trade0[0-9]+ \{$/ { skip = 1; next }
        /^    location \/exec_trade0[0-9]+\/ \{$/ { skip = 1; next }
        /^    location = \/exec_trade0[0-9]+\/config \{$/ { skip = 1; next }
        /^    location \/exec_trade0[0-9]+\/config\/ \{$/ { skip = 1; next }
        skip && /^    \}$/ { skip = 0; next }
        skip { next }
        { lines[NR] = $0 }
        END {
            last = 0
            for (i = NR; i >= 1; i--) if (lines[i] ~ /^\}/) { last = i; break }
            if (!last) { print "[ERROR] no server-closing brace found" > "/dev/stderr"; exit 42 }
            for (i = 1; i <= NR; i++) {
                if (i == last) print inc
                print lines[i]
            }
        }
    ' "$work" > "$patched"
    sudo -n install -m 0644 "$patched" "$CONF_REMOTE"
    if ! sudo -n nginx -t; then
        echo "[remote] nginx -t failed after patch; restoring previous conf" >&2
        sudo -n install -m 0644 "$work" "$CONF_REMOTE"
        rm -f "$work" "$patched"
        exit 1
    fi
    rm -f "$work" "$patched"
    echo "[remote] added include to $CONF_REMOTE (nginx -t ok)"
fi

if [[ "$DO_RELOAD" == "1" ]]; then
    sudo -n nginx -t
    sudo -n systemctl reload nginx
    echo "[remote] nginx reloaded"
fi
REMOTE_EOF

echo "[INFO] verify gateway routes"
code_manager="$(ssh "$SSH_HOST" 'curl -s -o /dev/null -w "%{http_code}" --max-time 8 http://127.0.0.1:4191/manager/')"
code_api="$(ssh "$SSH_HOST" 'curl -s -o /dev/null -w "%{http_code}" --max-time 8 http://127.0.0.1:4191/manager/api/health')"
echo "[INFO] /manager/ -> ${code_manager} (want 200), /manager/api/health -> ${code_api} (want 401)"
if [[ "$code_manager" != "200" || "$code_api" != "401" ]]; then
    echo "[ERROR] gateway verification failed" >&2
    exit 1
fi
echo "[INFO] done"
