# Tachikoma

Tachikoma is a local-first control plane that turns security and operational
signals into typed, reviewable proposals. It starts with OpenSnitch and keeps
all policy promotion and execution explicit and auditable.

## Development

```sh
cargo check
make supplychain
make check
```

The local web UI is rendered by Rust with Askama templates. It has no React,
Node, or Bun toolchain; HTMX can be vendored later as a small progressive
enhancement when partial-page updates are needed.

The browser UI is loopback-only. Connect and gRPC are exposed separately on a
user-owned Unix socket (default: `$XDG_RUNTIME_DIR/tachikoma/tachikoma.sock`),
mode `0600`; OpenSnitch input is opt-in with `--opensnitch-history` and is
read-only. A denied connection can create a review proposal, never an automatic
allow rule.
