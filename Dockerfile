FROM rust:1.96-bookworm AS toolchain
RUN apt-get update \
    && apt-get install --yes --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
FROM toolchain AS source
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY bin ./bin
COPY examples ./examples
COPY conformance ./conformance
COPY contrib/adapters ./contrib/adapters
COPY crates ./crates
COPY apps ./apps
COPY workers ./workers
COPY src ./src
COPY services ./services
COPY templates ./templates
COPY contrib/tachikoma/config.example.json ./contrib/tachikoma/config.example.json
FROM source AS build
RUN --mount=type=cache,id=tachikoma-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=tachikoma-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    cargo build --release --locked -p tachikoma -p approval-daemon \
    && install -Dm755 target/release/tachikomad /out/tachikomad \
    && install -Dm755 target/release/tachikoma /out/tachikoma

FROM build AS test
RUN --mount=type=cache,id=tachikoma-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=tachikoma-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    python3 bin/check-boundaries.py \
    && python3 bin/test-boundaries.py \
    && cargo test --workspace --all-targets --locked \
    && cargo build --workspace --locked \
    && python3 conformance/adapter/test_contract.py

# Real browser forms exercise Origin/referrer policy, cookies and async rendering.
FROM toolchain AS browser-tools
RUN apt-get update && apt-get install --yes --no-install-recommends chromium chromium-driver \
    && rm -rf /var/lib/apt/lists/*
FROM browser-tools AS browser-test
COPY --from=test /out /out
COPY --from=source /app/conformance /app/conformance
COPY --from=source /app/examples /app/examples
RUN TACHIKOMA_TEST_DAEMON=/out/tachikomad python3 conformance/adapter/test_browser.py

# Package selection builds a service without compiling any sibling implementation.
FROM source AS service-build
ARG SERVICE=snitchgate
ARG BINARY=snitchgate
RUN --mount=type=cache,id=tachikoma-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=tachikoma-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    cargo build --release --locked -p "$SERVICE" --bin "$BINARY" \
    && install -Dm755 "target/release/$BINARY" /out/service

FROM debian:bookworm-slim AS service-runtime
RUN apt-get update && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system gate && useradd --system --gid gate --home-dir /nonexistent gate
COPY --from=service-build /out/service /usr/local/bin/service
USER gate
ENTRYPOINT ["/usr/local/bin/service"]

FROM debian:bookworm-slim AS runtime
RUN groupadd --system tachikoma \
    && useradd --system --gid tachikoma --home-dir /nonexistent --shell /usr/sbin/nologin tachikoma \
    && install -d --owner=tachikoma --group=tachikoma --mode=0700 /var/lib/tachikoma
COPY --from=build /out/tachikomad /usr/local/bin/tachikomad
COPY --from=build /out/tachikoma /usr/local/bin/tachikoma
USER tachikoma
EXPOSE 7447
ENTRYPOINT ["/usr/local/bin/tachikomad", "--database", "/var/lib/tachikoma/tachikoma.sqlite3", "--rpc-socket", "/tmp/tachikoma.sock", "--listen", "0.0.0.0:7447", "--allow-container-bind"]
