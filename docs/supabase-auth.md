# Supabase authentication

Supabase is used for user identity only. Mantle PostgreSQL remains the source
of truth for the launcher catalog, manifests, storage locations, and jobs.

## Configuration

Create a Supabase project and enable the email/password provider. Do not put a
service-role key in the website, launcher, Git, or chat.

Build the website with:

```text
PUBLIC_SUPABASE_URL=https://<project-ref>.supabase.co
PUBLIC_SUPABASE_ANON_KEY=<publishable-or-anon-key>
PUBLIC_LAUNCHER_API_URL=https://vaultnode.pp.ua
```

The publishable/anon key is intended for browser use. The website account page
uses Supabase Auth's persistent browser session and supports sign-in, account
creation, and sign-out. Registration stores a 3–24 character username
(letters, numbers, and underscores) in the user's Supabase Auth metadata.

Configure the API with the same project URL and publishable/anon key:

```text
LAUNCHER_SUPABASE_URL=https://<project-ref>.supabase.co
LAUNCHER_SUPABASE_ANON_KEY=<publishable-or-anon-key>
```

The API validates user access tokens through Supabase Auth and exposes the
minimal identity check at `GET /api/v1/me`. It returns the user ID, email, and
username when present. If the two API variables are absent, the endpoint stays disabled and
the existing public catalog continues to work.

## Launcher handoff

For development and staging, the launcher can send a Supabase access token by
setting `LAUNCHER_ACCESS_TOKEN` in its process environment. The runtime adds it
as a Bearer token to API requests. The token is intentionally not stored in
the launcher's JSON settings file. When that token is present, the launcher
loads the profile and displays the username in its top bar.

The next launcher auth step is a browser/deep-link handoff so users do not
need to copy a token. That should be implemented with a short-lived one-time
code, not by storing a Supabase refresh token in the launcher settings file.
