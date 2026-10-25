FROM rust:1.96-bookworm AS toolchain
RUN apt-get update \
    && apt-get install --yes --no-install-recommends dbus-daemon dbus-bin protobuf-compiler pkg-config libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev \
        libxcb-shape0-dev libxcb-xfixes0-dev libxcb-randr0-dev libxcb-xinput-dev \
        libegl1-mesa-dev libgles2-mesa-dev libglib2.0-dev libfontconfig-dev \
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
    && dbus-run-session -- cargo test --workspace --all-targets --locked \
    && cargo build --workspace --locked \
    && python3 conformance/adapter/test_contract.py \
    && python3 conformance/assessment/test_export.py

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

# Explicit provider evaluations have no connection to approval or firewall services.
FROM source AS assessment-eval
RUN --mount=type=cache,id=tachikoma-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=tachikoma-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    cargo build --locked -p tachikoma-assessment-worker \
    && install -Dm755 target/debug/tachikoma-assessment-worker /out/tachikoma-assessment-worker
ENTRYPOINT ["/out/tachikoma-assessment-worker"]

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

# Native window smoke test is isolated from live services and sockets.
FROM test AS desktop-smoke
RUN apt-get update && apt-get install --yes --no-install-recommends python3-dbus python3-gi xvfb xauth xdotool mesa-vulkan-drivers libvulkan1 fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    mkdir -p /tmp/desktop-runtime && chmod 700 /tmp/desktop-runtime \
    && XDG_RUNTIME_DIR=/tmp/desktop-runtime WGPU_BACKEND=vulkan xvfb-run -a ./conformance/desktop/smoke.sh light \
    && XDG_RUNTIME_DIR=/tmp/desktop-runtime WGPU_BACKEND=vulkan xvfb-run -a ./conformance/desktop/smoke.sh dark

FROM desktop-smoke AS notification-tools
RUN --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    install -Dm755 target/debug/tachikoma-gui /out/tachikoma-gui
ENV TACHIKOMA_TEST_GUI=/out/tachikoma-gui TACHIKOMA_TEST_DAEMON=/out/tachikomad
FROM notification-tools AS desktop-test
RUN --mount=type=cache,id=tachikoma-target,target=/app/target,sharing=locked \
    XDG_RUNTIME_DIR=/tmp/desktop-runtime WGPU_BACKEND=vulkan dbus-run-session -- xvfb-run -e /dev/stderr -a python3 -u conformance/desktop/notifications.py
