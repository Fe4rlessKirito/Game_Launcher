-- Storage tiering, operator-managed cold accounts, capacity reservations, and restores.
-- This migration deliberately keeps provider credentials out of the database. The
-- credential_reference value points at an operator-managed secret/session location.

ALTER TABLE storage_locations
    ADD COLUMN IF NOT EXISTS tier TEXT NOT NULL DEFAULT 'HOT';

ALTER TABLE storage_objects
    ADD COLUMN IF NOT EXISTS tier TEXT NOT NULL DEFAULT 'HOT';

-- A chunk may have one verified object per provider. The old schema only allowed
-- one object for the entire chunk, which made replicas and hot/cold placement
-- impossible.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'storage_objects_pkey'
          AND contype = 'p'
          AND pg_get_constraintdef(oid) NOT ILIKE '%provider%'
    ) THEN
        ALTER TABLE storage_objects DROP CONSTRAINT storage_objects_pkey;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'storage_objects_pkey'
    ) THEN
        ALTER TABLE storage_objects
            ADD CONSTRAINT storage_objects_pkey PRIMARY KEY (encoded_hash, provider);
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS storage_locations_tier_idx
    ON storage_locations(encoded_hash, tier, priority, provider);
CREATE INDEX IF NOT EXISTS storage_objects_tier_idx
    ON storage_objects(encoded_hash, tier, provider)
    WHERE verified_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS storage_providers (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    tier TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    configuration_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS storage_accounts (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES storage_providers(id),
    credential_reference TEXT NOT NULL,
    tier TEXT NOT NULL DEFAULT 'COLD',
    status TEXT NOT NULL DEFAULT 'UNAVAILABLE',
    capacity_bytes BIGINT NOT NULL DEFAULT 0,
    used_bytes BIGINT NOT NULL DEFAULT 0,
    reserved_bytes BIGINT NOT NULL DEFAULT 0,
    safety_margin_bytes BIGINT NOT NULL DEFAULT 0,
    configuration_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    last_capacity_check TIMESTAMPTZ,
    last_health_check TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS storage_accounts_provider_status_idx
    ON storage_accounts(provider_id, status, id);

CREATE TABLE IF NOT EXISTS storage_reservations (
    id UUID PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES storage_accounts(id),
    encoded_hash TEXT NOT NULL REFERENCES chunks(encoded_hash),
    bytes BIGINT NOT NULL CHECK (bytes >= 0),
    state TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS storage_reservations_active_idx
    ON storage_reservations(account_id, state, expires_at);
CREATE UNIQUE INDEX IF NOT EXISTS storage_reservations_active_object_idx
    ON storage_reservations(account_id, encoded_hash)
    WHERE state IN ('HELD', 'COMMITTED');

CREATE TABLE IF NOT EXISTS storage_health_events (
    id BIGSERIAL PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES storage_providers(id),
    account_id TEXT REFERENCES storage_accounts(id),
    status TEXT NOT NULL,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS storage_health_events_recent_idx
    ON storage_health_events(provider_id, created_at DESC);

CREATE TABLE IF NOT EXISTS restore_jobs (
    id BIGSERIAL PRIMARY KEY,
    encoded_hash TEXT NOT NULL REFERENCES chunks(encoded_hash) ON DELETE CASCADE,
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

CREATE INDEX IF NOT EXISTS restore_jobs_claim_idx
    ON restore_jobs(state, next_attempt_at, lease_until, updated_at);
CREATE UNIQUE INDEX IF NOT EXISTS restore_jobs_active_idx
    ON restore_jobs(encoded_hash, target_provider)
    WHERE state IN ('QUEUED', 'RUNNING', 'RETRY');
