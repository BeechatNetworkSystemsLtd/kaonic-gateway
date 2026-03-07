
SDK_ASSET_NAME="stm32mp1-kaonic-proto-sdk-aarch64.sh"

echo "> Download SDK"
set -e && DOWNLOAD_URL=$(curl -s https://api.github.com/repos/BeechatNetworkSystemsLtd/kaonic-yocto/releases/latest \
    | grep "browser_download_url" \
    | grep "${SDK_ASSET_NAME}" \
    | cut -d '"' -f 4) \
    && wget "$DOWNLOAD_URL" -O /opt/yocto-sdk.sh

echo "> Install SDK"
chmod +x /opt/yocto-sdk.sh

/opt/yocto-sdk.sh -y -d /opt/yocto-sdk


