# Interop test servers

Throwaway SSH servers of many ages for `crates/worker/tests/interop.rs` and
the tmux matrix in CI (tmux_e2e). One image per Ubuntu release:

| Image | OpenSSH | Dropbear | tmux |
|---|---|---|---|
| `ferossh-interop:18.04` | 7.6p1 | 2017.75 | 2.6 |
| `ferossh-interop:20.04` | 8.2p1 | 2019.78 | 3.0a |
| `ferossh-interop:22.04` | 8.9p1 | 2020.81 | 3.2a |
| `ferossh-interop:24.04` | 9.6p1 | 2022.83 | 3.4 |
| `ferossh-interop:26.04` | 10.2p1 | 2025.89 | 3.6 |

The container argument picks the server: `openssh`, `dropbear`, or `gear`
(OpenSSH limited to what switches and routers typically offer). The user is
`fero`; mount a directory holding `authorized_keys` at `/keys`. The images
also carry vttest, less and vim.tiny, which `real_programs_draw_correctly`
runs through FeroSSH's terminal, and lrzsz for `zmodem_on_servers_of_every_age`.

Besides logins and algorithms, the tests cover SFTP, jump hosts, remote
forwards (`-R`), ZMODEM through OpenSSH's and Dropbear's ptys, and that a
frozen or killed server is reported as lost (so the window reconnects) while
a shell that exits is not.

In CI this is a required check on `main`. The job caches the built images,
keyed by the hash of this folder's files, so a run pulls nothing from Docker
Hub unless they changed; `build.sh` retries pulls and falls back to Google's
Docker Hub mirror.

```sh
interop/build.sh
FEROSSH_REQUIRE_INTEROP=1 cargo test -p ferossh-worker --test interop -- --nocapture

# tmux control mode against another tmux
id=$(docker run -d --entrypoint sleep ferossh-interop:26.04 infinity)
FEROSSH_TMUX_CONTAINER=$id cargo test -p ferossh tmux_e2e
docker rm -f $id
```

Behind an HTTPS-only proxy, set `HTTPS_PROXY` and `INTEROP_CA` (the proxy's
CA bundle) before `build.sh`.

`security_keys_on_servers_of_every_age` logs in with FIDO2 security key
files (Ed25519 and ECDSA) signed by FeroSSH's software authenticator, on
every image whose server takes them: OpenSSH 8.2 and later, and Dropbear
2022.83 and later (Ubuntu builds 2020.81 without them).

`sk-dummy.sh` builds OpenSSH's own software security key from the source
of the OpenSSH installed on the host, so `ssh-keygen -w` and `ssh-agent`
can make and use security keys with no hardware. The `security_keys`
worker test uses it:

```sh
interop/sk-dummy.sh /tmp/sk-dummy.so
FEROSSH_SK_DUMMY=/tmp/sk-dummy.so cargo test -p ferossh-worker --test security_keys
```
