-- Generic capacity provisioning jobs. Provider credentials are never stored here;
-- credential_reference and inbound_email_token_hash are non-secret references/hashes.

CREATE TABLE IF NOT EXISTS provisioning_jobs (
    id UUID PRIMARY KEY,
    provider_type TEXT NOT NULL,
    pool_id TEXT NOT NULL REFERENCES storage_pools(id),
    requested_capacity_bytes BIGINT NOT NULL CHECK (requested_capacity_bytes >= 0),
    status TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_error_code TEXT,
    last_error_summary TEXT,
    inbound_email_token_hash TEXT,
    inbound_email_address TEXT,
    inbound_email_expires_at TIMESTAMPTZ,
    candidate_reference TEXT,
    credential_reference TEXT,
    operator_action TEXT,
    retry_after TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS provisioning_jobs_status_idx
    ON provisioning_jobs(status, retry_after, updated_at DESC);
CREATE INDEX IF NOT EXISTS provisioning_jobs_email_idx
    ON provisioning_jobs(lower(inbound_email_address), inbound_email_expires_at);
CREATE UNIQUE INDEX IF NOT EXISTS provisioning_jobs_active_pool_idx
    ON provisioning_jobs(provider_type, pool_id)
    WHERE status NOT IN ('ENROLLED', 'FAILED_PERMANENT', 'CANCELLED');

CREATE TABLE IF NOT EXISTS provisioning_job_events (
    id BIGSERIAL PRIMARY KEY,
    job_id UUID NOT NULL REFERENCES provisioning_jobs(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    event_type TEXT NOT NULL,
    from_status TEXT,
    to_status TEXT NOT NULL,
    safe_summary TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(job_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS provisioning_job_events_job_idx
    ON provisioning_job_events(job_id, created_at, id);

CREATE TABLE IF NOT EXISTS provisioning_mail_messages (
    message_id TEXT PRIMARY KEY,
    body_sha256 TEXT NOT NULL,
    envelope_from TEXT,
    envelope_to TEXT NOT NULL,
    parsed_from TEXT,
    subject TEXT,
    job_id UUID NOT NULL REFERENCES provisioning_jobs(id) ON DELETE CASCADE,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS provisioning_mail_messages_job_idx
    ON provisioning_mail_messages(job_id, received_at DESC);

CREATE TABLE IF NOT EXISTS provisioning_mail_nonces (
    nonce TEXT PRIMARY KEY,
    signed_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS provisioning_mail_nonces_expiry_idx
    ON provisioning_mail_nonces(expires_at);
