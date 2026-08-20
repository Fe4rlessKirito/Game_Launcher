using System.Net.Http.Json;
using System.Net;
using System.Text.Json;
using System.Text.Json.Serialization;
using Launcher.Core;
using Launcher.Manifests;

namespace Launcher.Networking;

public sealed record ResolvedChunk(
    [property: JsonPropertyName("encoded_hash")] string EncodedHash,
    [property: JsonPropertyName("urls")] IReadOnlyList<string> Urls,
    [property: JsonPropertyName("expires_at")] DateTimeOffset? ExpiresAt);

public sealed record ResolvedPackSource(
    [property: JsonPropertyName("provider")] string Provider,
    [property: JsonPropertyName("pool_id")] string PoolId,
    [property: JsonPropertyName("provider_type")] string ProviderType,
    [property: JsonPropertyName("failure_domain")] string FailureDomain,
    [property: JsonPropertyName("url")] string Url,
    [property: JsonPropertyName("expires_at")] DateTimeOffset? ExpiresAt,
    [property: JsonPropertyName("range_supported")] bool RangeSupported,
    [property: JsonPropertyName("stable_url")] bool StableUrl,
    [property: JsonPropertyName("priority")] int Priority);

public sealed record ResolvedPack(
    [property: JsonPropertyName("pack_hash")] string PackHash,
    [property: JsonPropertyName("encoded_size")] long EncodedSize,
    [property: JsonPropertyName("chunk_hashes")] IReadOnlyList<string> ChunkHashes,
    [property: JsonPropertyName("sources")] IReadOnlyList<ResolvedPackSource> Sources);

public sealed record LauncherUserProfile(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("email")] string? Email,
    [property: JsonPropertyName("username")] string? Username);

public sealed record SupabaseAuthSession(string AccessToken);

public sealed class SupabaseAuthClient(HttpClient httpClient, Uri baseUri, string publishableKey)
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public async Task<SupabaseAuthSession> SignInWithPasswordAsync(
        string email,
        string password,
        CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(publishableKey))
        {
            throw new InvalidOperationException("Supabase sign-in is not configured for this launcher build.");
        }

        using var request = new HttpRequestMessage(HttpMethod.Post, new Uri(baseUri, "auth/v1/token?grant_type=password"));
        request.Headers.TryAddWithoutValidation("apikey", publishableKey);
        request.Content = JsonContent.Create(new { email, password }, options: JsonOptions);
        using var response = await httpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        var body = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException(ReadAuthError(body, response.StatusCode));
        }

        var session = JsonSerializer.Deserialize<AuthTokenResponse>(body, JsonOptions);
        if (session?.AccessToken is not { Length: > 0 } accessToken)
        {
            throw new InvalidOperationException("Supabase returned no access token.");
        }

        return new SupabaseAuthSession(accessToken);
    }

    public async Task UpdateUsernameAsync(
        string accessToken,
        string username,
        CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(publishableKey))
        {
            throw new InvalidOperationException("Supabase account updates are not configured for this launcher build.");
        }

        using var request = new HttpRequestMessage(HttpMethod.Put, new Uri(baseUri, "auth/v1/user"));
        request.Headers.TryAddWithoutValidation("apikey", publishableKey);
        request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {accessToken}");
        request.Content = JsonContent.Create(new { data = new { username } }, options: JsonOptions);
        using var response = await httpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        var body = await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException(ReadAuthError(body, response.StatusCode));
        }
    }

    private static string ReadAuthError(string body, HttpStatusCode statusCode)
    {
        try
        {
            var error = JsonSerializer.Deserialize<AuthErrorResponse>(body, JsonOptions);
            var message = error?.ErrorDescription ?? error?.Message ?? error?.Error;
            if (!string.IsNullOrWhiteSpace(message)) return message;
        }
        catch (JsonException)
        {
            // Fall through to a safe status-based message.
        }

        return $"Sign-in failed ({(int)statusCode} {statusCode}).";
    }

    private sealed record AuthTokenResponse([property: JsonPropertyName("access_token")] string? AccessToken);
    private sealed record AuthErrorResponse(
        [property: JsonPropertyName("error_description")] string? ErrorDescription,
        [property: JsonPropertyName("msg")] string? Message,
        [property: JsonPropertyName("error")] string? Error);
}

