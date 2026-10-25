# Interop test servers

Throwaway SSH servers of many ages for `crates/worker/tests/interop.rs` and
the tmux matrix in CI. One image per Ubuntu release:

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
runs through FeroSSH's terminal.

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
