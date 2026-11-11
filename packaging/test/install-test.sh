#!/bin/sh
# Installs hashiverse-server and hashiverse-cli from a hashiverse package repository, exactly as
# the README instructs, and checks the result. Runs inside a throwaway Debian/Ubuntu/RHEL/Fedora
# container (with or without systemd as PID 1).
#
# Usage: install-test.sh <phase> <packages-base-url> <expected-server-version> <expected-cli-version>
#
#   install  configure the repo from <packages-base-url> and install both packages
#   upgrade  run the auto-updater; expects ONLY hashiverse-server to reach the new version
#   remove   remove hashiverse-server; expects the node and updater stopped and disabled
#
# <packages-base-url> is the /packages directory (holding hashiverse-packages.asc, apt/, rpm/).
set -eu

phase="$1"
base_url="$2"
expected_server_version="$3"
expected_cli_version="$4"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

pass() {
    echo "ok: $*"
}

if command -v apt-get >/dev/null 2>&1; then
    family=apt
else
    family=dnf
fi

have_systemd=false
if [ -d /run/systemd/system ]; then
    have_systemd=true
fi

installed_version() {
    if [ "$family" = apt ]; then
        dpkg-query -W -f='${Version}' "$1" 2>/dev/null || true
    else
        rpm -q --queryformat '%{VERSION}' "$1" 2>/dev/null | grep -v "not installed" || true
    fi
}

expect_versions() {
    server_version="$(installed_version hashiverse-server)"
    cli_version="$(installed_version hashiverse-cli)"
    [ "$server_version" = "$expected_server_version" ] || fail "hashiverse-server version is '$server_version', expected '$expected_server_version'"
    [ "$cli_version" = "$expected_cli_version" ] || fail "hashiverse-cli version is '$cli_version', expected '$expected_cli_version'"
    pass "versions: server $server_version, cli $cli_version"
}

case "$phase" in
install)
    if [ "$family" = apt ]; then
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq curl ca-certificates >/dev/null
        curl -fsSL "$base_url/hashiverse-packages.asc" -o /usr/share/keyrings/hashiverse-packages.asc
        curl -fsSL "$base_url/apt/hashiverse.sources" -o /etc/apt/sources.list.d/hashiverse.sources
        curl -fsSL "$base_url/apt/hashiverse-mirrors.txt" -o /etc/apt/hashiverse-mirrors.txt
        apt-get update
        apt-get install -y hashiverse-server hashiverse-cli
    else
        curl -fsSL "$base_url/rpm/hashiverse.repo" -o /etc/yum.repos.d/hashiverse.repo
        dnf install -y hashiverse-server hashiverse-cli
    fi
    pass "installed with signature checks on"

    expect_versions
    hashiverse-cli --help >/dev/null || fail "hashiverse-cli --help failed"
    hashiverse-server --help >/dev/null || fail "hashiverse-server --help failed"
    pass "binaries run"

    for path in /usr/lib/systemd/system/hashiverse-server.service /usr/lib/systemd/system/hashiverse-server-updater.service \
        /usr/lib/systemd/system/hashiverse-server-updater.timer /usr/libexec/hashiverse-server/hashiverse-server-updater \
        /etc/default/hashiverse-server /var/lib/hashiverse-server; do
        [ -e "$path" ] || fail "missing $path"
    done
    pass "files in place"

    if [ "$have_systemd" = true ]; then
        systemd-analyze verify /usr/lib/systemd/system/hashiverse-server.service /usr/lib/systemd/system/hashiverse-server-updater.service \
            /usr/lib/systemd/system/hashiverse-server-updater.timer || fail "systemd-analyze verify"
        systemctl is-enabled --quiet hashiverse-server.service || fail "hashiverse-server not enabled after install"
        systemctl is-active --quiet hashiverse-server.service || fail "hashiverse-server not active after install"
        systemctl is-enabled --quiet hashiverse-server-updater.timer || fail "updater timer not enabled after install"
        systemctl is-active --quiet hashiverse-server-updater.timer || fail "updater timer not active after install"
        pass "node and updater timer enabled and running"
    fi
    ;;

upgrade)
    echo "# operator edit" >> /etc/default/hashiverse-server

    if [ "$have_systemd" = true ]; then
        started_before="$(systemctl show --property=ActiveEnterTimestampMonotonic --value hashiverse-server.service)"
        systemctl start hashiverse-server-updater || fail "updater service failed"
    else
        /usr/libexec/hashiverse-server/hashiverse-server-updater || fail "updater failed"
    fi

    expect_versions
    grep -q "# operator edit" /etc/default/hashiverse-server || fail "/etc/default/hashiverse-server edits were lost on upgrade"
    pass "settings kept across upgrade"

    if [ "$have_systemd" = true ]; then
        started_after="$(systemctl show --property=ActiveEnterTimestampMonotonic --value hashiverse-server.service)"
        [ "$started_after" != "$started_before" ] || fail "hashiverse-server was not restarted by the upgrade"
        systemctl is-active --quiet hashiverse-server.service || fail "hashiverse-server not active after upgrade"
        pass "node restarted onto the new version"

        systemctl disable --now hashiverse-server-updater
        if systemctl is-enabled --quiet hashiverse-server-updater.timer; then
            fail "'systemctl disable --now hashiverse-server-updater' left the timer enabled"
        fi
        if systemctl is-active --quiet hashiverse-server-updater.timer; then
            fail "'systemctl disable --now hashiverse-server-updater' left the timer running"
        fi
        pass "plain-name disable turns auto-update off"

        systemctl enable --now hashiverse-server-updater.timer
        systemctl is-active --quiet hashiverse-server-updater.timer || fail "could not re-enable the timer"
    fi
    ;;

remove)
    if [ "$family" = apt ]; then
        apt-get remove -y hashiverse-server
    else
        dnf remove -y hashiverse-server
    fi
    [ -z "$(installed_version hashiverse-server)" ] || fail "hashiverse-server still installed"
    [ -d /var/lib/hashiverse-server ] || fail "/var/lib/hashiverse-server (server identity) was deleted on removal"
    if [ "$have_systemd" = true ]; then
        if systemctl is-active --quiet hashiverse-server.service; then
            fail "hashiverse-server still running after removal"
        fi
        if systemctl is-active --quiet hashiverse-server-updater.timer; then
            fail "updater timer still running after removal"
        fi
    fi
    pass "removed; data dir kept"
    ;;

*)
    fail "unknown phase $phase"
    ;;
esac
