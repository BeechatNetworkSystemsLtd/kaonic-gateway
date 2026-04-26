#!/bin/bash

# Usage: ./update_service.sh <service_name|binary_name> <ip1> [ip2] [ip3] ...

set -euo pipefail

if [ "$#" -lt 2 ]; then
    echo "Usage: $0 <service_name|binary_name> <ip1> [ip2] [ip3] ..."
    exit 1
fi

USER="root"
TARGET_TRIPLE="${TARGET_TRIPLE:-armv7-unknown-linux-gnueabihf}"
BUILD_PROFILE="${BUILD_PROFILE:-release}"

SERVICE_INPUT="$1"
SERVICE_BASENAME="${SERVICE_INPUT%.service}"
SERVICE_UNIT="${SERVICE_BASENAME}.service"
LOCAL_BIN="${PWD}/target/${TARGET_TRIPLE}/${BUILD_PROFILE}/${SERVICE_BASENAME}"
PLUGIN_ROOT="/etc/kaonic/plugins/${SERVICE_BASENAME}"
REMOTE_PLUGIN_BIN="${PLUGIN_ROOT}/current/${SERVICE_BASENAME}"
REMOTE_PLUGIN_SHA="${PLUGIN_ROOT}/${SERVICE_BASENAME}.sha256"
REMOTE_SYSTEM_BIN="/usr/bin/${SERVICE_BASENAME}"
REMOTE_SYSTEM_SHA="/etc/kaonic/${SERVICE_BASENAME}.sha256"

shift
IPS=("$@")

if [ ! -f "$LOCAL_BIN" ]; then
    echo "Binary '$LOCAL_BIN' not found."
    exit 1
fi

for IP in "${IPS[@]}"; do
    echo "=============================="
    echo "Host: $IP"
    echo "=============================="

    echo "Stopping $SERVICE_UNIT on $IP..."
    ssh -o ConnectTimeout=5 "$USER@$IP" "systemctl stop '$SERVICE_UNIT'" || {
        echo "Failed to stop $SERVICE_UNIT on $IP, continuing..."
    }

    echo "Preparing plugin directories on $IP..."
    ssh "$USER@$IP" "mkdir -p '$PLUGIN_ROOT/current' /etc/kaonic" || {
        echo "Failed to prepare plugin directories on $IP"
        continue
    }

    echo "Uploading $LOCAL_BIN -> $USER@$IP:$REMOTE_PLUGIN_BIN"
    scp "$LOCAL_BIN" "$USER@$IP:$REMOTE_PLUGIN_BIN" || {
        echo "Failed to copy plugin binary to $IP"
        continue
    }

    ssh "$USER@$IP" "\
        chmod 0755 '$REMOTE_PLUGIN_BIN' && \
        sha256sum '$REMOTE_PLUGIN_BIN' | awk '{print \$1}' > '$REMOTE_PLUGIN_SHA' \
    " || {
        echo "Failed to update plugin checksum on $IP"
        continue
    }

    if ssh "$USER@$IP" "[ -L '$REMOTE_SYSTEM_BIN' ] || [ -e '$REMOTE_SYSTEM_BIN' ]"; then
        echo "Refreshing built-in binary metadata on $IP..."
        ssh "$USER@$IP" "\
            if [ -L '$REMOTE_SYSTEM_BIN' ]; then \
                if [ \"\$(readlink '$REMOTE_SYSTEM_BIN')\" != '$REMOTE_PLUGIN_BIN' ]; then \
                    ln -sfn '$REMOTE_PLUGIN_BIN' '$REMOTE_SYSTEM_BIN'; \
                fi; \
            elif [ ! -e '$REMOTE_SYSTEM_BIN' ]; then \
                ln -s '$REMOTE_PLUGIN_BIN' '$REMOTE_SYSTEM_BIN'; \
            else \
                echo 'Warning: $REMOTE_SYSTEM_BIN exists and is not a symlink; leaving it unchanged' >&2; \
            fi && \
            sha256sum '$REMOTE_PLUGIN_BIN' | awk '{print \$1}' > '$REMOTE_SYSTEM_SHA' \
        " || {
            echo "Failed to refresh built-in binary metadata on $IP"
            continue
        }
    fi

    echo "Starting $SERVICE_UNIT on $IP..."
    ssh "$USER@$IP" "systemctl start '$SERVICE_UNIT'" || {
        echo "Failed to start $SERVICE_UNIT on $IP"
        continue
    }

    ssh "$USER@$IP" "systemctl --no-pager --full status '$SERVICE_UNIT' | head -n 10"

    echo "Done for $IP"
    echo
done
