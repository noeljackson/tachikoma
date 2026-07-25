FROM rust:1.96-bookworm AS build
RUN apt-get update \
    && apt-get install --yes --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY src ./src
COPY templates ./templates
RUN cargo build --release --locked

FROM build AS test
RUN cargo test --locked

FROM debian:bookworm-slim AS runtime
RUN groupadd --system tachikoma && useradd --system --gid tachikoma --home-dir /nonexistent --shell /usr/sbin/nologin tachikoma
COPY --from=build /app/target/release/tachikomad /usr/local/bin/tachikomad
USER tachikoma
EXPOSE 7447
ENTRYPOINT ["/usr/local/bin/tachikomad", "--database", "/tmp/tachikoma.sqlite3", "--rpc-socket", "/tmp/tachikoma.sock", "--listen", "0.0.0.0:7447"]
