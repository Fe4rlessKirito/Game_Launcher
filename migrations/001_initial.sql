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
    manifest_bytes BYTEA,
    signature_json JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS builds_game_published_idx ON builds(game_id, published_at DESC) WHERE state = 'PUBLISHED';

CREATE TABLE IF NOT EXISTS chunks (
    encoded_hash TEXT PRIMARY KEY,
    encoded_size BIGINT NOT NULL,
    encoding_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS build_chunks (
    build_id TEXT NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
    encoded_hash TEXT NOT NULL REFERENCES chunks(encoded_hash),
    raw_size BIGINT NOT NULL,
    raw_hash TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY(build_id, encoded_hash, ordinal)
);

CREATE TABLE IF NOT EXISTS storage_locations (
    encoded_hash TEXT NOT NULL REFERENCES chunks(encoded_hash) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    object_key TEXT NOT NULL,
    direct_url TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    verified_at TIMESTAMPTZ,
    PRIMARY KEY(encoded_hash, provider, direct_url)
);

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
    max_attempts INTEGER NOT NULL DEFAULT 5,
    worker_id TEXT,
    lease_until TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE builds ADD COLUMN IF NOT EXISTS signature_json JSONB;
ALTER TABLE builds ADD COLUMN IF NOT EXISTS manifest_bytes BYTEA;
ALTER TABLE ingestion_jobs ADD COLUMN IF NOT EXISTS max_attempts INTEGER NOT NULL DEFAULT 5;
ALTER TABLE ingestion_jobs ADD COLUMN IF NOT EXISTS worker_id TEXT;

CREATE INDEX IF NOT EXISTS ingestion_jobs_claim_idx ON ingestion_jobs(stage, lease_until, updated_at);
