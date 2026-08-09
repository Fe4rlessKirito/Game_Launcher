using Launcher.Security;

namespace Launcher.Storage;

public sealed class ChunkCache(string root, long maxBytes)
{
    private readonly string _root = Path.GetFullPath(root);
    private readonly object _gate = new();
    private readonly Dictionary<string, CacheEntry> _entries = new(StringComparer.Ordinal);
    private readonly HashSet<string> _pinned = new(StringComparer.Ordinal);
    private long _currentBytes;

    public long CurrentBytes { get { lock (_gate) return _currentBytes; } }
    public string GetPath(string encodedHash) { ValidateHash(encodedHash); return Path.Combine(_root, $"{encodedHash}.bin"); }
    public string GetPartialPath(string encodedHash) { ValidateHash(encodedHash); return Path.Combine(_root, $"{encodedHash}.part"); }

    public async Task InitializeAsync(CancellationToken cancellationToken = default)
    {
        Directory.CreateDirectory(_root);
        foreach (var file in Directory.EnumerateFiles(_root, "*.bin"))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var hash = Path.GetFileNameWithoutExtension(file);
            if (hash.Length != 64) continue;
            var info = new FileInfo(file);
            lock (_gate) { _entries[hash] = new CacheEntry(info.Length, info.LastAccessTimeUtc); _currentBytes += info.Length; }
        }
        await EvictIfNeededAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<byte[]?> ReadAsync(string encodedHash, CancellationToken cancellationToken = default)
    {
        var path = GetPath(encodedHash);
        if (!File.Exists(path)) return null;
        var bytes = await File.ReadAllBytesAsync(path, cancellationToken).ConfigureAwait(false);
        if (!string.Equals(Hashing.ComputeHash(bytes), encodedHash, StringComparison.Ordinal)) { await DeleteAsync(encodedHash).ConfigureAwait(false); return null; }
        lock (_gate) if (_entries.TryGetValue(encodedHash, out var entry)) _entries[encodedHash] = entry with { LastAccess = DateTime.UtcNow };
        return bytes;
    }

    public async Task PutAsync(string encodedHash, ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken = default)
    {
        ValidateHash(encodedHash);
        if (!string.Equals(Hashing.ComputeHash(bytes.Span), encodedHash, StringComparison.Ordinal)) throw new InvalidDataException("Encoded chunk hash mismatch.");
        Directory.CreateDirectory(_root);
        var path = GetPath(encodedHash);
        var temp = path + $".{Guid.NewGuid():N}.part";
        await File.WriteAllBytesAsync(temp, bytes.ToArray(), cancellationToken).ConfigureAwait(false);
        File.Move(temp, path, true);
        lock (_gate)
        {
            if (_entries.TryGetValue(encodedHash, out var existing)) _currentBytes -= existing.Size;
            _entries[encodedHash] = new CacheEntry(bytes.Length, DateTime.UtcNow);
            _currentBytes += bytes.Length;
        }
        await EvictIfNeededAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task PutFileAsync(string encodedHash, string sourcePath, CancellationToken cancellationToken = default)
    {
        ValidateHash(encodedHash);
        if (!File.Exists(sourcePath)) throw new FileNotFoundException("Chunk source file was not found.", sourcePath);
        var actual = await Hashing.ComputeFileHashAsync(sourcePath, cancellationToken).ConfigureAwait(false);
        var length = new FileInfo(sourcePath).Length;
        if (!string.Equals(actual, encodedHash, StringComparison.Ordinal)) throw new InvalidDataException("Encoded chunk hash mismatch.");
        Directory.CreateDirectory(_root);
        var destination = GetPath(encodedHash);
        var temporary = destination + $".{Guid.NewGuid():N}.part";
        File.Move(sourcePath, temporary, true);
        File.Move(temporary, destination, true);
        lock (_gate)
        {
            if (_entries.TryGetValue(encodedHash, out var existing)) _currentBytes -= existing.Size;
            _entries[encodedHash] = new CacheEntry(length, DateTime.UtcNow);
            _currentBytes += length;
        }
        await EvictIfNeededAsync(cancellationToken).ConfigureAwait(false);
    }

    public IDisposable Pin(string encodedHash)
    {
        ValidateHash(encodedHash);
        lock (_gate) _pinned.Add(encodedHash);
        return new PinLease(() => { lock (_gate) _pinned.Remove(encodedHash); });
    }

    public async Task ClearAsync(CancellationToken cancellationToken = default)
    {
        string[] hashes;
        lock (_gate) hashes = _entries.Keys.Where(hash => !_pinned.Contains(hash)).ToArray();
        foreach (var hash in hashes) { cancellationToken.ThrowIfCancellationRequested(); await DeleteAsync(hash).ConfigureAwait(false); }
    }

    private async Task EvictIfNeededAsync(CancellationToken cancellationToken)
    {
        while (true)
        {
            string? candidate;
            lock (_gate) candidate = _currentBytes <= maxBytes ? null : _entries.Where(pair => !_pinned.Contains(pair.Key)).OrderBy(pair => pair.Value.LastAccess).Select(pair => pair.Key).FirstOrDefault();
            if (candidate is null) return;
            cancellationToken.ThrowIfCancellationRequested();
            await DeleteAsync(candidate).ConfigureAwait(false);
        }
    }

    private Task DeleteAsync(string encodedHash)
    {
        var path = GetPath(encodedHash);
        try { if (File.Exists(path)) File.Delete(path); } finally { lock (_gate) { if (_entries.Remove(encodedHash, out var entry)) _currentBytes -= entry.Size; } }
        return Task.CompletedTask;
    }

    private static void ValidateHash(string hash) { if (hash.Length != 64 || hash.Any(character => !Uri.IsHexDigit(character) || char.IsUpper(character))) throw new ArgumentException("Expected a lowercase BLAKE3 hash.", nameof(hash)); }
    private sealed record CacheEntry(long Size, DateTime LastAccess);
    private sealed class PinLease(Action release) : IDisposable { private Action? _release = release; public void Dispose() => Interlocked.Exchange(ref _release, null)?.Invoke(); }
}
