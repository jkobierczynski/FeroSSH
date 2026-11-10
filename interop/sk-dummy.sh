#!/bin/sh
# Builds OpenSSH's software security key, regress/misc/sk-dummy/sk-dummy.so,
# from the source of the OpenSSH installed here, so its middleware version
# matches the installed ssh-keygen and ssh-agent. With it they make and use
# sk-ssh-ed25519 and sk-ecdsa-sha2-nistp256 keys with no hardware: the
# "key handle" holds the private key. For tests only.
#
# Usage: interop/sk-dummy.sh OUT.so   (Debian/Ubuntu; needs deb-src,
# dpkg-dev, autoconf, libssl-dev and zlib1g-dev)
set -e
out=$(realpath "${1:?usage: sk-dummy.sh OUT.so}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
apt-get source -qq openssh >/dev/null 2>&1
cd openssh-*/
# Ubuntu patches configure.ac.
autoreconf -fi >/dev/null 2>&1
./configure -q >/dev/null
make -s openbsd-compat/libopenbsd-compat.a regress/misc/sk-dummy/sk-dummy.so >/dev/null 2>&1
cp regress/misc/sk-dummy/sk-dummy.so "$out"
echo "$out"
