# Snitchgate source import

Imported without squashing history from noeljackson/snitchgate at
0f02a90b74da9312da3e0ad7090a426ca556893d. The vendored OpenSnitch protocol
and its license remain in proto/ unchanged.

The monorepo workspace owns Cargo.lock and release profiles. Snitchgate uses
the workspace-compatible rusqlite version with bundled SQLite; database integer
bindings use checked i64 conversion. The original installed workstation service
continues using its independently installed binary until an explicit deployment.

This subtree becomes the implementation source after the workspace migration
is reviewed. The original repository is preserved; no automatic two-way source
sync or archival is performed.
