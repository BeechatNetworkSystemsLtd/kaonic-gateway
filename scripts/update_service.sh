#!/bin/bash

# Usage: ./update_service.sh <service_name> <ip1> [ip2] [ip3] ...

set -euo pipefail

# === Configuration ===
USER="root"                     # or another SSH user
REMOTE_PATH="/usr/bin"          # where to upload the binary
LOCAL_BIN="${PWD}/target/armv7-unknown-linux-gnueabihf/release/$1"           # binary name = first argument
SERVICE_NAME="$1"               # service name = same as binary name

# === Arguments check ===
if [ "$#" -lt 2 ]; then
    echo "Usage: $0 <service_name> <ip1> [ip2] [ip3] ..."
    exit 1
fi

shift  # remove service name from args list
IPS=("$@")

# === Check binary exists ===
if [ ! -f "$LOCAL_BIN" ]; then
    echo "Binary '$LOCAL_BIN' not found."
    exit 1
fi

# === Loop over all IPs ===
for IP in "${IPS[@]}"; do
    echo "=============================="
    echo "🔹 Host: $IP"
    echo "=============================="

    # Stop service
    echo "Stopping $SERVICE_NAME on $IP..."
    ssh -o ConnectTimeout=5 "$USER@$IP" "systemctl stop $SERVICE_NAME" || {
        echo "Failed to stop $SERVICE_NAME on $IP, continuing..."
    }

    # Copy file
    echo "Uploading $LOCAL_BIN →  $USER@$IP:$REMOTE_PATH/"
    scp "$LOCAL_BIN" "$USER@$IP:$REMOTE_PATH/" || {
        echo "Failed to copy file to $IP"
        continue
    }

    # Set permissions
    ssh "$USER@$IP" "chmod +x $REMOTE_PATH/$(basename "$LOCAL_BIN")"

    # Start service
    echo "Starting $SERVICE_NAME on $IP..."
    ssh "$USER@$IP" "systemctl start $SERVICE_NAME"

    # Check status
    ssh "$USER@$IP" "systemctl --no-pager --full status $SERVICE_NAME | head -n 10"

    echo "Done for $IP"
    echo
done


