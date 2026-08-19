using System.Buffers.Binary;
using System.Net;
using System.Net.Http.Headers;
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
            using var manager = new DownloadManager(client, new LauncherApiClient(client, new Uri("http://launcher/")), chunkCache, 2, packCache: packCache, packDownloadEnabled: true);
            var chunk = new ChunkReference(Hashing.ComputeHash(raw), raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin");
            var manifest = new Manifest(1, "manifest", "game", "build", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("game.exe", raw.Length, Hashing.ComputeHash(raw), [chunk])], new LaunchProfile("game.exe", ".", [], new Dictionary<string, string>()));

            var summary = await manager.DownloadAsync(manifest, "pack-job");
            Assert.Equal(pack.Length, summary.NetworkBytes);
            Assert.Equal(pack.Length, summary.PhysicalPackNetworkBytes);
            Assert.Equal(encoded.Length, summary.PhysicalPackLogicalBytes);
            Assert.Equal((double)pack.Length / encoded.Length, summary.PhysicalPackAmplification, 8);
            Assert.Equal(encoded, await chunkCache.ReadAsync(encodedHash));
            Assert.Equal(1, handler.PackGets);
            Assert.Equal(0, handler.ChunkGets);
            Assert.True(handler.PackResolutionCalls > 0);
            Assert.DoesNotContain(handler.RequestedPaths, path => path.StartsWith("/objects/", StringComparison.Ordinal));
        }
        finally
        {
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task PackDownloadResumesPartialAndReportsObservedAmplification()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-pack-resume-" + Guid.NewGuid().ToString("N"));
        try
        {
            var raw = Encoding.UTF8.GetBytes("resumable direct HOT physical pack payload");
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
            await packCache.InitializeAsync();
            var partialLength = pack.Length / 2;
            await File.WriteAllBytesAsync(packCache.GetPartialPath(packHash), pack[..partialLength]);
            using var manager = new DownloadManager(client, new LauncherApiClient(client, new Uri("http://launcher/")), chunkCache, 2, packCache: packCache, packDownloadEnabled: true);
            var chunk = new ChunkReference(Hashing.ComputeHash(raw), raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin");
            var manifest = new Manifest(1, "manifest", "game", "build", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("game.exe", raw.Length, Hashing.ComputeHash(raw), [chunk])], new LaunchProfile("game.exe", ".", [], new Dictionary<string, string>()));

            var summary = await manager.DownloadAsync(manifest, "pack-resume-job");

            Assert.Equal(pack.Length - partialLength, summary.NetworkBytes);
            Assert.Equal(pack.Length - partialLength, summary.PhysicalPackNetworkBytes);
            Assert.Equal(encoded.Length, summary.PhysicalPackLogicalBytes);
            Assert.Equal((double)(pack.Length - partialLength) / encoded.Length, summary.PhysicalPackAmplification, 8);
            Assert.Equal(encoded, await chunkCache.ReadAsync(encodedHash));
            Assert.Contains(partialLength, handler.PackRanges);
            Assert.False(File.Exists(packCache.GetPartialPath(packHash)));
        }
        finally
        {
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task SparseUpdateRelaysOnlyUncachedChunkWhenPackCoverageIsSmall()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-pack-sparse-" + Guid.NewGuid().ToString("N"));
        try
        {
            var cachedRaw = Encoding.UTF8.GetBytes(new string('a', 16_384));
            var missingRaw = Encoding.UTF8.GetBytes(new string('b', 16_384));
            using var compressor = new ZstdSharp.Compressor(3);
            var cachedEncoded = compressor.Wrap(cachedRaw).ToArray();
            var missingEncoded = compressor.Wrap(missingRaw).ToArray();
            var cachedHash = Hashing.ComputeHash(cachedEncoded);
            var missingHash = Hashing.ComputeHash(missingEncoded);
            var pack = BuildPack((cachedRaw, cachedEncoded), (missingRaw, missingEncoded));
            var packHash = Hashing.ComputeHash(pack);
            var handler = new SparsePackHandler(pack, packHash, cachedHash, missingHash);
            using var client = new HttpClient(handler);
            var chunkCache = new ChunkCache(Path.Combine(root, "chunks"), 1024 * 1024);
            var packCache = new PackCache(Path.Combine(root, "packs"), 1024 * 1024);
            await chunkCache.InitializeAsync();
            await chunkCache.PutAsync(cachedHash, cachedEncoded);
            using var manager = new DownloadManager(
                client,
                new LauncherApiClient(client, new Uri("http://launcher/")),
                chunkCache,
                2,
                packCache: packCache,
                packDownloadEnabled: true,
                sparseRelayThreshold: 0.5);
            var chunks = new[]
            {
                new ChunkReference(Hashing.ComputeHash(cachedRaw), cachedRaw.Length, cachedHash, cachedEncoded.Length, $"chunks/encoded/{cachedHash}.bin"),
                new ChunkReference(Hashing.ComputeHash(missingRaw), missingRaw.Length, missingHash, missingEncoded.Length, $"chunks/encoded/{missingHash}.bin")
            };
            var manifest = new Manifest(
                1,
                "manifest",
                "game",
                "build",
                "B",
                DateTimeOffset.UnixEpoch,
                ChunkingConfig.Default,
                EncodingConfig.Default,
                [new FileRecipe("game.exe", cachedRaw.Length + missingRaw.Length, Hashing.ComputeHash(cachedRaw.Concat(missingRaw).ToArray()), chunks)],
                new LaunchProfile("game.exe", ".", [], new Dictionary<string, string>()));

            var summary = await manager.DownloadAsync(manifest, "sparse-job");

            Assert.Equal(missingEncoded.Length, summary.NetworkBytes);
            Assert.Equal(0, summary.PhysicalPackNetworkBytes);
            Assert.Equal(0, summary.PhysicalPackLogicalBytes);
            Assert.Equal(0, handler.PackGets);
            Assert.Equal(1, handler.SparseRelayGets);
            Assert.Contains(missingHash, handler.PackResolutionRequest);
            Assert.DoesNotContain(cachedHash, handler.PackResolutionRequest);
            Assert.Equal(missingEncoded, await chunkCache.ReadAsync(missingHash));
        }
        finally
        {
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task PackDownloadFailsOverRefreshesAndRejectsCorruption()
    {
        var raw = Encoding.UTF8.GetBytes("physical pack fault injection payload");
        using var compressor = new ZstdSharp.Compressor(3);
        var encoded = compressor.Wrap(raw).ToArray();
        var encodedHash = Hashing.ComputeHash(encoded);
        var pack = BuildPack(raw, encoded);
        var packHash = Hashing.ComputeHash(pack);

        await RunPackScenarioAsync(
            "pack-failover-job",
            new DirectPackHandler(pack, packHash, encodedHash, failFirstPackSource: true),
            raw,
            encoded,
            encodedHash,
            pack.Length,
            handler => Assert.Contains("bad", handler.PackHosts));
        await RunPackScenarioAsync(
            "pack-refresh-job",
            new DirectPackHandler(pack, packHash, encodedHash, expiredFirstPackResolution: true),
            raw,
            encoded,
            encodedHash,
            pack.Length,
            handler =>
            {
                Assert.True(handler.PackResolutionCalls >= 2);
                Assert.Contains("expired", handler.PackHosts);
                Assert.Contains("hot", handler.PackHosts);
            });
        await RunPackScenarioAsync(
            "pack-corrupt-failover-job",
            new DirectPackHandler(pack, packHash, encodedHash, corruptFirstPackSource: true),
            raw,
            encoded,
            encodedHash,
            pack.Length,
            handler =>
            {
                Assert.Contains("bad", handler.PackHosts);
                Assert.Contains("hot", handler.PackHosts);
            });

        var corruptRoot = Path.Combine(Path.GetTempPath(), "launcher-pack-corrupt-" + Guid.NewGuid().ToString("N"));
        try
        {
            var corruptHandler = new DirectPackHandler(pack, packHash, encodedHash, corruptPack: true);
            using var corruptClient = new HttpClient(corruptHandler);
            var corruptCache = new ChunkCache(Path.Combine(corruptRoot, "chunks"), 1024 * 1024);
            var corruptPackCache = new PackCache(Path.Combine(corruptRoot, "packs"), 1024 * 1024);
            await corruptCache.InitializeAsync();
            using var corruptManager = new DownloadManager(corruptClient, new LauncherApiClient(corruptClient, new Uri("http://launcher/")), corruptCache, 2, packCache: corruptPackCache, packDownloadEnabled: true);

            await Assert.ThrowsAsync<LauncherOperationException>(() => corruptManager.DownloadAsync(BuildManifest(raw, encoded, encodedHash), "pack-corrupt-job"));

            Assert.False(File.Exists(corruptPackCache.GetPath(packHash)));
            Assert.False(File.Exists(corruptPackCache.GetPartialPath(packHash)));
        }
        finally
        {
            if (Directory.Exists(corruptRoot)) Directory.Delete(corruptRoot, true);
        }

        async Task RunPackScenarioAsync(string jobId, DirectPackHandler handler, byte[] scenarioRaw, byte[] scenarioEncoded, string scenarioEncodedHash, int scenarioPackLength, Action<DirectPackHandler> assertHandler)
        {
            var root = Path.Combine(Path.GetTempPath(), "launcher-pack-scenario-" + Guid.NewGuid().ToString("N"));
            try
            {
                using var client = new HttpClient(handler);
                var chunkCache = new ChunkCache(Path.Combine(root, "chunks"), 1024 * 1024);
                var packCache = new PackCache(Path.Combine(root, "packs"), 1024 * 1024);
                await chunkCache.InitializeAsync();
                using var manager = new DownloadManager(client, new LauncherApiClient(client, new Uri("http://launcher/")), chunkCache, 2, packCache: packCache, packDownloadEnabled: true);

                var summary = await manager.DownloadAsync(BuildManifest(scenarioRaw, scenarioEncoded, scenarioEncodedHash), jobId);

                Assert.Equal(scenarioPackLength, summary.PhysicalPackNetworkBytes);
                assertHandler(handler);
            }
            finally
            {
                if (Directory.Exists(root)) Directory.Delete(root, true);
            }
        }
    }

    private sealed class DirectPackHandler(
        byte[] pack,
        string packHash,
        string encodedHash,
        bool failFirstPackSource = false,
        bool expiredFirstPackResolution = false,
        bool corruptPack = false,
        bool corruptFirstPackSource = false) : HttpMessageHandler
    {
        public int PackGets { get; private set; }
        public int ChunkGets { get; private set; }
        public int PackResolutionCalls { get; private set; }
        public List<string> RequestedPaths { get; } = [];
        public List<long> PackRanges { get; } = [];
        public List<string> PackHosts { get; } = [];

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var path = request.RequestUri!.AbsolutePath;
            RequestedPaths.Add(path);
            if (request.Method == HttpMethod.Post && path.Contains("/packs/resolve", StringComparison.Ordinal))
            {
                PackResolutionCalls++;
                var hosts = expiredFirstPackResolution && PackResolutionCalls == 1
                    ? new[] { "expired" }
                    : failFirstPackSource || corruptFirstPackSource
                        ? new[] { "bad", "hot" }
                        : new[] { "hot" };
                var sources = hosts.Select((host, index) => new
                {
                    provider = host,
                    pool_id = host,
                    provider_type = "local",
                    failure_domain = host,
                    url = $"http://{host}/packs/{packHash}",
                    expires_at = (DateTimeOffset?)null,
                    range_supported = true,
                    stable_url = true,
                    priority = index
                });
                return Task.FromResult(Json(new[] { new { pack_hash = packHash, encoded_size = pack.Length, chunk_hashes = new[] { encodedHash }, sources } }));
            }
            if (request.Method == HttpMethod.Post)
            {
                return Task.FromResult(Json(new[] { new { encoded_hash = encodedHash, urls = new[] { $"http://hot/objects/{encodedHash}" }, expires_at = (DateTimeOffset?)null } }));
            }
            if (path == $"/packs/{packHash}")
            {
                PackGets++;
                var host = request.RequestUri!.Host;
                PackHosts.Add(host);
                if (host == "expired") return Task.FromResult(new HttpResponseMessage(HttpStatusCode.Gone));
                if (host == "bad") return Task.FromResult(new HttpResponseMessage(HttpStatusCode.InternalServerError));
                var range = request.Headers.Range?.Ranges.SingleOrDefault();
                if (range?.From is long from)
                {
                    var to = Math.Min(range.To ?? (pack.LongLength - 1), pack.LongLength - 1);
                    var payload = pack[(int)from..(int)(to + 1)];
                    if ((corruptPack || (corruptFirstPackSource && host == "bad")) && payload.Length > 0) payload[0] ^= 0xFF;
                    PackRanges.Add(from);
                    var response = new HttpResponseMessage(HttpStatusCode.PartialContent)
                    {
                        Content = new ByteArrayContent(payload)
                    };
                    response.Content.Headers.ContentRange = new ContentRangeHeaderValue(from, to, pack.LongLength);
                    return Task.FromResult(response);
                }
                var fullPayload = pack.ToArray();
                if ((corruptPack || (corruptFirstPackSource && host == "bad")) && fullPayload.Length > 0) fullPayload[0] ^= 0xFF;
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(fullPayload) });
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

    private sealed class SparsePackHandler(
        byte[] pack,
        string packHash,
        string cachedHash,
        string missingHash) : HttpMessageHandler
    {
        public int PackGets { get; private set; }
        public int SparseRelayGets { get; private set; }
        public string PackResolutionRequest { get; private set; } = string.Empty;

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var path = request.RequestUri!.AbsolutePath;
            if (request.Method == HttpMethod.Post && path.Contains("/packs/resolve", StringComparison.Ordinal))
            {
                PackResolutionRequest = await request.Content!.ReadAsStringAsync(cancellationToken);
                return Json(new[]
                {
                    new
                    {
                        pack_hash = packHash,
                        encoded_size = pack.Length,
                        chunk_hashes = new[] { cachedHash, missingHash },
                        sources = new[]
                        {
                            new
                            {
                                provider = "filemirage",
                                pool_id = "filemirage",
                                provider_type = "filemirage",
                                failure_domain = "filemirage",
                                url = $"http://filemirage/packs/{packHash}",
                                expires_at = (DateTimeOffset?)null,
                                range_supported = true,
                                stable_url = false,
                                priority = 0
                            }
                        }
                    }
                });
            }
            if (request.Method == HttpMethod.Post && path.EndsWith("/resolve", StringComparison.Ordinal))
            {
                return Json(new[]
                {
                    new
                    {
                        encoded_hash = missingHash,
                        urls = new[] { $"http://launcher/api/v1/builds/build/chunks/{missingHash}" },
                        expires_at = (DateTimeOffset?)null
                    }
                });
            }
            if (path == $"/packs/{packHash}")
            {
                PackGets++;
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(pack) };
            }
            if (path == $"/api/v1/builds/build/chunks/{missingHash}")
            {
                SparseRelayGets++;
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(ExtractEncodedChunk(pack, missingHash)) };
            }
            return new HttpResponseMessage(HttpStatusCode.NotFound);
        }

        private static byte[] ExtractEncodedChunk(byte[] bytes, string encodedHash)
        {
            var reader = PhysicalPackReader.Parse(bytes);
            return reader.ReadEncoded(encodedHash);
        }

        private static HttpResponseMessage Json<T>(T value) => new(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(value), Encoding.UTF8, "application/json") };
    }

    private static byte[] BuildPack(byte[] raw, byte[] encoded) => BuildPack((raw, encoded));

    private static byte[] BuildPack(params (byte[] Raw, byte[] Encoded)[] chunks)
    {
        const int headerSize = 64;
        const int entrySize = 96;
        const int footerSize = 72;
        var entries = chunks
            .Select(item => new
            {
                item.Raw,
                item.Encoded,
                EncodedHash = Hashing.ComputeHash(item.Encoded)
            })
            .OrderBy(item => item.EncodedHash, StringComparer.Ordinal)
            .ToArray();
        var indexOffset = headerSize + entries.Sum(item => item.Encoded.Length);
        var bytes = new byte[indexOffset + entrySize * entries.Length + footerSize];
        Encoding.ASCII.GetBytes("LGRPACK1").CopyTo(bytes, 0);
        BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(8, 2), 1);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(12, 4), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(16, 8), (ulong)entries.Length);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(24, 8), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(32, 8), (ulong)indexOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(40, 8), (ulong)(entrySize * entries.Length));
        var dataOffset = headerSize;
        var entryOffset = indexOffset;
        foreach (var entry in entries)
        {
            entry.Encoded.CopyTo(bytes, dataOffset);
            Convert.FromHexString(entry.EncodedHash).CopyTo(bytes, entryOffset);
            Convert.FromHexString(Hashing.ComputeHash(entry.Raw)).CopyTo(bytes, entryOffset + 32);
            BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(entryOffset + 64, 8), (ulong)dataOffset);
            BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(entryOffset + 72, 8), (ulong)entry.Encoded.Length);
            BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(entryOffset + 80, 8), (ulong)entry.Raw.Length);
            BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(entryOffset + 88, 4), 1);
            dataOffset += entry.Encoded.Length;
            entryOffset += entrySize;
        }
        var footerOffset = indexOffset + entrySize * entries.Length;
        Encoding.ASCII.GetBytes("LGRPFTR1").CopyTo(bytes, footerOffset);
        BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(footerOffset + 8, 2), 1);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 12, 8), (ulong)indexOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 20, 8), (ulong)(entrySize * entries.Length));
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 28, 8), (ulong)entries.Length);
        Convert.FromHexString(Hashing.ComputeHash(bytes.AsSpan(indexOffset, entrySize * entries.Length))).CopyTo(bytes, footerOffset + 40);
        return bytes;
    }

    private static Manifest BuildManifest(byte[] raw, byte[] encoded, string encodedHash)
    {
        var chunk = new ChunkReference(Hashing.ComputeHash(raw), raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin");
        return new Manifest(1, "manifest", "game", "build", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("game.exe", raw.Length, Hashing.ComputeHash(raw), [chunk])], new LaunchProfile("game.exe", ".", [], new Dictionary<string, string>()));
    }
}
