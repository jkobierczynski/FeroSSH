#!/bin/sh
# Builds the interop images ferossh-interop:<ubuntu release>.
# Behind an HTTPS-only proxy set HTTPS_PROXY and INTEROP_CA (its CA bundle).
set -e
cd "$(dirname "$0")"
for r in ${INTEROP_RELEASES:-18.04 20.04 22.04 24.04 26.04}; do
    if [ -n "$INTEROP_CA" ]; then
        set -- --network host --secret id=ca,src="$INTEROP_CA" \
            --build-arg https_proxy="$HTTPS_PROXY"
    else
        set --
    fi
    docker build -q "$@" --build-arg BASE=ubuntu:$r -t ferossh-interop:$r . >/dev/null
    echo "ferossh-interop:$r"
done
