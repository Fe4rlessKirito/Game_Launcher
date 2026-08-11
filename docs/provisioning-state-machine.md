# Provisioning state machine

The durable states are:

```text
CREATED -> STARTING -> REGISTRATION_STARTED -> WAITING_FOR_EMAIL
                                      |              |
                                      |              v
                                      +------> EMAIL_RECEIVED -> WAITING_FOR_PROVIDER
                                                               |
                                                               v
                                                        CANDIDATE_READY
                                                               |
                                                               v
                                                           VALIDATING
                                                               |
                                                               v
                                                           ENROLLING
                                                               |
                                                               v
                                                            ENROLLED
```

Any active state can move to `FAILED_RETRYABLE`, `FAILED_PERMANENT`, or
`NEEDS_OPERATOR` when the event is safe for that state. `FAILED_RETRYABLE`
returns through `RetryTimer` to `STARTING`; the provisioning worker performs
that wake-up after `retry_after`. Expired aliases/jobs become
`FAILED_PERMANENT` with a timeout code. `CANCELLED`, `FAILED_PERMANENT`, and
`ENROLLED` are terminal. Event idempotency is keyed by `(job_id,
idempotency_key)` and is checked inside the same transaction as the state
update.

The database also enforces one non-terminal job for a `(provider_type,
pool_id)` pair and a unique request idempotency key. PostgreSQL advisory locks
make the read/insert path deterministic under concurrent publishers; the
partial unique index is the final stampede guard.
