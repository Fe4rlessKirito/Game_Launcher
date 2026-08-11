# ADR 0007: generic capacity provisioning jobs

## Status

Accepted for staging implementation.

## Decision

Use a provider-neutral, PostgreSQL-backed provisioning job state machine with
capability-declared manual/automatic modes. Provider adapters return candidate
material; server-owned validation and enrollment decide whether capacity joins a
storage pool. Email verification is an authenticated raw-MIME ingress boundary,
not a provider signup API.

## Context

Storage pools may need more accounts without coupling placement, manifests, or
chunk transfer code to a provider's signup flow. MEGA must remain operator
managed, while fake automatic provisioning provides deterministic tests and a
future in-process/external hook has a durable contract.

## Consequences

The database gains jobs, safe event history, Message-ID deduplication, and HMAC
nonce ledgers. A small persistent volume is sufficient for MEGAcmd session
state; chunk data remains in HOT storage or bounded temporary space. The
operator must explicitly complete manual jobs, and a missing automatic
provisioner blocks capacity rather than silently changing storage policy.
