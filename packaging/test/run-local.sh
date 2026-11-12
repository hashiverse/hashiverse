#!/usr/bin/env bash
# Local end-to-end test of the packaging, using Docker. Builds packages (with stand-in binaries)
# for two stable versions and two release candidates, a signed repository for each, and runs
# install-test.sh in each target distro through two lanes:
#   stable  install 0.0.1 -> auto-upgrade to 0.0.2 -> remove
#   rc      install 0.0.2-rc1 from the rc repo -> auto-upgrade to rc2 -> switch the box to the
#           stable repo and auto-upgrade to 0.0.2 (an rc sorts before its final) -> remove
#
# Usage (from the repository root):
#   packaging/test/run-local.sh [work-dir] [distro-image ...]
#
# Each repo's mirror list puts a dead mirror first, to check that clients fail over.
# Images whose name contains "systemd" or "init" are run with systemd as PID 1 (privileged),
# which additionally checks enable/start on install, restart on upgrade, and stop on removal.
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/../.." && pwd)"
work_dir="${1:-$(mktemp -d)}"
shift || true
images=("$@")
if [ "${#images[@]}" -eq 0 ]; then
    images=(debian:12 ubuntu:22.04 rockylinux:9 fedora:latest jrei/systemd-debian:12 rockylinux/rockylinux:9-ubi-init)
fi

# Same algorithm as the production key (packaging/hashiverse-packages.asc); override to try others.
test_key_algorithm="${HASHIVERSE_TEST_KEY_ALGORITHM:-ed25519}"

version_1="0.0.1"
version_2="0.0.2"
rc_version_1="0.0.2-rc1"
rc_version_2="0.0.2-rc2"
all_versions="$version_1 $version_2 $rc_version_1 $rc_version_2"
network="hashiverse-packaging-test"
repo_container="hashiverse-packaging-test-repo"
container="hashiverse-packaging-test-target"

# The version dpkg and rpm report once installed: nfpm turns a semver prerelease "-rc1" into "~rc1".
installed_form() {
    echo "${1/-/"~"}"
}

rm -rf "$work_dir"
mkdir -p "$work_dir"
work_dir="$(cd "$work_dir" && pwd)"

docker_path() {
    # Docker Desktop on Windows wants a Windows-style path for bind mounts.
    if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else echo "$1"; fi
}

echo "=== stand-in binaries"
for version in $all_versions; do
    mkdir -p "$work_dir/bin-$version"
    cat > "$work_dir/bin-$version/hashiverse-cli" <<EOF
#!/bin/sh
case "\${1:-}" in --help|--version) echo "stand-in hashiverse-cli $version"; exit 0 ;; esac
exec sleep infinity
EOF
    # The server stand-in records what the systemd unit's sandbox lets it do, for install-test.sh to
    # check: what the real server needs (its data dir, ipset, iptables) must be allowed, the rest denied.
    cat > "$work_dir/bin-$version/hashiverse-server" <<EOF
#!/bin/sh
case "\${1:-}" in --help|--version) echo "stand-in hashiverse-server $version"; exit 0 ;; esac
probe_file=/var/lib/hashiverse-server/hardening-probe.txt
probe() {
    probe_name="\$1"
    shift
    if "\$@" >/dev/null 2>&1; then echo "\$probe_name=allowed"; else echo "\$probe_name=denied"; fi
}
{
    probe write_data_dir touch /var/lib/hashiverse-server/hardening-probe-write
    probe ipset ipset create hashiverse-hardening-probe hash:ip --exist
    probe iptables iptables -L INPUT -n
    probe write_usr touch /usr/bin/hashiverse-hardening-probe
    probe write_root_home touch /root/hashiverse-hardening-probe
    probe sys_admin unshare --mount true
} > "\$probe_file.tmp"
mv "\$probe_file.tmp" "\$probe_file"
exec sleep infinity
EOF
done

echo "=== nfpm packages"
for version in $all_versions; do
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
# The dead first mirror (port 9) checks failover. Each channel is served from its own path, as
# stable and rc are from their own hosts in production.
cat > "$work_dir/build-repos.sh" <<EOF
set -eu
apt-get update -qq && apt-get install -y -qq gnupg apt-utils rpm createrepo-c >/dev/null
gpg --batch --quick-gen-key --passphrase "" "Hashiverse Packages Test" $test_key_algorithm sign never
for version in $version_1 $version_2; do
    HASHIVERSE_PACKAGE_MIRRORS="http://127.0.0.1:9/packages http://$repo_container:8000/stable" \\
        bash /src/packaging/build-repo.sh "/work/packages-\$version" "/work/repo-\$version" "Hashiverse Packages Test"
