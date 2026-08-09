using System.Text.RegularExpressions;

namespace Launcher.Manifests;

public static partial class ManifestValidator
{
    public static void Validate(Manifest manifest)
    {
        if (manifest.SchemaVersion != 1) throw new InvalidDataException($"Unsupported manifest schema version {manifest.SchemaVersion}.");
        if (manifest.Chunking.Algorithm != "fastcdc" || manifest.Chunking.FormatVersion != 1 || manifest.Chunking.MinimumBytes <= 0 || manifest.Chunking.MinimumBytes > manifest.Chunking.AverageBytes || manifest.Chunking.AverageBytes > manifest.Chunking.MaximumBytes)
            throw new InvalidDataException("Invalid FastCDC parameters.");
        if (manifest.Encoding.Id != "zstd-v1-level-3" || manifest.Encoding.Level != 3) throw new InvalidDataException("Unsupported chunk encoding.");
        var paths = new HashSet<string>(StringComparer.Ordinal);
        foreach (var file in manifest.Files)
        {
            ValidatePortablePath(file.Path);
            if (!paths.Add(file.Path)) throw new InvalidDataException($"Duplicate manifest path: {file.Path}");
            ValidateHash(file.Blake3, "file");
            if (file.Size != file.Chunks.Sum(chunk => chunk.RawSize)) throw new InvalidDataException($"Chunk sizes do not match {file.Path}.");
            foreach (var chunk in file.Chunks)
            {
                ValidateHash(chunk.RawHash, "raw");
                ValidateHash(chunk.EncodedHash, "encoded");
                if (!string.Equals(chunk.ObjectKey, $"chunks/encoded/{chunk.EncodedHash}.bin", StringComparison.Ordinal)) throw new InvalidDataException("Chunk object key does not match encoded hash.");
            }
        }
        ValidatePortablePath(manifest.Launch.Executable);
        if (!manifest.Files.Any(file => file.Path == manifest.Launch.Executable)) throw new InvalidDataException("Launch executable is not owned by manifest.");
    }

    public static void ValidatePortablePath(string portablePath)
    {
        if (string.IsNullOrWhiteSpace(portablePath) || portablePath.Contains('\\') || portablePath.StartsWith('/') || portablePath.Contains(':', StringComparison.Ordinal)) throw new InvalidDataException($"Invalid manifest path: {portablePath}");
        var parts = portablePath.Split('/');
        if (parts.Any(part => part.Length == 0 || part is "." or "..")) throw new InvalidDataException($"Invalid manifest path: {portablePath}");
        if (!PortablePathRegex().IsMatch(portablePath)) throw new InvalidDataException($"Invalid manifest path: {portablePath}");
    }

    private static void ValidateHash(string value, string kind)
    {
        if (value.Length != 64 || value.Any(character => !Uri.IsHexDigit(character) || char.IsUpper(character))) throw new InvalidDataException($"Invalid {kind} BLAKE3 hash.");
    }

    [GeneratedRegex("^[^\\u0000-\\u001f]+$")]
    private static partial Regex PortablePathRegex();
}
