#!/bin/sh
# Builds the interop images ferossh-interop:<ubuntu release>.
# Behind an HTTPS-only proxy set HTTPS_PROXY and INTEROP_CA (its CA bundle).
set -e
cd "$(dirname "$0")"

# Pulls ubuntu:$1, retrying, and from Google's Docker Hub mirror when Docker
# Hub keeps refusing (it rate-limits anonymous pulls).
pull() {
    for wait in 2 4 8 16; do
        docker pull -q "ubuntu:$1" >/dev/null && return
        echo "pulling ubuntu:$1 failed; again in ${wait}s" >&2
        sleep "$wait"
    done
    docker pull -q "mirror.gcr.io/library/ubuntu:$1" >/dev/null
    docker tag "mirror.gcr.io/library/ubuntu:$1" "ubuntu:$1"
}

for r in ${INTEROP_RELEASES:-18.04 20.04 22.04 24.04 26.04}; do
    pull "$r"
    if [ -n "$INTEROP_CA" ]; then
        set -- --network host --secret id=ca,src="$INTEROP_CA" \
            --build-arg https_proxy="$HTTPS_PROXY"
    else
        set --
    fi
    docker build -q "$@" --pull=false --build-arg BASE=ubuntu:$r -t ferossh-interop:$r . >/dev/null
    echo "ferossh-interop:$r"
done
