#!/usr/bin/env bash
set -euo pipefail

readonly NGINX_VERSION="1.24.0-2ubuntu7.15"
readonly NGINX_DEB="nginx_${NGINX_VERSION}_amd64.deb"
readonly COMMON_DEB="nginx-common_${NGINX_VERSION}_all.deb"
readonly NGINX_SHA256="3004458b1e9804ebe5e9c6a4c4fddcc80af012dbbe1a9f0669f275ee0aedc118"
readonly COMMON_SHA256="ce7211d826cb36f9454a5bae6270bdbc4da2dfd5d1137820914ba4555c25480d"
readonly DEFAULT_MIRROR="https://mirrors.cloud.tencent.com/ubuntu/pool/main/n/nginx"
readonly INSTALL_ROOT="${NGINX_USER_ROOT:-${HOME}/.local/opt/nginx}"
readonly VERSION_ROOT="${INSTALL_ROOT}/${NGINX_VERSION}"
readonly CURRENT_LINK="${INSTALL_ROOT}/current"

if [[ "$(dpkg --print-architecture)" != "amd64" ]]; then
  echo "[ERROR] the pinned user Nginx package requires amd64" >&2
  exit 1
fi
if [[ -e "${CURRENT_LINK}" && ! -L "${CURRENT_LINK}" ]]; then
  echo "[ERROR] refusing to replace non-symlink path: ${CURRENT_LINK}" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf -- "${work_dir}"' EXIT
mirror="${NGINX_DEB_MIRROR:-${DEFAULT_MIRROR}}"

curl --fail --location --retry 3 --output "${work_dir}/${NGINX_DEB}" \
  "${mirror}/${NGINX_DEB}"
curl --fail --location --retry 3 --output "${work_dir}/${COMMON_DEB}" \
  "${mirror}/${COMMON_DEB}"

printf '%s  %s\n' "${NGINX_SHA256}" "${work_dir}/${NGINX_DEB}" | sha256sum --check
printf '%s  %s\n' "${COMMON_SHA256}" "${work_dir}/${COMMON_DEB}" | sha256sum --check

mkdir -p "${work_dir}/nginx" "${work_dir}/common"
dpkg-deb --extract "${work_dir}/${NGINX_DEB}" "${work_dir}/nginx"
dpkg-deb --extract "${work_dir}/${COMMON_DEB}" "${work_dir}/common"

install -d "${VERSION_ROOT}/sbin" "${VERSION_ROOT}/conf"
install -m 0755 "${work_dir}/nginx/usr/sbin/nginx" "${VERSION_ROOT}/sbin/nginx"
install -m 0644 "${work_dir}/common/etc/nginx/mime.types" "${VERSION_ROOT}/conf/mime.types"
ln -sfn "${NGINX_VERSION}" "${CURRENT_LINK}"

"${CURRENT_LINK}/sbin/nginx" -v
echo "[OK] user Nginx installed at ${CURRENT_LINK}"
