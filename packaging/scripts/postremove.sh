#!/bin/sh
# Post-remove for the hashiverse-server package (deb and rpm): forget the removed unit files.
set -e

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi

exit 0
