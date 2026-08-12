-- Build history is retained, while HOT placement is allowed to follow only
-- the latest build for a game. Physical packs need an explicit build link so
-- an old build's HOT pack mirror can be retired without deleting a pack that
-- is still used by the current build.

CREATE TABLE IF NOT EXISTS build_packs (
    build_id TEXT NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
    pack_hash TEXT NOT NULL REFERENCES physical_packs(pack_hash) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(build_id, pack_hash)
);

CREATE INDEX IF NOT EXISTS build_packs_pack_idx
    ON build_packs(pack_hash, build_id);

-- Preserve associations for packs published before this migration existed.
-- A pack is associated with every published build whose manifest references one
-- of the pack's encoded chunks. The operation is idempotent and does not copy
-- any bytes.
INSERT INTO build_packs(build_id, pack_hash)
SELECT DISTINCT bc.build_id, pc.pack_hash
FROM pack_chunks pc
JOIN build_chunks bc ON bc.encoded_hash = pc.encoded_hash
ON CONFLICT(build_id, pack_hash) DO NOTHING;
