#!/bin/sh
set -eu
package=$1
case "$package" in
    *.deb)
        apt-get update
        DEBIAN_FRONTEND=noninteractive apt-get install -y "$package" curl
        ;;
    *.rpm) dnf install -y "$package" curl ;;
    *.pkg.tar.zst)
        pacman -Syu --noconfirm
        pacman -U --noconfirm "$package"
        ;;
    *) exit 2 ;;
esac
getcap /usr/bin/vosu
# Rootless containers can set file capabilities outside their bounding set,
# then refuse execve. Record the install result before removing only this
# disposable container's capability so the unprivileged smoke can execute.
setcap -r /usr/bin/vosu || true
/usr/bin/vosu --help
for file in /usr/bin/vosu /usr/share/applications/vosu.desktop /usr/share/icons/hicolor/scalable/apps/vosu.svg /usr/share/licenses/vosu/LICENSE; do
    test -f "$file"
done
for size in 16 22 24 32 48 64 128 256 512; do
    test -f "/usr/share/icons/hicolor/${size}x${size}/apps/vosu.png"
done
# Do not start the daemon as container root. One Debian smoke exercises serving
# with disposable XDG directories and an unrelated listener, without a browser.
if [ "${VOSU_HTTP_SMOKE:-0}" = 1 ]; then
    useradd -m smoke
    mkdir -p /tmp/smoke/config/vosu /tmp/smoke/data /tmp/smoke/cache
    cat > /tmp/smoke/config/vosu/vosu.toml <<'CONFIG'
SERVER_PORT = 24109.0
DASHBOARD_PORT = 24110.0
OPEN_DASHBOARD_ON_STARTUP = false
ENABLE_AUTOUPDATE = false
CONFIG
    chown -R smoke:smoke /tmp/smoke
    su -s /bin/sh smoke -c 'XDG_CONFIG_HOME=/tmp/smoke/config XDG_DATA_HOME=/tmp/smoke/data XDG_CACHE_HOME=/tmp/smoke/cache BROWSER=/bin/true /usr/bin/vosu' &
    child=$!
    trap 'kill "$child" 2>/dev/null || true' EXIT
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        overlay_probe=$(curl --silent --output /dev/null --write-out '%{http_code} %{redirect_url}' http://127.0.0.1:24109/ || true)
        dashboard_code=$(curl --silent --output /tmp/dashboard --write-out '%{http_code}' http://127.0.0.1:24110/ || true)
        case "$overlay_probe" in
            '302 http://127.0.0.1:24110/'*)
                if [ "$dashboard_code" = 200 ] && [ -s /tmp/dashboard ] && grep -qi '<html' /tmp/dashboard; then
                    echo 'overlay HTTP 302 -> dashboard HTTP 200'
                    exit 0
                fi
                ;;
        esac
        sleep 1
    done
    echo "HTTP smoke failed (overlay='${overlay_probe:-}' dashboard='${dashboard_code:-}')" >&2
    exit 1
fi
