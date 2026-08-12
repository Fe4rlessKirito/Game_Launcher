FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY server ./server
COPY migrations ./migrations
RUN cargo build --manifest-path server/Cargo.toml --release -p launcher-worker

FROM python:3.13-slim-bookworm
ARG MEGACMD_DEB_URL=https://mega.nz/linux/repo/Debian_12/amd64/megacmd-Debian_12_amd64.deb
ARG MEGACMD_DEB_SHA256=7CA78364DA0234B06A623DF19DE9A7DB3D6AB6F2A42924C1B99AF7B1170F4C06
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gosu \
    && curl --fail --location --retry 3 --output /tmp/megacmd.deb "$MEGACMD_DEB_URL" \
    && echo "$MEGACMD_DEB_SHA256  /tmp/megacmd.deb" | sha256sum --check --status \
    && apt-get install -y --no-install-recommends /tmp/megacmd.deb \
    && rm -f /tmp/megacmd.deb \
    && rm -rf /var/lib/apt/lists/*
COPY analyzer /opt/launcher-analyzer
RUN python -m pip install --no-cache-dir --disable-pip-version-check /opt/launcher-analyzer \
    && rm -rf /opt/launcher-analyzer
COPY --from=build /src/server/target/release/launcher-admin /usr/local/bin/launcher-admin
COPY deploy/worker-entrypoint.sh /usr/local/bin/worker-entrypoint
RUN useradd --system --create-home launcher \
    && mkdir -p /var/lib/launcher/storage /var/lib/launcher/megacmd /tmp/launcher-mega \
    && chown -R launcher:launcher /var/lib/launcher /tmp/launcher-mega \
    && chmod +x /usr/local/bin/worker-entrypoint
ENV TMPDIR=/tmp/launcher-mega
ENTRYPOINT ["/usr/local/bin/worker-entrypoint"]
