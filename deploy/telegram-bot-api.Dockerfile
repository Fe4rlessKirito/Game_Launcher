FROM debian:bookworm AS build

ARG TELEGRAM_BOT_API_REF=adfd7f6a8e990272851777eeb3ae0def4216f161

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        cmake \
        g++ \
        git \
        gperf \
        libssl-dev \
        make \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
RUN git init telegram-bot-api \
    && git -C telegram-bot-api remote add origin https://github.com/tdlib/telegram-bot-api.git \
    && git -C telegram-bot-api fetch --depth 1 origin "$TELEGRAM_BOT_API_REF" \
    && git -C telegram-bot-api checkout --detach FETCH_HEAD \
    && git -C telegram-bot-api submodule update --init --recursive

WORKDIR /src/telegram-bot-api
RUN cmake -S . -B build \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX=/opt/telegram-bot-api \
    && cmake --build build --target install --parallel

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 zlib1g \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /var/lib/telegram telegram

COPY --from=build /opt/telegram-bot-api/bin/telegram-bot-api /usr/local/bin/telegram-bot-api
COPY deploy/telegram-bot-api-entrypoint.sh /usr/local/bin/telegram-bot-api-entrypoint

RUN mkdir -p /var/lib/telegram-bot-api /tmp/telegram-bot-api \
    && chown -R telegram:telegram /var/lib/telegram-bot-api /tmp/telegram-bot-api \
    && chmod +x /usr/local/bin/telegram-bot-api-entrypoint

USER telegram
ENV TELEGRAM_BOT_API_DIR=/var/lib/telegram-bot-api
ENV TELEGRAM_BOT_API_TEMP_DIR=/tmp/telegram-bot-api
EXPOSE 8081
ENTRYPOINT ["/usr/local/bin/telegram-bot-api-entrypoint"]
