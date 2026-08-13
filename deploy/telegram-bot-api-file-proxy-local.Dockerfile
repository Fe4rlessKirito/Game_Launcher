FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*

COPY deploy/telegram-bot-api-file-proxy.py /usr/local/bin/telegram-bot-api-file-proxy.py
COPY deploy/telegram-bot-api-file-proxy-entrypoint.sh /usr/local/bin/telegram-bot-api-file-proxy-entrypoint
RUN chmod +x /usr/local/bin/telegram-bot-api-file-proxy.py /usr/local/bin/telegram-bot-api-file-proxy-entrypoint

USER 999:999
EXPOSE 8081
ENTRYPOINT ["/usr/local/bin/telegram-bot-api-file-proxy-entrypoint"]
