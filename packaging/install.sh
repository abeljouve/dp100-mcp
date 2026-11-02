#!/bin/sh
# Install dp100-mcp as a shared, sandboxed systemd service (Streamable HTTP on
# 127.0.0.1:8765) plus the dp100ctl CLI. Idempotent; run as root from the
# repository root after `cargo build --release`.
#
# The service is the single owner of the DP100 (exclusive flock on its hidraw
# node); agents connect over HTTP. An existing /etc/dp100-mcp/config.toml is
# never overwritten.
set -eu

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
[ -x target/release/dp100-mcp ] || { echo "build first: cargo build --release" >&2; exit 1; }

install -m 0755 target/release/dp100-mcp target/release/dp100ctl /usr/local/bin/

install -m 0644 packaging/99-dp100.rules /etc/udev/rules.d/99-dp100.rules
udevadm control --reload-rules
udevadm trigger --subsystem-match=hidraw --action=change
udevadm settle

getent group plugdev >/dev/null || groupadd --system plugdev
id dp100 >/dev/null 2>&1 || useradd --system --user-group --no-create-home --home-dir /nonexistent \
    --shell /usr/sbin/nologin --groups plugdev --comment "dp100-mcp service" dp100

install -d -m 0755 /etc/dp100-mcp
if [ ! -e /etc/dp100-mcp/config.toml ]; then
    install -m 0644 packaging/dp100-mcp.toml /etc/dp100-mcp/config.toml
    echo "installed /etc/dp100-mcp/config.toml: review the limits"
fi

install -d -m 0755 /usr/local/share/doc/dp100-mcp
install -m 0644 README.md docs/PROTOCOL.md /usr/local/share/doc/dp100-mcp/

install -m 0644 packaging/dp100-mcp.service /etc/systemd/system/dp100-mcp.service
systemctl daemon-reload
systemctl enable dp100-mcp
systemctl restart dp100-mcp
sleep 1
systemctl --no-pager --lines=5 status dp100-mcp
