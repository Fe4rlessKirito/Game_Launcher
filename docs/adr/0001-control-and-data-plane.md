# ADR 0001: Separate control and data planes

Status: accepted

The API resolves metadata and provider locations, while the launcher obtains chunk bytes directly from storage. This keeps the VPS bandwidth requirement proportional to control traffic and allows provider/CDN failover without changing the client manifest model.
