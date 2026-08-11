using System.Buffers.Binary;
using System.Net;
using System.Text;
using System.Text.Json;
using Launcher.Core;
using Launcher.Downloads;
using Launcher.Manifests;
using Launcher.Networking;
using Launcher.Security;
using Launcher.Storage;

namespace Launcher.Downloads.Tests;

public sealed class PhysicalPackDownloadTests
{
    [Fact]
    public async Task PackDownloadUsesDirectHotSourceAndMaterializesLogicalChunk()
    {
        var previous = Environment.GetEnvironmentVariable("LAUNCHER_PACK_DOWNLOAD_ENABLED");
        Environment.SetEnvironmentVariable("LAUNCHER_PACK_DOWNLOAD_ENABLED", "true");
        var root = Path.Combine(Path.GetTempPath(), "launcher-pack-download-" + Guid.NewGuid().ToString("N"));
        try
        {
            var raw = Encoding.UTF8.GetBytes("direct HOT physical pack");
            using var compressor = new ZstdSharp.Compressor(3);
            var encoded = compressor.Wrap(raw).ToArray();
            var encodedHash = Hashing.ComputeHash(encoded);
            var pack = BuildPack(raw, encoded);
            var packHash = Hashing.ComputeHash(pack);
            var handler = new DirectPackHandler(pack, packHash, encodedHash);
            using var client = new HttpClient(handler);
            var chunkCache = new ChunkCache(Path.Combine(root, "chunks"), 1024 * 1024);
            var packCache = new PackCache(Path.Combine(root, "packs"), 1024 * 1024);
            await chunkCache.InitializeAsync();
            using var manager = new DownloadManager(client, new LauncherApiClient(client, new Uri("http://launcher/")), chunkCache, 2, packCache: packCache);
            var chunk = new ChunkReference(Hashing.ComputeHash(raw), raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin");
            var manifest = new Manifest(1, "manifest", "game", "build", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("game.exe", raw.Length, Hashing.ComputeHash(raw), [chunk])], new LaunchProfile("game.exe", ".", [], new Dictionary<string, string>()));

            var summary = await manager.DownloadAsync(manifest, "pack-job");
            Assert.Equal(pack.Length, summary.NetworkBytes);
            Assert.Equal(encoded, await chunkCache.ReadAsync(encodedHash));
            Assert.Equal(1, handler.PackGets);
            Assert.Equal(0, handler.ChunkGets);
            Assert.True(handler.PackResolutionCalls > 0);
            Assert.DoesNotContain(handler.RequestedPaths, path => path.StartsWith("/objects/", StringComparison.Ordinal));
        }
        finally
        {
            Environment.SetEnvironmentVariable("LAUNCHER_PACK_DOWNLOAD_ENABLED", previous);
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    private sealed class DirectPackHandler(byte[] pack, string packHash, string encodedHash) : HttpMessageHandler
    {
        public int PackGets { get; private set; }
        public int ChunkGets { get; private set; }
        public int PackResolutionCalls { get; private set; }
        public List<string> RequestedPaths { get; } = [];

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var path = request.RequestUri!.AbsolutePath;
            RequestedPaths.Add(path);
            if (request.Method == HttpMethod.Post && path.Contains("/packs/resolve", StringComparison.Ordinal))
            {
                PackResolutionCalls++;
                return Task.FromResult(Json(new[] { new { pack_hash = packHash, encoded_size = pack.Length, chunk_hashes = new[] { encodedHash }, sources = new[] { new { provider = "hot", pool_id = "hot", provider_type = "local", failure_domain = "hot", url = $"http://hot/packs/{packHash}", expires_at = (DateTimeOffset?)null, range_supported = true, stable_url = true, priority = 0 } } } }));
            }
            if (request.Method == HttpMethod.Post)
            {
                return Task.FromResult(Json(new[] { new { encoded_hash = encodedHash, urls = new[] { $"http://hot/objects/{encodedHash}" }, expires_at = (DateTimeOffset?)null } }));
            }
            if (path == $"/packs/{packHash}")
            {
                PackGets++;
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(pack) });
            }
            if (path == $"/objects/{encodedHash}")
            {
                ChunkGets++;
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.InternalServerError));
            }
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound));
        }

        private static HttpResponseMessage Json<T>(T value) => new(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(value), Encoding.UTF8, "application/json") };
    }

    private static byte[] BuildPack(byte[] raw, byte[] encoded)
    {
        const int headerSize = 64;
        const int entrySize = 96;
        const int footerSize = 72;
        var indexOffset = headerSize + encoded.Length;
        var bytes = new byte[indexOffset + entrySize + footerSize];
        Encoding.ASCII.GetBytes("LGRPACK1").CopyTo(bytes, 0);
        BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(8, 2), 1);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(12, 4), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(16, 8), 1);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(24, 8), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(32, 8), (ulong)indexOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(40, 8), entrySize);
        encoded.CopyTo(bytes, headerSize);
        Convert.FromHexString(Hashing.ComputeHash(encoded)).CopyTo(bytes, indexOffset);
        Convert.FromHexString(Hashing.ComputeHash(raw)).CopyTo(bytes, indexOffset + 32);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(indexOffset + 64, 8), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(indexOffset + 72, 8), (ulong)encoded.Length);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(indexOffset + 80, 8), (ulong)raw.Length);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(indexOffset + 88, 4), 1);
        var footerOffset = indexOffset + entrySize;
        Encoding.ASCII.GetBytes("LGRPFTR1").CopyTo(bytes, footerOffset);
        BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(footerOffset + 8, 2), 1);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 12, 8), (ulong)indexOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 20, 8), entrySize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 28, 8), 1);
        Convert.FromHexString(Hashing.ComputeHash(bytes.AsSpan(indexOffset, entrySize))).CopyTo(bytes, footerOffset + 40);
        return bytes;
    }
}
