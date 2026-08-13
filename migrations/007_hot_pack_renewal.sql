-- HOT mirrors with inactivity-based retention need a provider upload clock.
-- Existing rows receive the migration time because an original upload start
-- timestamp cannot be reconstructed safely.
ALTER TABLE pack_locations
    ADD COLUMN IF NOT EXISTS last_uploaded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS renewal_attempt_after TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE INDEX IF NOT EXISTS pack_locations_renewal_idx
    ON pack_locations(storage_class, provider, last_uploaded_at, renewal_attempt_after)
    WHERE storage_class='HOT' AND verified_at IS NOT NULL;
