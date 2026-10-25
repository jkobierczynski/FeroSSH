# syntax=docker/dockerfile:1
# Throwaway SSH servers for the interop tests (crates/worker/tests/interop.rs).
# One image per Ubuntu release gives a spread of OpenSSH, Dropbear and tmux
# versions; entrypoint.sh picks which server to run.
ARG BASE=ubuntu:24.04
FROM ${BASE}
# Behind an HTTPS-only proxy, build.sh passes its CA as the secret "ca" and
# apt switches to https.
RUN --mount=type=secret,id=ca,required=false \
    if [ -f /run/secrets/ca ]; then \
      install -m 644 /run/secrets/ca /tmp/ca.crt; \
      echo 'Acquire::https::CAInfo "/tmp/ca.crt";' > /etc/apt/apt.conf.d/99ca; \
      find /etc/apt -name '*.list' -o -name '*.sources' | xargs sed -i 's#http://#https://#'; \
    fi \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      openssh-server dropbear-bin tmux \
 && rm -rf /var/lib/apt/lists/* /etc/apt/apt.conf.d/99ca /tmp/ca.crt \
 && mkdir -p /run/sshd /etc/dropbear \
 && useradd -m -s /bin/bash fero \
 && sed -i 's/^fero:[^:]*:/fero:*:/' /etc/shadow
COPY entrypoint.sh /entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
