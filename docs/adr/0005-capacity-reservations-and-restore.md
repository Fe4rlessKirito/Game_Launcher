# ADR 0005: Capacity reservations and server-side restore

## Status

Accepted

## Decision

PostgreSQL is the production ledger for account capacity, reservations,
provider health, and restore jobs. Account rows are locked during reservation;
holds are committed only after transfer verification and are recoverable after
expiry. Cold reads enqueue a leased restore job rather than exposing cold
credentials or URLs to a client.

## Consequences

Concurrent publishers cannot overcommit the same account. A full or unhealthy
pool blocks publication with an actionable status. Restore work is retryable,
observable, and independent of API request lifetimes.
