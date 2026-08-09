using System.Text.Json;
using System.Text.Json.Serialization;

namespace Launcher.Manifests;

public sealed record ChunkingConfig(
    [property: JsonPropertyName("algorithm")] string Algorithm,
    [property: JsonPropertyName("format_version")] int FormatVersion,
    [property: JsonPropertyName("minimum_bytes")] long MinimumBytes,
    [property: JsonPropertyName("average_bytes")] long AverageBytes,
    [property: JsonPropertyName("maximum_bytes")] long MaximumBytes)
{
    public static ChunkingConfig Default => new("fastcdc", 1, 1 * 1024 * 1024, 4 * 1024 * 1024, 16 * 1024 * 1024);
}

public sealed record EncodingConfig(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("level")] int Level)
{
    public static EncodingConfig Default => new("zstd-v1-level-3", 3);
}

public sealed record ChunkReference(
    [property: JsonPropertyName("raw_hash")] string RawHash,
    [property: JsonPropertyName("raw_size")] long RawSize,
    [property: JsonPropertyName("encoded_hash")] string EncodedHash,
    [property: JsonPropertyName("encoded_size")] long EncodedSize,
    [property: JsonPropertyName("object_key")] string ObjectKey);

public sealed record FileRecipe(
    [property: JsonPropertyName("path")] string Path,
    [property: JsonPropertyName("size")] long Size,
    [property: JsonPropertyName("blake3")] string Blake3,
    [property: JsonPropertyName("chunks")] IReadOnlyList<ChunkReference> Chunks);

public sealed record LaunchProfile(
    [property: JsonPropertyName("executable")] string Executable,
    [property: JsonPropertyName("working_directory")] string WorkingDirectory,
    [property: JsonPropertyName("arguments")] IReadOnlyList<string> Arguments,
    [property: JsonPropertyName("environment")] IReadOnlyDictionary<string, string> Environment);

public sealed record Manifest(
    [property: JsonPropertyName("schema_version")] int SchemaVersion,
    [property: JsonPropertyName("manifest_id")] string ManifestId,
    [property: JsonPropertyName("game_id")] string GameId,
    [property: JsonPropertyName("build_id")] string BuildId,
    [property: JsonPropertyName("display_version")] string DisplayVersion,
    [property: JsonPropertyName("generated_at")] DateTimeOffset GeneratedAt,
    [property: JsonPropertyName("chunking")] ChunkingConfig Chunking,
    [property: JsonPropertyName("encoding")] EncodingConfig Encoding,
    [property: JsonPropertyName("files")] IReadOnlyList<FileRecipe> Files,
    [property: JsonPropertyName("launch")] LaunchProfile Launch);

public static class ManifestJson
{
    public static readonly JsonSerializerOptions Options = new(JsonSerializerDefaults.Web)
    {
        PropertyNamingPolicy = null,
        WriteIndented = true,
        ReadCommentHandling = JsonCommentHandling.Disallow,
        AllowTrailingCommas = false
    };

    public static Manifest Deserialize(string json) => JsonSerializer.Deserialize<Manifest>(json, Options) ?? throw new JsonException("manifest payload was empty");

    public static string Serialize(Manifest manifest) => JsonSerializer.Serialize(manifest, Options);
}
