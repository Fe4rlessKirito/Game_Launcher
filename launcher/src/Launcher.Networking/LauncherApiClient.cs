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

public sealed class LauncherApiClient(HttpClient httpClient, Uri baseUri)
{
    private const int MaxRestorePolls = 20;
    private static readonly TimeSpan DefaultRestorePollDelay = TimeSpan.FromSeconds(5);
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web) { PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower };

    public async Task<IReadOnlyList<GameCatalogItem>> GetGamesAsync(CancellationToken cancellationToken = default)
    {
        var response = await httpClient.GetFromJsonAsync<CatalogResponse>(new Uri(baseUri, "api/v1/games"), JsonOptions, cancellationToken).ConfigureAwait(false);
        return response?.Items ?? [];
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
                return resolved.ToDictionary(item => item.EncodedHash, StringComparer.Ordinal);
            }

            var delay = response.Headers.RetryAfter?.Delta ?? DefaultRestorePollDelay;
            await Task.Delay(TimeSpan.FromSeconds(Math.Clamp(delay.TotalSeconds, 1, 30)), cancellationToken).ConfigureAwait(false);
        }
    }

    public async Task<IReadOnlyList<ResolvedPack>> ResolvePacksAsync(string buildId, IReadOnlyCollection<string> encodedHashes, CancellationToken cancellationToken = default)
    {
        using var response = await httpClient.PostAsJsonAsync(new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/packs/resolve"), new PackResolutionRequest(encodedHashes), JsonOptions, cancellationToken).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        return await response.Content.ReadFromJsonAsync<IReadOnlyList<ResolvedPack>>(JsonOptions, cancellationToken).ConfigureAwait(false) ?? [];
    }

    private sealed record CatalogResponse([property: JsonPropertyName("items")] IReadOnlyList<GameCatalogItem> Items, [property: JsonPropertyName("next_cursor")] string? NextCursor);
    private sealed record ChunkResolutionRequest([property: JsonPropertyName("encoded_hashes")] IReadOnlyCollection<string> EncodedHashes);
    private sealed record PackResolutionRequest([property: JsonPropertyName("encoded_hashes")] IReadOnlyCollection<string> EncodedHashes);
}
