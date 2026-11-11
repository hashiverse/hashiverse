#!/bin/sh
# Post-install for the hashiverse-server package (deb and rpm).
#
# Fresh install: enable and start the node and its daily updater timer.
# Upgrade:       restart the node onto the new binary, but only if it is already running,
#                so a node an operator deliberately stopped stays stopped.
# No systemd (containers, chroots): do nothing beyond the message.
set -e

is_upgrade=false
case "$1" in
    # deb: "configure <previously-configured-version>" (empty on a fresh install)
    configure) [ -n "${2:-}" ] && is_upgrade=true ;;
    # deb error-unwind paths: nothing to do
    abort-upgrade | abort-remove | abort-deconfigure) exit 0 ;;
    # rpm: $1 is the number of package instances after this transaction (1 = install, 2 = upgrade)
    1) ;;
    2) is_upgrade=true ;;
esac

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    if [ "$is_upgrade" = true ]; then
        systemctl try-restart hashiverse-server.service || true
    else
        # Never fail the package install if the node can't start (e.g. port 443 already taken).
        systemctl enable --now hashiverse-server.service || true
        systemctl enable --now hashiverse-server-updater.timer || true
    fi
fi

if [ "$is_upgrade" = false ]; then
    cat >&2 <<'EOF'

hashiverse-server is installed and running as the systemd service "hashiverse-server".
  - The first start computes the server identity: about 5 hours of proof-of-work.
    Follow it with: journalctl -u hashiverse-server -f
  - Settings: /etc/default/hashiverse-server (then: systemctl restart hashiverse-server)
  - Data and identity: /var/lib/hashiverse-server
  - Auto-updates: daily, at a random time; turn off with
    systemctl disable --now hashiverse-server-updater

EOF
fi

exit 0
