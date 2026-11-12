#!/bin/sh
# Pre-remove for the hashiverse-server package (deb and rpm).
# Stops and disables the node and its updater on removal, but not on upgrade.
# /var/lib/hashiverse-server (including the server identity) is deliberately left in place.
set -e

is_removal=false
case "$1" in
    # deb: "remove" on removal; "upgrade", "deconfigure", "failed-upgrade" otherwise
    remove) is_removal=true ;;
    # rpm: $1 is the number of package instances left after this transaction (0 = removal)
    0) is_removal=true ;;
esac

if [ "$is_removal" = true ] && [ -d /run/systemd/system ]; then
    systemctl disable --now hashiverse-server-updater.timer || true
    systemctl stop hashiverse-server-updater.service || true
    systemctl disable --now hashiverse-server.service || true
fi

exit 0
