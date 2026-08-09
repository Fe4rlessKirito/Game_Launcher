CREATE TABLE IF NOT EXISTS games (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    hero_image_url TEXT,
    cover_image_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS builds (
    id TEXT PRIMARY KEY,
    game_id TEXT NOT NULL REFERENCES games(id),
    display_version TEXT NOT NULL,
    state TEXT NOT NULL,
    size_bytes BIGINT NOT NULL DEFAULT 0,
    manifest_json JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS builds_game_published_idx ON builds(game_id, published_at DESC) WHERE state = 'PUBLISHED';

CREATE TABLE IF NOT EXISTS storage_objects (
    encoded_hash TEXT PRIMARY KEY,
    encoded_size BIGINT NOT NULL,
    provider TEXT NOT NULL,
    object_key TEXT NOT NULL,
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS ingestion_jobs (
    id BIGSERIAL PRIMARY KEY,
    build_id TEXT REFERENCES builds(id),
    stage TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_until TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
