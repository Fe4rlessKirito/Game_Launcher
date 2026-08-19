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

public class DownloadTests
{
    [Fact]
    public void ZstdRoundTripPreservesBytes()
    {
        var source = Enumerable.Range(0, 1024).Select(index => (byte)(index % 251)).ToArray();
        using var compressor = new ZstdSharp.Compressor(3);
        var encoded = compressor.Wrap(source);
        Assert.Equal(source, ZstdCodec.Decompress(encoded));
    }

    [Fact]
    public async Task SuccessfulDownloadVerifiesAndCachesEncodedChunk()
    {
        await using var fixture = await DownloadFixture.CreateAsync();
        var summary = await fixture.Manager.DownloadAsync(fixture.Manifest, "job-success");
        Assert.Equal(1, summary.ChunksDownloaded);
        Assert.Equal(fixture.Encoded.Length, summary.NetworkBytes);
        Assert.Equal(fixture.Encoded, await fixture.Cache.ReadAsync(fixture.EncodedHash));
    }

    [Fact]
    public async Task HtmlProviderRedirectsAreFollowedBeforeValidatingChunk()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-download-redirect-" + Guid.NewGuid().ToString("N"));
        try
        {
            var raw = Encoding.UTF8.GetBytes("provider redirect fixture");
            using var compressor = new ZstdSharp.Compressor(3);
            var encoded = compressor.Wrap(raw).ToArray();
            var encodedHash = Hashing.ComputeHash(encoded);
            var chunk = new ChunkReference(Hashing.ComputeHash(raw), raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin");
            var manifest = new Manifest(1, "redirect-manifest", "redirect-game", "redirect-build", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("game.exe", raw.Length, Hashing.ComputeHash(raw), [chunk])], new LaunchProfile("game.exe", ".", [], new Dictionary<string, string>()));
            var cache = new ChunkCache(Path.Combine(root, "cache"), 4 * 1024 * 1024);
            await cache.InitializeAsync();
            var handler = new HtmlRedirectHandler(encoded);
            using var client = new HttpClient(handler);
            using var manager = new DownloadManager(client, new LauncherApiClient(client, new Uri("http://launcher/")), cache, 2);

            var summary = await manager.DownloadAsync(manifest, "redirect-job");

            Assert.Equal(1, summary.ChunksDownloaded);
            Assert.Equal(encoded, await cache.ReadAsync(encodedHash));
            Assert.Equal(3, handler.GetRequests);
        }
        finally
        {
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task MirrorFailureFallsBackAndCorruptResponseIsRejected()
    {
        await using var fixture = await DownloadFixture.CreateAsync();
        fixture.Handler.FailStatus["http://mirror-a/objects/"] = HttpStatusCode.InternalServerError;
        var summary = await fixture.Manager.DownloadAsync(fixture.Manifest, "job-mirror");
        Assert.Equal(1, summary.ChunksDownloaded);
        Assert.Contains(fixture.Handler.RequestedUrls, url => url.StartsWith("http://mirror-a", StringComparison.Ordinal));
        Assert.Contains(fixture.Handler.RequestedUrls, url => url.StartsWith("http://mirror-b", StringComparison.Ordinal));

        await using var corrupt = await DownloadFixture.CreateAsync(corruptResponse: true);
        await Assert.ThrowsAsync<LauncherOperationException>(() => corrupt.Manager.DownloadAsync(corrupt.Manifest, "job-corrupt"));
        Assert.False(File.Exists(corrupt.Cache.GetPath(corrupt.EncodedHash)));
    }

    [Fact]
    public async Task HttpFailuresTimeoutsAndExpiredUrlsRecoverThroughResolver()
    {
        await using var notFound = await DownloadFixture.CreateAsync();
        notFound.Handler.FailStatus["http://mirror-a/objects/"] = HttpStatusCode.NotFound;
        var notFoundSummary = await notFound.Manager.DownloadAsync(notFound.Manifest, "job-404");
        Assert.Equal(1, notFoundSummary.ChunksDownloaded);

        await using var rateLimited = await DownloadFixture.CreateAsync(rateLimitOnce: true);
        var rateLimitedSummary = await rateLimited.Manager.DownloadAsync(rateLimited.Manifest, "job-429");
        Assert.Equal(1, rateLimitedSummary.ChunksDownloaded);

        await using var timedOut = await DownloadFixture.CreateAsync(timeoutOnce: true);
        var timeoutSummary = await timedOut.Manager.DownloadAsync(timedOut.Manifest, "job-timeout");
        Assert.Equal(1, timeoutSummary.ChunksDownloaded);

        await using var reset = await DownloadFixture.CreateAsync(connectionResetOnce: true);
        var resetSummary = await reset.Manager.DownloadAsync(reset.Manifest, "job-reset");
        Assert.Equal(1, resetSummary.ChunksDownloaded);

        await using var expired = await DownloadFixture.CreateAsync(expiredFirstResolution: true);
        var expiredSummary = await expired.Manager.DownloadAsync(expired.Manifest, "job-expired-url");
        Assert.Equal(1, expiredSummary.ChunksDownloaded);
        Assert.True(expired.Handler.ResolveCalls >= 2);

        await using var unavailable = await DownloadFixture.CreateAsync();
        unavailable.Handler.FailStatus["http://mirror-a/objects/"] = HttpStatusCode.ServiceUnavailable;
        unavailable.Handler.FailStatus["http://mirror-b/objects/"] = HttpStatusCode.ServiceUnavailable;
        await Assert.ThrowsAsync<LauncherOperationException>(() => unavailable.Manager.DownloadAsync(unavailable.Manifest, "job-unavailable"));
    }

    [Fact]
    public async Task ExistingPartialFileUsesRangeAndServerIgnoringRangeRestartsSafely()
    {
        await using var fixture = await DownloadFixture.CreateAsync();
        var partialLength = fixture.Encoded.Length / 2;
        await File.WriteAllBytesAsync(fixture.Cache.GetPartialPath(fixture.EncodedHash), fixture.Encoded[..partialLength]);
        await fixture.Manager.DownloadAsync(fixture.Manifest, "job-range");
        Assert.Contains(fixture.Handler.Ranges, range => range == partialLength);

        await using var ignored = await DownloadFixture.CreateAsync(ignoreRange: true);
        var ignoredLength = ignored.Encoded.Length / 2;
        await File.WriteAllBytesAsync(ignored.Cache.GetPartialPath(ignored.EncodedHash), ignored.Encoded[..ignoredLength]);
        await ignored.Manager.DownloadAsync(ignored.Manifest, "job-ignore-range");
        Assert.Equal(ignored.Encoded, await ignored.Cache.ReadAsync(ignored.EncodedHash));
    }

    [Fact]
    public async Task InjectedMidChunkDisconnectLeavesPartialAndResumes()
    {
        await using var fixture = await DownloadFixture.CreateAsync(injectAtThirtyPercent: true);
        var summary = await fixture.Manager.DownloadAsync(fixture.Manifest, "job-mid-chunk-failure");
        Assert.Equal(1, summary.ChunksDownloaded);
        Assert.Contains(fixture.Handler.Ranges, range => range > 0);
        Assert.Equal(fixture.Encoded, await fixture.Cache.ReadAsync(fixture.EncodedHash));
    }

    [Fact]
    public async Task PauseResumeCancellationAndJobPersistenceAreObservable()
    {
        await using var fixture = await DownloadFixture.CreateAsync(slowResponse: true, withStateStore: true);
        fixture.Manager.Pause();
        var pending = fixture.Manager.DownloadAsync(fixture.Manifest, "job-pause");
        await Task.Delay(100);
        Assert.False(pending.IsCompleted);
        fixture.Manager.Resume();
        await pending;
        Assert.Equal(DownloadJobState.Ready, (await fixture.StateStore!.GetDownloadJobAsync("job-pause"))!.State);

        await using var cancelled = await DownloadFixture.CreateAsync(slowResponse: true, withStateStore: true);
        using var cancellation = new CancellationTokenSource(50);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => cancelled.Manager.DownloadAsync(cancelled.Manifest, "job-cancel", cancellationToken: cancellation.Token));
        Assert.Equal(DownloadJobState.Cancelled, (await cancelled.StateStore!.GetDownloadJobAsync("job-cancel"))!.State);
    }

