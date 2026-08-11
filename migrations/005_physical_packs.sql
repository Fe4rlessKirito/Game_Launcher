-- Physical immutable packs are additive to the logical chunk model. Legacy
-- chunks, storage_objects, and storage_locations remain the compatibility
-- path until every client has a pack-capable release.

CREATE TABLE IF NOT EXISTS physical_packs (
    pack_hash TEXT PRIMARY KEY,
    format_version INTEGER NOT NULL,
    encoded_size BIGINT NOT NULL CHECK (encoded_size >= 0),
    chunk_count BIGINT NOT NULL CHECK (chunk_count >= 0),
    target_size BIGINT NOT NULL CHECK (target_size > 0),
    state TEXT NOT NULL DEFAULT 'VERIFIED',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    verified_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS pack_chunks (
    pack_hash TEXT NOT NULL REFERENCES physical_packs(pack_hash) ON DELETE CASCADE,
    encoded_hash TEXT NOT NULL REFERENCES chunks(encoded_hash),
    raw_hash TEXT NOT NULL,
    raw_size BIGINT NOT NULL CHECK (raw_size >= 0),
    encoded_offset BIGINT NOT NULL CHECK (encoded_offset >= 0),
    encoded_size BIGINT NOT NULL CHECK (encoded_size >= 0),
    compression_id TEXT NOT NULL,
    flags INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(pack_hash, encoded_hash)
);

CREATE INDEX IF NOT EXISTS pack_chunks_encoded_idx
    ON pack_chunks(encoded_hash, pack_hash);

CREATE TABLE IF NOT EXISTS pack_locations (
    pack_hash TEXT NOT NULL REFERENCES physical_packs(pack_hash) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    pool_id TEXT NOT NULL,
    failure_domain TEXT NOT NULL,
    storage_class TEXT NOT NULL,
    object_key TEXT NOT NULL,
    direct_url TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 100,
    verified_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    PRIMARY KEY(pack_hash, provider, direct_url)
);

CREATE INDEX IF NOT EXISTS pack_locations_hot_idx
    ON pack_locations(pack_hash, storage_class, priority, provider)
    WHERE verified_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS pack_restore_jobs (
    id BIGSERIAL PRIMARY KEY,
    pack_hash TEXT NOT NULL REFERENCES physical_packs(pack_hash) ON DELETE CASCADE,
    target_provider TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'QUEUED',
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until TIMESTAMPTZ,
    worker_id TEXT,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS pack_restore_jobs_active_idx
    ON pack_restore_jobs(pack_hash, target_provider)
    WHERE state IN ('QUEUED', 'RUNNING', 'RETRY');

CREATE INDEX IF NOT EXISTS pack_restore_jobs_claim_idx
    ON pack_restore_jobs(state, next_attempt_at, lease_until, updated_at);

CREATE TABLE IF NOT EXISTS pack_leases (
    pack_hash TEXT NOT NULL REFERENCES physical_packs(pack_hash) ON DELETE CASCADE,
    lease_id UUID PRIMARY KEY,
    owner TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS pack_leases_pack_expiry_idx
    ON pack_leases(pack_hash, expires_at);