public sealed class LauncherApiClient(HttpClient httpClient, Uri baseUri)
{
    private const int MaxRestorePolls = 20;
    private static readonly TimeSpan DefaultRestorePollDelay = TimeSpan.FromSeconds(5);
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web) { PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower };

    public async Task<IReadOnlyList<GameCatalogItem>> GetGamesAsync(CancellationToken cancellationToken = default)
    {
        var games = new List<GameCatalogItem>();
        var seenCursors = new HashSet<string>(StringComparer.Ordinal);
        string? cursor = null;

        do
        {
            var query = cursor is null
                ? "api/v1/games?limit=100"
                : $"api/v1/games?limit=100&cursor={Uri.EscapeDataString(cursor)}";
            var response = await httpClient.GetFromJsonAsync<CatalogResponse>(new Uri(baseUri, query), JsonOptions, cancellationToken).ConfigureAwait(false);
            if (response is null) throw new InvalidDataException("API returned an empty catalog page.");
            foreach (var item in response.Items ?? [])
            {
                ValidateCatalogItem(item);
                games.Add(item);
            }

            cursor = response.NextCursor;
            if (cursor is not null && !seenCursors.Add(cursor))
            {
                throw new HttpRequestException("The catalog returned a repeated pagination cursor.");
            }
        }
        while (cursor is not null);

        return games;
    }

    public async Task<LauncherUserProfile> GetCurrentUserAsync(CancellationToken cancellationToken = default)
    {
        return await httpClient.GetFromJsonAsync<LauncherUserProfile>(new Uri(baseUri, "api/v1/me"), JsonOptions, cancellationToken).ConfigureAwait(false)
            ?? throw new HttpRequestException("API returned an empty user profile.");
    }

    public async Task<Manifest> GetManifestAsync(string buildId, CancellationToken cancellationToken = default)
    {
        var manifest = await httpClient.GetFromJsonAsync<Manifest>(new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/manifest"), JsonOptions, cancellationToken).ConfigureAwait(false) ?? throw new HttpRequestException("API returned an empty manifest.");
        ManifestValidator.Validate(manifest);
        return manifest;
    }

    public async Task<(Manifest Manifest, byte[] RawBytes)> GetManifestWithBytesAsync(string buildId, CancellationToken cancellationToken = default)
    {
        using var response = await httpClient.GetAsync(new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/manifest"), HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
        var manifest = ManifestJson.Deserialize(System.Text.Encoding.UTF8.GetString(bytes));
        ManifestValidator.Validate(manifest);
        return (manifest, bytes);
    }

    public async Task<ManifestSignature> GetManifestSignatureAsync(string buildId, CancellationToken cancellationToken = default)
    {
        return await httpClient.GetFromJsonAsync<ManifestSignature>(new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/signature"), JsonOptions, cancellationToken).ConfigureAwait(false)
            ?? throw new HttpRequestException("API returned an empty manifest signature.");
    }

    public async Task<IReadOnlyDictionary<string, ResolvedChunk>> ResolveChunksAsync(string buildId, IReadOnlyCollection<string> encodedHashes, CancellationToken cancellationToken = default)
    {
        var endpoint = new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/resolve");
        for (var attempt = 0; ; attempt++)
        {
            using var response = await httpClient.PostAsJsonAsync(endpoint, new ChunkResolutionRequest(encodedHashes), JsonOptions, cancellationToken).ConfigureAwait(false);
            if (response.StatusCode != HttpStatusCode.ServiceUnavailable || response.Headers.RetryAfter is null || attempt >= MaxRestorePolls - 1)
            {
                response.EnsureSuccessStatusCode();
                var resolved = await response.Content.ReadFromJsonAsync<IReadOnlyList<ResolvedChunk>>(JsonOptions, cancellationToken).ConfigureAwait(false) ?? [];
                foreach (var item in resolved)
                {
                    if (item is null || !IsHash(item.EncodedHash) || item.Urls is null || item.Urls.Count == 0 || item.Urls.Any(url => !IsHttpUrl(url)))
                    {
                        throw new InvalidDataException("API returned an invalid chunk location.");
                    }
                }
                return resolved.ToDictionary(item => item.EncodedHash, StringComparer.Ordinal);
            }

            var delay = response.Headers.RetryAfter?.Delta ?? DefaultRestorePollDelay;
            await Task.Delay(TimeSpan.FromSeconds(Math.Clamp(delay.TotalSeconds, 1, 30)), cancellationToken).ConfigureAwait(false);
        }
    }

    public async Task<IReadOnlyList<ResolvedPack>> ResolvePacksAsync(string buildId, IReadOnlyCollection<string> encodedHashes, CancellationToken cancellationToken = default)
    {
        var endpoint = new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/packs/resolve");
        // Physical packs are an optional acceleration path. A deployment may
        // legitimately have pack storage disabled, so do not interpret every
        // 503/Retry-After response as a cold restore. The caller catches this
        // failure and immediately falls back to logical chunk resolution.
        using var response = await httpClient.PostAsJsonAsync(endpoint, new PackResolutionRequest(encodedHashes), JsonOptions, cancellationToken).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        var packs = await response.Content.ReadFromJsonAsync<IReadOnlyList<ResolvedPack>>(JsonOptions, cancellationToken).ConfigureAwait(false) ?? [];
        foreach (var pack in packs)
        {
            if (pack is null || !IsHash(pack.PackHash) || pack.EncodedSize <= 0 || pack.ChunkHashes is null || pack.ChunkHashes.Count == 0 || pack.ChunkHashes.Any(hash => !IsHash(hash)) || pack.Sources is null)
            {
                throw new InvalidDataException("API returned an invalid physical pack location.");
            }

            foreach (var source in pack.Sources)
            {
                if (source is null || !IsHttpUrl(source.Url) || string.IsNullOrWhiteSpace(source.Provider))
                {
                    throw new InvalidDataException("API returned an invalid physical pack source.");
                }
            }
        }
        return packs;
    }

    private static bool IsHash(string? value) => value is { Length: 64 } && value.All(character => Uri.IsHexDigit(character) && !char.IsUpper(character));

    private static bool IsHttpUrl(string? value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri)
        && uri.Scheme is "http" or "https";

    private static void ValidateCatalogItem(GameCatalogItem? item)
    {
        if (item is null
            || string.IsNullOrWhiteSpace(item.Id)
            || string.IsNullOrWhiteSpace(item.Title)
            || item.LatestBuild is { } build && (string.IsNullOrWhiteSpace(build.Id)
                || string.IsNullOrWhiteSpace(build.GameId)
                || !string.Equals(build.GameId, item.Id, StringComparison.OrdinalIgnoreCase)
                || string.IsNullOrWhiteSpace(build.DisplayVersion)
                || build.SizeBytes < 0))
        {
            throw new InvalidDataException("API returned an invalid catalog item.");
        }
    }

    private sealed record CatalogResponse([property: JsonPropertyName("items")] IReadOnlyList<GameCatalogItem>? Items, [property: JsonPropertyName("next_cursor")] string? NextCursor);
    private sealed record ChunkResolutionRequest([property: JsonPropertyName("encoded_hashes")] IReadOnlyCollection<string> EncodedHashes);
    private sealed record PackResolutionRequest([property: JsonPropertyName("encoded_hashes")] IReadOnlyCollection<string> EncodedHashes);
}
