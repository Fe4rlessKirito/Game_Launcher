#!/bin/sh
set -eu

mkdir -p /var/lib/launcher/storage /var/lib/launcher/megacmd /tmp/launcher-mega
chown -R launcher:launcher /var/lib/launcher/megacmd /tmp/launcher-mega

export HOME=/var/lib/launcher/megacmd
case ",${LAUNCHER_STORAGE_PROVIDERS:-}," in
    *,mega,*)
        if ! command -v mega-whoami >/dev/null 2>&1; then
            echo "diagnostic=MEGA_RUNTIME_MISSING official MEGAcmd is required when LAUNCHER_STORAGE_PROVIDERS includes mega" >&2
            exit 64
        fi
        ;;
esac
exec gosu launcher /usr/local/bin/launcher-admin "$@"
