using Launcher.Security;

namespace Launcher.Storage;

/// <summary>
/// Bounded local cache for immutable physical packs. Pack files are temporary
/// acceleration artifacts; logical chunks remain the durable install cache.
/// </summary>
public sealed class PackCache(string root, long maxBytes)
{
    private readonly string _root = Path.GetFullPath(root);
    private readonly long _maxBytes = maxBytes >= 0 ? maxBytes : throw new ArgumentOutOfRangeException(nameof(maxBytes));
    private readonly object _gate = new();
    private readonly Dictionary<string, CacheEntry> _entries = new(StringComparer.Ordinal);
    private readonly HashSet<string> _pinned = new(StringComparer.Ordinal);
    private long _currentBytes;

    public long CurrentBytes { get { lock (_gate) return _currentBytes; } }
    public string GetPath(string packHash) { ValidateHash(packHash); return Path.Combine(_root, $"{packHash}.pack"); }
    public string GetPartialPath(string packHash) { ValidateHash(packHash); return Path.Combine(_root, $"{packHash}.part"); }
    public void DeletePartial(string packHash) { ValidateHash(packHash); try { if (File.Exists(GetPartialPath(packHash))) File.Delete(GetPartialPath(packHash)); } catch (IOException) { } }

    public async Task InitializeAsync(CancellationToken cancellationToken = default)
    {
        Directory.CreateDirectory(_root);
        lock (_gate)
        {
            _entries.Clear();
            _currentBytes = 0;
        }
        foreach (var file in Directory.EnumerateFiles(_root, "*.pack"))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var hash = Path.GetFileNameWithoutExtension(file);
            if (!IsHash(hash)) continue;
            var info = new FileInfo(file);
            lock (_gate) { _entries[hash] = new CacheEntry(info.Length, info.LastAccessTimeUtc); _currentBytes += info.Length; }
        }
        await EvictIfNeededAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<byte[]?> ReadAsync(string packHash, CancellationToken cancellationToken = default)
    {
        var path = GetPath(packHash);
        if (!File.Exists(path)) return null;
        var bytes = await File.ReadAllBytesAsync(path, cancellationToken).ConfigureAwait(false);
        if (!string.Equals(Hashing.ComputeHash(bytes), packHash, StringComparison.Ordinal)) { await DeleteAsync(packHash).ConfigureAwait(false); return null; }
        lock (_gate) if (_entries.TryGetValue(packHash, out var entry)) _entries[packHash] = entry with { LastAccess = DateTime.UtcNow };
        return bytes;
    }

    public async Task PutAsync(string packHash, ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken = default)
    {
        ValidateHash(packHash);
        if (!string.Equals(Hashing.ComputeHash(bytes.Span), packHash, StringComparison.Ordinal)) throw new InvalidDataException("Physical pack hash mismatch.");
        Directory.CreateDirectory(_root);
        var path = GetPath(packHash);
        var temporary = path + $".{Guid.NewGuid():N}.part";
        try
        {
            await File.WriteAllBytesAsync(temporary, bytes.ToArray(), cancellationToken).ConfigureAwait(false);
            File.Move(temporary, path, true);
        }
        catch
        {
            try { if (File.Exists(temporary)) File.Delete(temporary); } catch (IOException) { }
            throw;
        }
        lock (_gate)
        {
            if (_entries.TryGetValue(packHash, out var existing)) _currentBytes -= existing.Size;
            _entries[packHash] = new CacheEntry(bytes.Length, DateTime.UtcNow);
            _currentBytes += bytes.Length;
        }
        await EvictIfNeededAsync(cancellationToken).ConfigureAwait(false);
    }

    public IDisposable Pin(string packHash)
    {
        ValidateHash(packHash);
        lock (_gate) _pinned.Add(packHash);
        return new PinLease(() => { lock (_gate) _pinned.Remove(packHash); });
    }

    private async Task EvictIfNeededAsync(CancellationToken cancellationToken)
    {
        while (true)
        {
            string? candidate;
            lock (_gate) candidate = _currentBytes <= _maxBytes ? null : _entries.Where(pair => !_pinned.Contains(pair.Key)).OrderBy(pair => pair.Value.LastAccess).Select(pair => pair.Key).FirstOrDefault();
            if (candidate is null) return;
            cancellationToken.ThrowIfCancellationRequested();
            await DeleteAsync(candidate).ConfigureAwait(false);
        }
    }

    private Task DeleteAsync(string packHash)
    {
        try { if (File.Exists(GetPath(packHash))) File.Delete(GetPath(packHash)); } finally { lock (_gate) { if (_entries.Remove(packHash, out var entry)) _currentBytes -= entry.Size; } }
        return Task.CompletedTask;
    }

    private static bool IsHash(string value) => value.Length == 64 && value.All(character => Uri.IsHexDigit(character) && !char.IsUpper(character));
    private static void ValidateHash(string hash) { if (!IsHash(hash)) throw new ArgumentException("Expected a lowercase BLAKE3 hash.", nameof(hash)); }
    private sealed record CacheEntry(long Size, DateTime LastAccess);
    private sealed class PinLease(Action release) : IDisposable { private Action? _release = release; public void Dispose() => Interlocked.Exchange(ref _release, null)?.Invoke(); }
}
