FROM ghcr.io/fe4rlesskirito/game-launcher-telegram-bot-api@sha256:0691ebdaaccd79f0f8c8746f5c07c94442f0b83b3eba05f0e83de73fbefe8d6c

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends gosu python3 \
    && rm -rf /var/lib/apt/lists/*

COPY deploy/telegram-bot-api-file-proxy.py /usr/local/bin/telegram-bot-api-file-proxy.py
COPY deploy/telegram-bot-api-proxy-entrypoint.sh /usr/local/bin/telegram-bot-api-proxy-entrypoint
RUN chmod +x /usr/local/bin/telegram-bot-api-file-proxy.py /usr/local/bin/telegram-bot-api-proxy-entrypoint

USER telegram
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/telegram-bot-api-proxy-entrypoint"]
