FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY server ./server
COPY migrations ./migrations
RUN cargo build --manifest-path server/Cargo.toml --release -p launcher-worker

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gosu \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/server/target/release/launcher-admin /usr/local/bin/launcher-admin
COPY deploy/worker-entrypoint.sh /usr/local/bin/worker-entrypoint
RUN useradd --system --create-home launcher \
    && mkdir -p /var/lib/launcher/storage /var/lib/launcher/megacmd /tmp/launcher-mega \
    && chown -R launcher:launcher /var/lib/launcher /tmp/launcher-mega \
    && chmod +x /usr/local/bin/worker-entrypoint
ENV TMPDIR=/tmp/launcher-mega
ENTRYPOINT ["/usr/local/bin/worker-entrypoint"]
