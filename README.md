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

The `tachikoma` terminal client uses that Unix-socket Connect API. For example,
`tachikoma status`, `tachikoma queue`, and `tachikoma approve <proposal-id>`
never write the SQLite database directly.
`tachikoma tui` presents that same queue in a native terminal UI; `q` or Escape
closes it.

`tachikoma suggest-kubectl --context development -- get pods` is the first
Kubernetes-ready adapter path. It creates a review-only proposal and does not
load kubeconfig, contact a cluster, or execute `kubectl`; a future `kube-rs`
executor must remain a separately scoped capability.

Automation policies are JSON-scoped durable intent, not generic shell or
network permission. They must name a non-empty scope; an `automatic` policy is
limited to low-risk proposals and still cannot execute anything until a future
adapter-specific executor is explicitly installed and enabled.

Use `tachikoma policy upsert ... --scope '{"context":"development"}'` to
configure them. The terminal client calls the policy Connect service; it does
not modify the local state database directly.