    [Fact]
    public async Task HundredsOfChunksCompleteWithBoundedSchedulerConcurrency()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-download-stress-" + Guid.NewGuid().ToString("N"));
        var cache = new ChunkCache(Path.Combine(root, "cache"), 64 * 1024 * 1024);
        await cache.InitializeAsync();
        var rawParts = new List<byte[]>();
        var chunks = new List<ChunkReference>();
        var encodedByHash = new Dictionary<string, byte[]>(StringComparer.Ordinal);
        for (var index = 0; index < 256; index++)
        {
            var raw = Enumerable.Range(0, 32 * 1024).Select(value => (byte)((value + index * 17) % 251)).ToArray();
            BitConverter.GetBytes(index).CopyTo(raw, 0);
            rawParts.Add(raw);
            using var compressor = new ZstdSharp.Compressor(3);
            var encoded = compressor.Wrap(raw).ToArray();
            var encodedHash = Hashing.ComputeHash(encoded);
            encodedByHash[encodedHash] = encoded;
            chunks.Add(new ChunkReference(Hashing.ComputeHash(raw), raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin"));
        }
        var rawFile = rawParts.SelectMany(part => part).ToArray();
        var manifest = new Manifest(1, "stress-manifest", "stress-game", "stress-build", "stress", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("stress.bin", rawFile.Length, Hashing.ComputeHash(rawFile), chunks)], new LaunchProfile("stress.bin", ".", [], new Dictionary<string, string>()));
        var handler = new MultiChunkHandler(encodedByHash);
        using var httpClient = new HttpClient(handler);
        var manager = new DownloadManager(httpClient, new LauncherApiClient(httpClient, new Uri("http://launcher/")), cache, 8);
        try
        {
            var summary = await manager.DownloadAsync(manifest, "job-stress");
            Assert.Equal(256, summary.ChunksDownloaded);
            Assert.Equal(0, summary.ChunksReused);
            Assert.True(handler.MaxActiveRequests > 1);
            Assert.InRange(handler.MaxActiveRequests, 1, 8);
            Assert.Equal(256, handler.GetRequests);
        }
        finally
        {
            manager.Dispose();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    private sealed class DownloadFixture : IAsyncDisposable
    {
        public required FixtureHandler Handler { get; init; }
        public required HttpClient HttpClient { get; init; }
        public required ChunkCache Cache { get; init; }
        public required DownloadManager Manager { get; init; }
        public required Manifest Manifest { get; init; }
        public required byte[] Encoded { get; init; }
        public required string EncodedHash { get; init; }
        public LocalStateStore? StateStore { get; init; }
        public string Root { get; init; } = "";

        public static async Task<DownloadFixture> CreateAsync(
            bool corruptResponse = false,
            bool ignoreRange = false,
            bool slowResponse = false,
            bool withStateStore = false,
            bool rateLimitOnce = false,
            bool timeoutOnce = false,
            bool connectionResetOnce = false,
            bool expiredFirstResolution = false,
            DownloadFailureInjection? failureInjection = null,
            bool injectAtThirtyPercent = false)
        {
            var root = Path.Combine(Path.GetTempPath(), "launcher-download-" + Guid.NewGuid().ToString("N"));
            var cache = new ChunkCache(Path.Combine(root, "cache"), 32 * 1024 * 1024);
            await cache.InitializeAsync();
            var raw = Encoding.UTF8.GetBytes(string.Concat(Enumerable.Repeat("download-fixture-", 1024)));
            using var compressor = new ZstdSharp.Compressor(3);
            var encoded = compressor.Wrap(raw).ToArray();
            if (injectAtThirtyPercent) failureInjection = new DownloadFailureInjection(Math.Max(1, encoded.Length / 3));
            var encodedHash = Hashing.ComputeHash(encoded);
            var rawHash = Hashing.ComputeHash(raw);
            var chunk = new ChunkReference(rawHash, raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin");
            var manifest = new Manifest(1, "manifest", "synthetic-game", "build", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, [new FileRecipe("SyntheticGame.exe", raw.Length, Hashing.ComputeHash(raw), [chunk])], new LaunchProfile("SyntheticGame.exe", ".", [], new Dictionary<string, string>()));
            var handler = new FixtureHandler(encoded, encodedHash, corruptResponse, ignoreRange, slowResponse, rateLimitOnce, timeoutOnce, connectionResetOnce, expiredFirstResolution);
            var client = new HttpClient(handler) { Timeout = timeoutOnce ? TimeSpan.FromMilliseconds(250) : TimeSpan.FromSeconds(2) };
            var api = new LauncherApiClient(client, new Uri("http://launcher/"));
            LocalStateStore? state = null;
            if (withStateStore)
            {
                state = new LocalStateStore(Path.Combine(root, "launcher.db"));
                await state.InitializeAsync();
            }
            return new DownloadFixture { Handler = handler, HttpClient = client, Cache = cache, Manager = new DownloadManager(client, api, cache, 2, state, failureInjection), Manifest = manifest, Encoded = encoded, EncodedHash = encodedHash, StateStore = state, Root = root };
        }

        public ValueTask DisposeAsync()
        {
            Manager.Dispose();
            HttpClient.Dispose();
            Microsoft.Data.Sqlite.SqliteConnection.ClearAllPools();
            if (Directory.Exists(Root)) Directory.Delete(Root, true);
            return ValueTask.CompletedTask;
        }
    }

    private sealed class MultiChunkHandler(IReadOnlyDictionary<string, byte[]> encodedByHash) : HttpMessageHandler
    {
        private int _activeRequests;
        private int _getRequests;
        private int _maxActiveRequests;

        public int GetRequests => _getRequests;
        public int MaxActiveRequests => _maxActiveRequests;

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (request.Method == HttpMethod.Post)
            {
                var locations = encodedByHash.Select(pair => new
                {
                    encoded_hash = pair.Key,
                    urls = new[] { $"http://stress/objects/{pair.Key}" },
                    expires_at = (DateTimeOffset?)null
                });
                return new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent(JsonSerializer.Serialize(locations), Encoding.UTF8, "application/json")
                };
            }

            Interlocked.Increment(ref _getRequests);
            var active = Interlocked.Increment(ref _activeRequests);
            while (active > Volatile.Read(ref _maxActiveRequests))
            {
                Interlocked.CompareExchange(ref _maxActiveRequests, active, Volatile.Read(ref _maxActiveRequests));
            }
            try
            {
                await Task.Delay(2, cancellationToken);
                var hash = request.RequestUri!.Segments[^1];
                return encodedByHash.TryGetValue(hash, out var encoded)
                    ? new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(encoded) }
                    : new HttpResponseMessage(HttpStatusCode.NotFound);
            }
            finally
            {
                Interlocked.Decrement(ref _activeRequests);
            }
        }
    }

