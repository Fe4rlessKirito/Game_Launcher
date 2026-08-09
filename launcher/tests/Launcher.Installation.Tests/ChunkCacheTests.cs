using Launcher.Security;
using Launcher.Storage;

namespace Launcher.Installation.Tests;

public sealed class ChunkCacheTests
{
    [Fact]
    public async Task InsertReadCorruptionClearAndBoundedEvictionWork()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-cache-" + Guid.NewGuid().ToString("N"));
        try
        {
            var cache = new ChunkCache(root, 8);
            await cache.InitializeAsync();
            var first = new byte[] { 1, 2, 3, 4, 5 };
            var second = new byte[] { 6, 7, 8, 9, 10 };
            var firstHash = Hashing.ComputeHash(first);
            var secondHash = Hashing.ComputeHash(second);
            await cache.PutAsync(firstHash, first);
            using (cache.Pin(firstHash)) await cache.PutAsync(secondHash, second);
            Assert.Equal(first, await cache.ReadAsync(firstHash));
            await File.WriteAllBytesAsync(cache.GetPath(firstHash), new byte[] { 99 });
            Assert.Null(await cache.ReadAsync(firstHash));
            Assert.True(cache.CurrentBytes <= 8);
            await cache.ClearAsync();
            Assert.Null(await cache.ReadAsync(secondHash));
        }
        finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
    }

    [Fact]
    public async Task ConcurrentReadersSeeVerifiedBytes()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-cache-" + Guid.NewGuid().ToString("N"));
        try
        {
            var cache = new ChunkCache(root, 1024 * 1024);
            await cache.InitializeAsync();
            var bytes = Enumerable.Range(0, 64 * 1024).Select(index => (byte)(index % 251)).ToArray();
            var hash = Hashing.ComputeHash(bytes);
            await cache.PutAsync(hash, bytes);
            var reads = await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => cache.ReadAsync(hash)));
            Assert.All(reads, actual => Assert.Equal(bytes, actual));
        }
        finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
    }
}
