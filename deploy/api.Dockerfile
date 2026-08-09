FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY server ./server
COPY migrations ./migrations
RUN cargo build --manifest-path server/Cargo.toml --release -p launcher-api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/server/target/release/launcher-api /usr/local/bin/launcher-api
RUN useradd --system --create-home launcher && mkdir -p /var/lib/launcher/storage && chown -R launcher:launcher /var/lib/launcher
USER launcher
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/launcher-api"]