    private sealed class HtmlRedirectHandler(byte[] encoded) : HttpMessageHandler
    {
        private static readonly string[] InitialUrls = ["http://filemirage.test/file/direct/landing"];

        public int GetRequests { get; private set; }

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (request.Method == HttpMethod.Post)
            {
                var body = JsonSerializer.Serialize(new[]
                {
                    new
                    {
                        encoded_hash = Hashing.ComputeHash(encoded),
                        urls = InitialUrls,
                        expires_at = (DateTimeOffset?)null
                    }
                });
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent(body, Encoding.UTF8, "application/json")
                });
            }

            GetRequests++;
            return request.RequestUri!.AbsolutePath switch
            {
                "/file/direct/landing" => Task.FromResult(Html("http://filemirage.test/file/direct/second")),
                "/file/direct/second" => Task.FromResult(Html("http://filemirage.test/file/direct/bytes")),
                "/file/direct/bytes" => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new ByteArrayContent(encoded)
                }),
                _ => Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound))
            };
        }

        private static HttpResponseMessage Html(string redirectUrl)
        {
            return new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent($"<script>window .location.href = \"{redirectUrl}\"</script>", Encoding.UTF8, "text/html")
            };
        }
    }

    private sealed class FixtureHandler(
        byte[] encoded,
        string encodedHash,
        bool corruptResponse,
        bool ignoreRange,
        bool slowResponse,
        bool rateLimitOnce,
        bool timeoutOnce,
        bool connectionResetOnce,
        bool expiredFirstResolution) : HttpMessageHandler
    {
        public Dictionary<string, HttpStatusCode> FailStatus { get; } = new(StringComparer.Ordinal);
        public List<string> RequestedUrls { get; } = [];
        public List<long> Ranges { get; } = [];
        public int ResolveCalls { get; private set; }
        private int _rateLimitSent;
        private int _timeoutSent;
        private int _connectionResetSent;

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (request.Method == HttpMethod.Post)
            {
                ResolveCalls++;
                var urls = expiredFirstResolution && ResolveCalls == 1
                    ? new[] { $"http://expired/objects/{encodedHash}" }
                    : new[] { $"http://mirror-a/objects/{encodedHash}", $"http://mirror-b/objects/{encodedHash}" };
                var body = JsonSerializer.Serialize(new[] { new { encoded_hash = encodedHash, urls, expires_at = (DateTimeOffset?)null } });
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(body, Encoding.UTF8, "application/json") };
            }
            var url = request.RequestUri!.ToString();
            RequestedUrls.Add(url);
            if (url.StartsWith("http://expired/", StringComparison.Ordinal)) return new HttpResponseMessage(HttpStatusCode.Gone);
            var failing = FailStatus.FirstOrDefault(pair => url.StartsWith(pair.Key, StringComparison.Ordinal));
            if (!string.IsNullOrEmpty(failing.Key)) return new HttpResponseMessage(failing.Value);
            if (timeoutOnce && Interlocked.Exchange(ref _timeoutSent, 1) == 0) throw new TaskCanceledException("fixture timeout");
            if (connectionResetOnce && Interlocked.Exchange(ref _connectionResetSent, 1) == 0) throw new HttpRequestException("fixture connection reset");
            if (rateLimitOnce && Interlocked.Exchange(ref _rateLimitSent, 1) == 0)
            {
                var rateLimited = new HttpResponseMessage(HttpStatusCode.TooManyRequests);
                rateLimited.Headers.RetryAfter = new RetryConditionHeaderValue(TimeSpan.Zero);
                return rateLimited;
            }
            var range = request.Headers.Range?.Ranges.SingleOrDefault()?.From;
            if (range.HasValue) Ranges.Add(range.Value);
            var start = range.GetValueOrDefault();
            var payload = encoded;
            var status = HttpStatusCode.OK;
            if (range.HasValue && !ignoreRange)
            {
                status = HttpStatusCode.PartialContent;
                payload = encoded[(int)start..];
            }
            if (corruptResponse) payload = payload.ToArray();
            if (corruptResponse && payload.Length > 0) payload[0] ^= 0xFF;
            if (slowResponse)
            {
                await Task.Delay(100, cancellationToken);
            }
            var response = new HttpResponseMessage(status) { Content = new ByteArrayContent(payload) };
            response.Content.Headers.ContentLength = payload.Length;
            if (status == HttpStatusCode.PartialContent) response.Content.Headers.ContentRange = new ContentRangeHeaderValue(start, encoded.Length - 1, encoded.Length);
            return response;
        }
    }
}
