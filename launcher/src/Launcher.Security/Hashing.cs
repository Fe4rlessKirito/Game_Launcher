using Blake3;

namespace Launcher.Security;

public static class Hashing
{
    public static string ComputeHash(ReadOnlySpan<byte> bytes) => Hasher.Hash(bytes.ToArray()).ToString().ToLowerInvariant();

    public static async Task<string> ComputeFileHashAsync(string path, CancellationToken cancellationToken = default)
    {
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 1024 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
        using var hasher = Hasher.New();
        var buffer = new byte[1024 * 1024];
        int read;
        while ((read = await stream.ReadAsync(buffer.AsMemory(), cancellationToken).ConfigureAwait(false)) > 0) hasher.Update(buffer.AsSpan(0, read));
        return hasher.Finalize().ToString().ToLowerInvariant();
    }

    public static async Task<bool> VerifyFileAsync(string path, long expectedSize, string expectedHash, CancellationToken cancellationToken = default)
    {
        var info = new FileInfo(path);
        if (!info.Exists || info.Length != expectedSize) return false;
        return string.Equals(await ComputeFileHashAsync(path, cancellationToken).ConfigureAwait(false), expectedHash, StringComparison.Ordinal);
    }
}
