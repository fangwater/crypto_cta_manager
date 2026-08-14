#!/usr/bin/env bash
set -Eeuo pipefail

readonly PG_VERSION="16.15"
readonly PG_MAJOR="16"
readonly SOURCE_NAME="postgresql-$PG_VERSION.tar.gz"
readonly SOURCE_URL="https://mirrors.cloud.tencent.com/postgresql/source/v$PG_VERSION/$SOURCE_NAME"
readonly SOURCE_SHA256="4f200ca23dfb120ff9838f13ce06014aad1d3c432d16ee9f93ab2000c0eeef7b"
readonly TARGET_PREFIX="/home/el01/.local/opt/postgresql/$PG_MAJOR"

output_path=${1:-"$PWD/postgresql-$PG_VERSION-linux-x86_64-user.tar.gz"}
output_path=$(realpath -m "$output_path")

for command_name in curl gcc gzip make nproc perl realpath sha256sum tar; do
    if ! command -v "$command_name" >/dev/null; then
        echo "Missing build command: $command_name" >&2
        exit 1
    fi
done

source /etc/os-release
if [[ $ID != "ubuntu" || $VERSION_ID != "24.04" || $(uname -m) != "x86_64" ]]; then
    echo "Build on Ubuntu 24.04 x86_64 for compatibility with cta_exec." >&2
    exit 1
fi

build_dir=$(mktemp -d)
cleanup() {
    rm -rf -- "$build_dir"
}
trap cleanup EXIT

curl --fail --silent --show-error --location --retry 3 \
    --connect-timeout 10 --max-time 300 \
    "$SOURCE_URL" -o "$build_dir/$SOURCE_NAME"
printf '%s  %s\n' "$SOURCE_SHA256" "$build_dir/$SOURCE_NAME" | \
    sha256sum --check --status

tar -xzf "$build_dir/$SOURCE_NAME" -C "$build_dir"
source_dir="$build_dir/postgresql-$PG_VERSION"
stage_dir="$build_dir/stage"

build_log="$build_dir/build.log"
if ! (
    cd "$source_dir"
    ./configure \
        --prefix="$TARGET_PREFIX" \
        --disable-nls \
        --without-icu \
        --without-readline \
        --without-zlib \
        CFLAGS="-O2 -pipe"
    make -j "${BUILD_JOBS:-$(nproc)}"
    make DESTDIR="$stage_dir" install

    for extension_name in amcheck btree_gist pageinspect pg_stat_statements pgstattuple; do
        make -C "contrib/$extension_name" -j "${BUILD_JOBS:-$(nproc)}"
        make -C "contrib/$extension_name" DESTDIR="$stage_dir" install
    done
) >"$build_log" 2>&1; then
    tail -n 120 "$build_log" >&2
    exit 1
fi

install_root="$stage_dir$TARGET_PREFIX"
for required_file in bin/initdb bin/postgres bin/psql share/postgresql.conf.sample; do
    if [[ ! -f $install_root/$required_file ]]; then
        echo "Build did not produce $required_file" >&2
        exit 1
    fi
done

output_dir=$(dirname "$output_path")
if [[ ! -d $output_dir ]]; then
    install -d -m 0755 "$output_dir"
fi
tar -C "$(dirname "$install_root")" -czf "$output_path" "$PG_MAJOR"
(
    cd "$output_dir"
    sha256sum "$(basename "$output_path")" >"$(basename "$output_path").sha256"
)

echo "Bundle: $output_path"
echo "Checksum: $output_path.sha256"