done
for version in $rc_version_1 $rc_version_2; do
    HASHIVERSE_PACKAGE_CHANNEL=rc HASHIVERSE_PACKAGE_MIRRORS="http://127.0.0.1:9/packages http://$repo_container:8000/rc" \\
        bash /src/packaging/build-repo.sh "/work/packages-\$version" "/work/repo-\$version" "Hashiverse Packages Test"
done
mkdir -p /work/serve
EOF
MSYS_NO_PATHCONV=1 docker run --rm \
    -v "$(docker_path "$repository_root"):/src:ro" -v "$(docker_path "$work_dir"):/work" \
    ubuntu:24.04 bash /work/build-repos.sh

echo "=== repository server"
docker network create "$network" >/dev/null 2>&1 || true
docker rm -f "$repo_container" >/dev/null 2>&1 || true
MSYS_NO_PATHCONV=1 docker run -d --name "$repo_container" --network "$network" \
    -v "$(docker_path "$work_dir"):/work" -w /work/serve python:3-alpine python -m http.server 8000 >/dev/null

# serve_version <stable|rc> <version>: publish that version's repository as the channel's current one.
serve_version() {
    MSYS_NO_PATHCONV=1 docker exec "$repo_container" sh -c "rm -rf /work/serve/$1 && cp -r /work/repo-$2 /work/serve/$1"
}

start_target() {
    local image="$1"
    docker rm -f "$container" >/dev/null 2>&1 || true
    local run_args=(--name "$container" --network "$network" -v "$(docker_path "$repository_root/packaging/test"):/test:ro")
    if [[ "$image" == *systemd* || "$image" == *init* ]]; then
        MSYS_NO_PATHCONV=1 docker run -d --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw "${run_args[@]}" "$image" >/dev/null
        for _ in $(seq 1 30); do
            if docker exec "$container" test -d /run/systemd/system 2>/dev/null; then break; fi
            sleep 1
        done
    else
        MSYS_NO_PATHCONV=1 docker run -d "${run_args[@]}" "$image" sleep infinity >/dev/null
    fi
}

# install_test <phase> <stable|rc> <expected-server-version> <expected-cli-version>
install_test() {
    MSYS_NO_PATHCONV=1 docker exec "$container" sh /test/install-test.sh "$1" "http://$repo_container:8000/$2" "$3" "$4" >&2
}

# Each lane prints its result as its last line of stdout; everything else goes to stderr.
stable_lane() {
    serve_version stable "$version_1"
    install_test install stable "$version_1" "$version_1" || { echo "FAIL (install)"; return; }
    serve_version stable "$version_2"
    install_test upgrade stable "$version_2" "$version_1" || { echo "FAIL (upgrade)"; return; }
    install_test remove stable "" "$version_1" || { echo "FAIL (remove)"; return; }
    echo "PASS"
}

rc_lane() {
    local rc_1 rc_2
    rc_1="$(installed_form "$rc_version_1")"
    rc_2="$(installed_form "$rc_version_2")"
    serve_version rc "$rc_version_1"
    install_test install rc "$rc_1" "$rc_1" || { echo "FAIL (install)"; return; }
    serve_version rc "$rc_version_2"
    install_test upgrade rc "$rc_2" "$rc_1" || { echo "FAIL (upgrade rc)"; return; }
    serve_version stable "$version_2"
    install_test upgrade stable "$version_2" "$rc_1" || { echo "FAIL (rc -> stable)"; return; }
    install_test remove stable "" "$rc_1" || { echo "FAIL (remove)"; return; }
    echo "PASS"
}

results=()
for image in "${images[@]}"; do
    for lane in stable rc; do
        echo "=== $image ($lane)"
        start_target "$image"
        status="$("${lane}_lane" | tail -n 1)"
        docker rm -f "$container" >/dev/null
        results+=("$status  $image ($lane)")
    done
done

docker rm -f "$repo_container" >/dev/null
docker network rm "$network" >/dev/null

echo
echo "=== results"
printf "%s\n" "${results[@]}"
if printf "%s\n" "${results[@]}" | grep -q "^FAIL"; then
    exit 1
fi
