#!/bin/sh
set -eu

# A Render/Docker volume can be mounted as root even when the image declares a
# non-root app user. Fix the data directory, then run the service as `app`.
if [ "$(id -u)" = "0" ]; then
    mkdir -p /data
    chown -R app:app /data
    exec gosu app "$@"
fi

exec "$@"
