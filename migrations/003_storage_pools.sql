-- Storage-domain generalization: logical classes and provider pools are
-- independent. Existing provider/account/location rows remain valid and are
-- mapped deterministically to one pool per existing provider.

CREATE TABLE IF NOT EXISTS storage_pools (
    id TEXT PRIMARY KEY,
    storage_class TEXT NOT NULL,
    provider_type TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 100,
    failure_domain TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    status TEXT NOT NULL DEFAULT 'READY',
    provisioning_mode TEXT NOT NULL DEFAULT 'MANUAL',
    configuration_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE storage_providers
    ADD COLUMN IF NOT EXISTS pool_id TEXT REFERENCES storage_pools(id);

ALTER TABLE storage_accounts
    ADD COLUMN IF NOT EXISTS pool_id TEXT REFERENCES storage_pools(id),
    ADD COLUMN IF NOT EXISTS failure_domain TEXT;

ALTER TABLE storage_locations
    ADD COLUMN IF NOT EXISTS pool_id TEXT REFERENCES storage_pools(id),
    ADD COLUMN IF NOT EXISTS failure_domain TEXT;

ALTER TABLE storage_objects
    ADD COLUMN IF NOT EXISTS pool_id TEXT REFERENCES storage_pools(id),
    ADD COLUMN IF NOT EXISTS failure_domain TEXT;

-- Providers already known to the tiering migration become pools. MEGA
-- accounts intentionally share the provider failure domain, while distinct
-- provider IDs remain distinguishable by default.
INSERT INTO storage_pools(
    id, storage_class, provider_type, priority, failure_domain,
    enabled, status, provisioning_mode, configuration_json
)
SELECT id,
       tier,
       kind,
       100,
       CASE WHEN lower(kind) = 'mega' THEN 'mega' ELSE id END,
       enabled,
       CASE WHEN enabled THEN 'READY' ELSE 'DISABLED' END,
       CASE WHEN lower(kind) = 'mega' THEN 'MANUAL' ELSE 'DISABLED' END,
       configuration_json
FROM storage_providers
ON CONFLICT(id) DO NOTHING;

-- The initial schema allowed locations/objects before a provider ledger row
-- existed (for example the local development provider). Preserve those rows
-- by creating deterministic compatibility pools from their existing tier.
INSERT INTO storage_pools(
    id, storage_class, provider_type, priority, failure_domain,
    enabled, status, provisioning_mode
)
SELECT provider,
       tier,
       CASE WHEN lower(provider) LIKE '%mega%' THEN 'mega' ELSE provider END,
       100,
       CASE WHEN lower(provider) LIKE '%mega%' THEN 'mega' ELSE provider END,
       TRUE,
       'READY',
       CASE WHEN lower(provider) LIKE '%mega%' THEN 'MANUAL' ELSE 'DISABLED' END
FROM (
    SELECT provider, tier FROM storage_locations
    UNION
    SELECT provider, tier FROM storage_objects
) existing
ON CONFLICT(id) DO NOTHING;

UPDATE storage_providers
SET pool_id = id
WHERE pool_id IS NULL;

UPDATE storage_accounts
SET pool_id = provider_id
WHERE pool_id IS NULL;

UPDATE storage_accounts AS account
SET failure_domain = pool.failure_domain
FROM storage_pools AS pool
WHERE pool.id = account.pool_id
  AND account.failure_domain IS NULL;

UPDATE storage_locations AS location
SET pool_id = pool.id,
    failure_domain = pool.failure_domain
FROM storage_pools AS pool
WHERE pool.id = location.provider
  AND (location.pool_id IS NULL OR location.failure_domain IS NULL);

UPDATE storage_objects AS object
SET pool_id = pool.id,
    failure_domain = pool.failure_domain
FROM storage_pools AS pool
WHERE pool.id = object.provider
  AND (object.pool_id IS NULL OR object.failure_domain IS NULL);

CREATE INDEX IF NOT EXISTS storage_pools_class_priority_idx
    ON storage_pools(storage_class, enabled, priority, id);
CREATE INDEX IF NOT EXISTS storage_locations_pool_idx
    ON storage_locations(encoded_hash, pool_id, failure_domain, priority);
CREATE INDEX IF NOT EXISTS storage_objects_pool_idx
    ON storage_objects(encoded_hash, pool_id, failure_domain)
    WHERE verified_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS storage_accounts_pool_status_idx
    ON storage_accounts(pool_id, status, id);
