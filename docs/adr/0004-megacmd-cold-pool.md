# ADR 0004: MEGAcmd-backed cold pool

## Status

Accepted

## Decision

Use the maintained official [MEGAcmd](https://github.com/meganz/MEGAcmd)
scriptable CLI through an isolated subprocess adapter. Each operator account
has a separate MEGAcmd home/session, deterministic object paths, bounded output,
timeouts, and typed authentication/unavailability errors.

## Consequences

The Rust server avoids a heavy native SDK dependency and does not automate
MEGA signup or password handling. Real account enrollment remains an operator
action using a pre-authenticated session and a credential reference. Fake
accounts cover pool behavior in ordinary CI; real smoke tests are opt-in.
