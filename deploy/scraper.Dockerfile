FROM python:3.13-slim-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY scraper/pyproject.toml /opt/launcher-scraper/pyproject.toml
COPY scraper/src /opt/launcher-scraper/src
RUN python -m pip install --no-cache-dir --disable-pip-version-check /opt/launcher-scraper \
    && rm -rf /opt/launcher-scraper \
    && useradd --system --create-home launcher \
    && mkdir -p /var/lib/launcher/storage \
    && chown -R launcher:launcher /var/lib/launcher

USER launcher
ENV PYTHONUNBUFFERED=1
ENTRYPOINT ["launcher-scraper"]
