#!/usr/bin/env bash
# Local end-to-end test of the packaging, using Docker. Builds two versions of the packages
# (with stand-in binaries unless real ones are given), two signed repositories, and runs
# install-test.sh through install -> auto-upgrade -> remove in each target distro.
#
# Usage (from the repository root):
#   packaging/test/run-local.sh [work-dir] [distro-image ...]
#
# The repo's mirror list puts a dead mirror first, to check that clients fail over.
# Images whose name contains "systemd" or "init" are run with systemd as PID 1 (privileged),
# which additionally checks enable/start on install, restart on upgrade, and stop on removal.
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/../.." && pwd)"
work_dir="${1:-$(mktemp -d)}"
shift || true
images=("$@")
if [ "${#images[@]}" -eq 0 ]; then
    images=(debian:12 ubuntu:22.04 rockylinux:9 fedora:latest jrei/systemd-debian:12)
fi

version_1="0.0.1"
version_2="0.0.2"
network="hashiverse-packaging-test"
repo_container="hashiverse-packaging-test-repo"

rm -rf "$work_dir"
mkdir -p "$work_dir"
work_dir="$(cd "$work_dir" && pwd)"

docker_path() {
    # Docker Desktop on Windows wants a Windows-style path for bind mounts.
    if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else echo "$1"; fi
}

echo "=== stand-in binaries"
for version in "$version_1" "$version_2"; do
    mkdir -p "$work_dir/bin-$version"
    for binary in hashiverse-server hashiverse-cli; do
        cat > "$work_dir/bin-$version/$binary" <<EOF
#!/bin/sh
case "\${1:-}" in --help|--version) echo "stand-in $binary $version"; exit 0 ;; esac
exec sleep infinity
EOF
    done
done

echo "=== nfpm packages"
for version in "$version_1" "$version_2"; do
    mkdir -p "$work_dir/packages-$version"
    for package in hashiverse-server hashiverse-cli; do
        for packager in deb rpm; do
            MSYS_NO_PATHCONV=1 docker run --rm \
                -v "$(docker_path "$repository_root"):/src" -v "$(docker_path "$work_dir"):/work" -w /src \
                -e HASHIVERSE_PACKAGE_VERSION="$version" -e HASHIVERSE_PACKAGE_ARCH=amd64 -e HASHIVERSE_BINARY_DIR="/work/bin-$version" \
                goreleaser/nfpm:v2.43.0 package --config "packaging/nfpm/$package.yaml" --packager "$packager" --target "/work/packages-$version/"
        done
    done
done

echo "=== signed repositories"
MSYS_NO_PATHCONV=1 docker run --rm \
    -v "$(docker_path "$repository_root"):/src:ro" -v "$(docker_path "$work_dir"):/work" \
    -e HASHIVERSE_PACKAGE_MIRRORS="http://127.0.0.1:9/packages http://$repo_container:8000/current" \
    ubuntu:24.04 bash -euc "
        apt-get update -qq && apt-get install -y -qq gnupg apt-utils rpm createrepo-c >/dev/null
        gpg --batch --quick-gen-key --passphrase '' 'Hashiverse Packages Test' rsa4096 sign never
        for version in $version_1 $version_2; do
            bash /src/packaging/build-repo.sh /work/packages-\$version /work/repo-\$version 'Hashiverse Packages Test'
        done
        mkdir -p /work/serve && cp -r /work/repo-$version_1 /work/serve/current
    "

echo "=== repository server"
docker network create "$network" >/dev/null 2>&1 || true
docker rm -f "$repo_container" >/dev/null 2>&1 || true
MSYS_NO_PATHCONV=1 docker run -d --name "$repo_container" --network "$network" \
    -v "$(docker_path "$work_dir"):/work" -w /work/serve python:3-alpine python -m http.server 8000 >/dev/null

serve_version() {
    MSYS_NO_PATHCONV=1 docker exec "$repo_container" sh -c "rm -rf /work/serve/current && cp -r /work/repo-$1 /work/serve/current"
}

results=()
for image in "${images[@]}"; do
    echo "=== $image"
    serve_version "$version_1"
    container="hashiverse-packaging-test-target"
    docker rm -f "$container" >/dev/null 2>&1 || true

    run_args=(--name "$container" --network "$network" -v "$(docker_path "$repository_root/packaging/test"):/test:ro")
    if [[ "$image" == *systemd* || "$image" == *init* ]]; then
        MSYS_NO_PATHCONV=1 docker run -d --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw "${run_args[@]}" "$image" >/dev/null
        for _ in $(seq 1 30); do
            if docker exec "$container" test -d /run/systemd/system 2>/dev/null; then break; fi
            sleep 1
        done
    else
        MSYS_NO_PATHCONV=1 docker run -d "${run_args[@]}" "$image" sleep infinity >/dev/null
    fi

    base_url="http://$repo_container:8000/current"
    status="PASS"
    if ! MSYS_NO_PATHCONV=1 docker exec "$container" sh /test/install-test.sh install "$base_url" "$version_1" "$version_1"; then
        status="FAIL (install)"
    else
        serve_version "$version_2"
        if ! MSYS_NO_PATHCONV=1 docker exec "$container" sh /test/install-test.sh upgrade "$base_url" "$version_2" "$version_1"; then
            status="FAIL (upgrade)"
        elif ! MSYS_NO_PATHCONV=1 docker exec "$container" sh /test/install-test.sh remove "$base_url" "" "$version_1"; then
            status="FAIL (remove)"
        fi
    fi
    docker rm -f "$container" >/dev/null
    results+=("$status  $image")
done

docker rm -f "$repo_container" >/dev/null
docker network rm "$network" >/dev/null

echo
echo "=== results"
printf "%s\n" "${results[@]}"
if printf "%s\n" "${results[@]}" | grep -q "^FAIL"; then
    exit 1
fi
