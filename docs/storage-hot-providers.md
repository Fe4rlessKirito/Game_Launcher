# HOT provider contract

Every HOT provider advertises upload/delete, direct download, range support,
URL stability/expiry, refresh behavior, maximum object size, preferred pack
size, and recommended concurrency. A client-facing source must advertise
`direct_download=true`; a provider that requires private credentials is never
eligible for resolver output.

The built-in local provider exposes stable direct URLs and byte ranges. The S3
compatible provider exposes stable public URLs when configured, otherwise
short-lived presigned URLs with resolver refresh. Runtime provider URLs are
preferred over stored URLs so expired presigned locations are not reused.

FileMirage is intentionally not hard-coded until its documented API contract
and a controlled probe establish upload, delete, direct-link, range, and
expiry semantics. Buzzheavier and GoFile have documented HTTP APIs and may be
implemented as isolated adapters only after their configured plan and direct
link capability are probed. A provider without a proven direct-download
contract remains a server-side restore source, never a client URL.

Normal bytes are served by providers, not by the API's local proxy. The proxy
routes exist only for local development fixtures and explicitly bounded
fallbacks.
