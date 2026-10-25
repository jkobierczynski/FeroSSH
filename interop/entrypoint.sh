#!/bin/sh
# Usage: entrypoint.sh openssh|dropbear|gear|cbc
# The test mounts the user's public key at /keys/authorized_keys.
set -e
install -d -m 700 -o fero -g fero /home/fero/.ssh
install -m 600 -o fero -g fero /keys/authorized_keys /home/fero/.ssh/authorized_keys
case "$1" in
openssh)
    ssh-keygen -A >/dev/null
    exec /usr/sbin/sshd -D -e
    ;;
gear)
    # Algorithms as a typical switch or router offers them: no curve25519,
    # no AEAD ciphers. FeroSSH reaches these only with its legacy set.
    ssh-keygen -A >/dev/null
    exec /usr/sbin/sshd -D -e \
        -o KexAlgorithms=ecdh-sha2-nistp256,diffie-hellman-group14-sha256 \
        -o Ciphers=aes128-ctr,aes256-ctr \
        -o MACs=hmac-sha2-256-etm@openssh.com,hmac-sha2-256,hmac-sha2-512 \
        -o HostKeyAlgorithms=ecdsa-sha2-nistp256,rsa-sha2-256
    ;;
cbc)
    # Nothing FeroSSH allows, even with its legacy set.
    ssh-keygen -A >/dev/null
    exec /usr/sbin/sshd -D -e -o Ciphers=aes128-cbc,aes256-cbc
    ;;
dropbear)
    for t in ed25519 ecdsa rsa; do
        dropbearkey -t $t -f /etc/dropbear/dropbear_${t}_host_key >/dev/null 2>&1 || true
    done
    exec dropbear -F -E -p 22
    ;;
*)
    echo "unknown server $1" >&2
    exit 2
    ;;
esac
