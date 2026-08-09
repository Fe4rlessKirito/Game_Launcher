using System.Text;
using System.Text.RegularExpressions;

namespace Launcher.Manifests;

public static partial class ManifestValidator
{
    public static void Validate(Manifest manifest)
    {
        ArgumentNullException.ThrowIfNull(manifest);
        if (manifest.SchemaVersion != 1) throw new InvalidDataException($"Unsupported manifest schema version {manifest.SchemaVersion}.");
        if (string.IsNullOrWhiteSpace(manifest.ManifestId) || string.IsNullOrWhiteSpace(manifest.GameId) || string.IsNullOrWhiteSpace(manifest.BuildId) || string.IsNullOrWhiteSpace(manifest.DisplayVersion)) throw new InvalidDataException("Manifest identity fields are required.");
        if (manifest.Chunking is null || manifest.Encoding is null || manifest.Files is null || manifest.Launch is null) throw new InvalidDataException("Manifest required sections are missing.");
        if (manifest.Chunking.Algorithm != "fastcdc" || manifest.Chunking.FormatVersion != 1 || manifest.Chunking.MinimumBytes <= 0 || manifest.Chunking.MinimumBytes > manifest.Chunking.AverageBytes || manifest.Chunking.AverageBytes > manifest.Chunking.MaximumBytes)
            throw new InvalidDataException("Invalid FastCDC parameters.");
        if (manifest.Encoding.Id != "zstd-v1-level-3" || manifest.Encoding.Level != 3) throw new InvalidDataException("Unsupported chunk encoding.");
        var paths = new HashSet<string>(StringComparer.Ordinal);
        var chunksByEncodedHash = new Dictionary<string, ChunkReference>(StringComparer.Ordinal);
        foreach (var file in manifest.Files)
        {
            if (file is null || file.Chunks is null) throw new InvalidDataException("Manifest file recipe is incomplete.");
            ValidatePortablePath(file.Path);
            if (!paths.Add(file.Path)) throw new InvalidDataException($"Duplicate manifest path: {file.Path}");
            if (file.Size < 0) throw new InvalidDataException($"Negative file size: {file.Path}");
            ValidateHash(file.Blake3, "file");
            if (file.Chunks.Any(chunk => chunk is null || chunk.RawSize <= 0 || chunk.EncodedSize <= 0)) throw new InvalidDataException($"Impossible chunk size in {file.Path}.");
            if (file.Size != file.Chunks.Sum(chunk => chunk.RawSize)) throw new InvalidDataException($"Chunk sizes do not match {file.Path}.");
            foreach (var chunk in file.Chunks)
            {
                ValidateHash(chunk.RawHash, "raw");
                ValidateHash(chunk.EncodedHash, "encoded");
                if (!string.Equals(chunk.ObjectKey, $"chunks/encoded/{chunk.EncodedHash}.bin", StringComparison.Ordinal)) throw new InvalidDataException("Chunk object key does not match encoded hash.");
                if (chunksByEncodedHash.TryGetValue(chunk.EncodedHash, out var existing) && !Equals(existing, chunk))
                {
                    if (!string.Equals(existing.RawHash, chunk.RawHash, StringComparison.Ordinal) || existing.RawSize != chunk.RawSize || existing.EncodedSize != chunk.EncodedSize || !string.Equals(existing.ObjectKey, chunk.ObjectKey, StringComparison.Ordinal)) throw new InvalidDataException($"Conflicting duplicate chunk metadata: {chunk.EncodedHash}");
                }
                else chunksByEncodedHash[chunk.EncodedHash] = chunk;
            }
        }
        ValidatePortablePath(manifest.Launch.Executable);
        if (manifest.Launch.WorkingDirectory != ".") ValidatePortablePath(manifest.Launch.WorkingDirectory);
        if (!manifest.Files.Any(file => file.Path == manifest.Launch.Executable)) throw new InvalidDataException("Launch executable is not owned by manifest.");
    }

    public static void ValidatePortablePath(string portablePath)
    {
        if (string.IsNullOrWhiteSpace(portablePath) || portablePath.Contains('\\') || portablePath.StartsWith('/') || portablePath.StartsWith("//", StringComparison.Ordinal) || portablePath.Contains(':', StringComparison.Ordinal)) throw new InvalidDataException($"Invalid manifest path: {portablePath}");
        var parts = portablePath.Split('/');
        if (parts.Any(part => part.Length == 0 || part is "." or "..")) throw new InvalidDataException($"Invalid manifest path: {portablePath}");
        if (parts.Any(part => part.EndsWith(' ') || part.EndsWith('.') || IsReservedWindowsName(part))) throw new InvalidDataException($"Invalid Windows-compatible manifest path: {portablePath}");
        if (portablePath.Normalize(NormalizationForm.FormC) != portablePath) throw new InvalidDataException($"Manifest path is not NFC-normalized: {portablePath}");
        if (!PortablePathRegex().IsMatch(portablePath)) throw new InvalidDataException($"Invalid manifest path: {portablePath}");
    }

    private static bool IsReservedWindowsName(string part)
    {
        var stem = part.Split('.')[0].ToUpperInvariant();
        return stem is "CON" or "PRN" or "AUX" or "NUL" || (stem.Length == 4 && (stem.StartsWith("COM", StringComparison.Ordinal) || stem.StartsWith("LPT", StringComparison.Ordinal)) && char.IsDigit(stem[3]) && stem[3] is >= '1' and <= '9');
    }

    private static void ValidateHash(string value, string kind)
    {
        if (value.Length != 64 || value.Any(character => !Uri.IsHexDigit(character) || char.IsUpper(character))) throw new InvalidDataException($"Invalid {kind} BLAKE3 hash.");
    }

    [GeneratedRegex("^[^\\u0000-\\u001f]+$")]
    private static partial Regex PortablePathRegex();
}
