using System.Net.Http.Json;
using System.Text.Json;
using System.Text.Json.Serialization;
using Launcher.Core;
using Launcher.Manifests;

namespace Launcher.Networking;

public sealed record ResolvedChunk(
    [property: JsonPropertyName("encoded_hash")] string EncodedHash,
    [property: JsonPropertyName("urls")] IReadOnlyList<string> Urls,
    [property: JsonPropertyName("expires_at")] DateTimeOffset? ExpiresAt);

public sealed class LauncherApiClient(HttpClient httpClient, Uri baseUri)
{
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
        using var response = await httpClient.PostAsJsonAsync(new Uri(baseUri, $"api/v1/builds/{Uri.EscapeDataString(buildId)}/resolve"), new ChunkResolutionRequest(encodedHashes), JsonOptions, cancellationToken).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        var resolved = await response.Content.ReadFromJsonAsync<IReadOnlyList<ResolvedChunk>>(JsonOptions, cancellationToken).ConfigureAwait(false) ?? [];
        return resolved.ToDictionary(item => item.EncodedHash, StringComparer.Ordinal);
    }

    private sealed record CatalogResponse([property: JsonPropertyName("items")] IReadOnlyList<GameCatalogItem> Items, [property: JsonPropertyName("next_cursor")] string? NextCursor);
    private sealed record ChunkResolutionRequest([property: JsonPropertyName("encoded_hashes")] IReadOnlyCollection<string> EncodedHashes);
}
