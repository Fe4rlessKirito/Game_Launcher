using System.Diagnostics;
using Launcher.Core;
using Launcher.Manifests;
using Launcher.Networking;
using Launcher.Security;
using Launcher.Storage;

namespace Launcher.Downloads;

public sealed record DownloadSummary(
    string JobId,
    long TotalEncodedBytes,
    long PreparedBytes,
    long NetworkBytes,
    long ReusedBytes,
    int ChunksDownloaded,
    int ChunksReused)
{
    public double NetworkSavings => TotalEncodedBytes <= 0 ? 0 : Math.Clamp((double)(TotalEncodedBytes - NetworkBytes) / TotalEncodedBytes, 0, 1);
}

public sealed class DownloadManager(
    HttpClient httpClient,
    LauncherApiClient apiClient,
    ChunkCache cache,
    int maxConcurrency = 4,
    LocalStateStore? stateStore = null,
    DownloadFailureInjection? failureInjection = null,
    PackCache? packCache = null) : IDisposable
{
    private readonly SemaphoreSlim _pauseGate = new(1, 1);
    private readonly SemaphoreSlim _concurrency = new(Math.Clamp(maxConcurrency, 1, 32));
    private readonly SemaphoreSlim _resolverGate = new(1, 1);
    private readonly HotSourceScheduler _sourceScheduler = new();
    private readonly HashSet<string> _packPreparedChunks = new(StringComparer.Ordinal);

    public void Pause() { _pauseGate.Wait(0); }
    public void Resume() { if (_pauseGate.CurrentCount == 0) _pauseGate.Release(); }

    public void Dispose()
    {
        _concurrency.Dispose();
        _pauseGate.Dispose();
        _resolverGate.Dispose();
    }

    public async Task<DownloadSummary> DownloadAsync(Manifest manifest, string jobId, IProgress<DownloadProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(manifest);
        var uniqueChunks = manifest.Files.SelectMany(file => file.Chunks).GroupBy(chunk => chunk.EncodedHash, StringComparer.Ordinal).Select(group => group.First()).ToArray();
        var totalBytes = uniqueChunks.Sum(chunk => chunk.EncodedSize);
        var preparedBytes = 0L;
        var networkBytes = 0L;
        var reusedBytes = 0L;
        var downloadedChunks = 0;
        var reusedChunks = 0;
        var stopwatch = Stopwatch.StartNew();
        await SaveJobAsync(new PersistedDownloadJob(jobId, manifest.BuildId, DownloadJobState.Resolving, 0, totalBytes, DateTimeOffset.UtcNow), cancellationToken).ConfigureAwait(false);
        progress?.Report(new DownloadProgress(jobId, DownloadJobState.Resolving, 0, totalBytes, 0, null));
        var resolved = await apiClient.ResolveChunksAsync(manifest.BuildId, uniqueChunks.Select(chunk => chunk.EncodedHash).ToArray(), cancellationToken).ConfigureAwait(false);
        if (packCache is not null && IsPackDownloadEnabled())
        {
            try
            {
                await packCache.InitializeAsync(cancellationToken).ConfigureAwait(false);
                var packBytes = await DownloadPacksAsync(manifest.BuildId, uniqueChunks, packCache, cancellationToken).ConfigureAwait(false);
                Interlocked.Add(ref networkBytes, packBytes);
            }
            catch (Exception error) when (error is HttpRequestException or InvalidDataException or IOException)
            {
                // Pack resolution is an acceleration path. A client can still
                // complete through the signed logical chunk resolver.
            }
        }

        var tasks = uniqueChunks.Select(async chunk =>
        {
            await _pauseGate.WaitAsync(cancellationToken).ConfigureAwait(false);
            _pauseGate.Release();
            var cached = await cache.ReadAsync(chunk.EncodedHash, cancellationToken).ConfigureAwait(false);
            if (cached is not null)
            {
                Interlocked.Add(ref preparedBytes, chunk.EncodedSize);
                if (_packPreparedChunks.Contains(chunk.EncodedHash))
                {
                    Interlocked.Increment(ref downloadedChunks);
                }
                else
                {
                    Interlocked.Add(ref reusedBytes, chunk.EncodedSize);
                    Interlocked.Increment(ref reusedChunks);
                }
                Report(DownloadJobState.Ready, chunk.EncodedHash);
                return;
            }
            if (!resolved.TryGetValue(chunk.EncodedHash, out var location) || location.Urls.Count == 0) throw new LauncherOperationException($"No storage locations for chunk {chunk.EncodedHash}.");
            await _concurrency.WaitAsync(cancellationToken).ConfigureAwait(false);
            try
            {
                var bytes = await DownloadChunkWithRetryAsync(chunk, location.Urls, async () =>
                {
                    await _resolverGate.WaitAsync(cancellationToken).ConfigureAwait(false);
                    try
                    {
                        var refreshed = await apiClient.ResolveChunksAsync(manifest.BuildId, new[] { chunk.EncodedHash }, cancellationToken).ConfigureAwait(false);
                        return refreshed.TryGetValue(chunk.EncodedHash, out var fresh) ? fresh.Urls : Array.Empty<string>();
                    }
                    finally { _resolverGate.Release(); }
                }, cancellationToken).ConfigureAwait(false);
                Interlocked.Add(ref preparedBytes, chunk.EncodedSize);
                Interlocked.Add(ref networkBytes, bytes);
                Interlocked.Increment(ref downloadedChunks);
                Report(DownloadJobState.Ready, chunk.EncodedHash);
            }
            finally { _concurrency.Release(); }
        });

        try
        {
            await Task.WhenAll(tasks).ConfigureAwait(false);
            var summary = new DownloadSummary(jobId, totalBytes, preparedBytes, networkBytes, reusedBytes, downloadedChunks, reusedChunks);
            await SaveJobAsync(new PersistedDownloadJob(jobId, manifest.BuildId, DownloadJobState.Ready, preparedBytes, totalBytes, DateTimeOffset.UtcNow), CancellationToken.None).ConfigureAwait(false);
            progress?.Report(new DownloadProgress(jobId, DownloadJobState.Ready, preparedBytes, totalBytes, Rate(), TimeSpan.Zero));
            return summary;
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            await SaveJobAsync(new PersistedDownloadJob(jobId, manifest.BuildId, DownloadJobState.Cancelled, preparedBytes, totalBytes, DateTimeOffset.UtcNow), CancellationToken.None).ConfigureAwait(false);
            throw;
        }
        catch (Exception error)
        {
            await SaveJobAsync(new PersistedDownloadJob(jobId, manifest.BuildId, DownloadJobState.Failed, preparedBytes, totalBytes, DateTimeOffset.UtcNow, error.Message), CancellationToken.None).ConfigureAwait(false);
            throw;
        }

        void Report(DownloadJobState state, string hash)
        {
            var current = Interlocked.Read(ref preparedBytes);
            var rate = Rate();
            progress?.Report(new DownloadProgress(jobId, state, current, totalBytes, rate, rate > 0 ? TimeSpan.FromSeconds(Math.Max(0, totalBytes - current) / rate) : null, hash));
            _ = SaveJobAsync(new PersistedDownloadJob(jobId, manifest.BuildId, state, current, totalBytes, DateTimeOffset.UtcNow), CancellationToken.None);
        }

        double Rate() => stopwatch.Elapsed.TotalSeconds <= 0 ? 0 : preparedBytes / stopwatch.Elapsed.TotalSeconds;
    }

    private async Task<long> DownloadChunkWithRetryAsync(ChunkReference chunk, IReadOnlyList<string> urls, Func<Task<IReadOnlyList<string>>> refreshUrls, CancellationToken cancellationToken)
    {
        Exception? lastError = null;
        var locations = urls.ToArray();
        for (var refreshAttempt = 0; refreshAttempt < 2; refreshAttempt++)
        {
            for (var attempt = 0; attempt < 4; attempt++)
            {
                foreach (var url in locations)
                {
                    try
                    {
                        var networkBytes = await DownloadOneUrlAsync(chunk, url, cancellationToken).ConfigureAwait(false);
                        return networkBytes;
                    }
                    catch (HttpRequestException error) { lastError = error; }
                    catch (IOException error) { lastError = error; }
                    catch (InvalidDataException error)
                    {
                        lastError = error;
                        TryDeletePartial(chunk.EncodedHash);
                    }
                    catch (OperationCanceledException error) when (!cancellationToken.IsCancellationRequested) { lastError = error; }
                }
                if (attempt < 3) await Task.Delay(TimeSpan.FromMilliseconds(100 * Math.Pow(2, attempt)), cancellationToken).ConfigureAwait(false);
            }
            locations = (await refreshUrls().ConfigureAwait(false)).ToArray();
            if (locations.Length == 0) break;
        }
        throw new LauncherOperationException($"Chunk download failed after retries: {chunk.EncodedHash}", lastError);
    }

    private async Task<long> DownloadOneUrlAsync(ChunkReference chunk, string url, CancellationToken cancellationToken)
    {
        var partial = cache.GetPartialPath(chunk.EncodedHash);
        var offset = File.Exists(partial) ? new FileInfo(partial).Length : 0;
        if (offset > chunk.EncodedSize) { TryDeletePartial(chunk.EncodedHash); offset = 0; }
        if (offset == chunk.EncodedSize)
        {
            try
            {
                await cache.PutFileAsync(chunk.EncodedHash, partial, cancellationToken).ConfigureAwait(false);
                return 0;
            }
            catch (InvalidDataException)
            {
                TryDeletePartial(chunk.EncodedHash);
                offset = 0;
            }
        }
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        if (offset > 0) request.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(offset, null);
        using var response = await httpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        if (response.StatusCode == System.Net.HttpStatusCode.NotFound || response.StatusCode == System.Net.HttpStatusCode.Gone) throw new HttpRequestException($"Chunk URL returned {(int)response.StatusCode}.");
        if (response.StatusCode == System.Net.HttpStatusCode.TooManyRequests)
        {
            var retryAfter = response.Headers.RetryAfter?.Delta ?? TimeSpan.FromMilliseconds(250);
            await Task.Delay(retryAfter, cancellationToken).ConfigureAwait(false);
            throw new HttpRequestException("Chunk URL rate limited.");
        }
        if ((int)response.StatusCode >= 500 || response.StatusCode == System.Net.HttpStatusCode.RequestTimeout) throw new HttpRequestException($"Chunk URL returned {(int)response.StatusCode}.");
        response.EnsureSuccessStatusCode();

        var append = offset > 0 && response.StatusCode == System.Net.HttpStatusCode.PartialContent && response.Content.Headers.ContentRange?.From == offset;
        if (offset > 0 && !append) { TryDeletePartial(chunk.EncodedHash); offset = 0; }
        Directory.CreateDirectory(Path.GetDirectoryName(partial)!);
        long received = 0;
        await using (var output = new FileStream(partial, append ? FileMode.Append : FileMode.Create, FileAccess.Write, FileShare.Read, 1024 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan))
        {
            await using var input = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
            var buffer = new byte[1024 * 1024];
            int read;
            while ((read = await input.ReadAsync(buffer.AsMemory(), cancellationToken).ConfigureAwait(false)) > 0)
            {
                var writable = read;
                var injectedFailure = failureInjection is not null && failureInjection.TryLimitWrite(received, read, out writable);
                await output.WriteAsync(buffer.AsMemory(0, writable), cancellationToken).ConfigureAwait(false);
                received += writable;
                if (injectedFailure) throw new IOException("Injected mid-chunk download interruption.");
            }
            await output.FlushAsync(cancellationToken).ConfigureAwait(false);
        }
        var finalLength = new FileInfo(partial).Length;
        if (finalLength != chunk.EncodedSize) throw new InvalidDataException($"Encoded chunk size verification failed for {chunk.EncodedHash}.");
        await cache.PutFileAsync(chunk.EncodedHash, partial, cancellationToken).ConfigureAwait(false);
        return received;
    }

    private void TryDeletePartial(string hash)
    {
        try
        {
            var path = cache.GetPartialPath(hash);
            if (File.Exists(path)) File.Delete(path);
        }
        catch (IOException) { }
    }

    private Task SaveJobAsync(PersistedDownloadJob job, CancellationToken cancellationToken) => stateStore is null ? Task.CompletedTask : stateStore.SaveDownloadJobAsync(job, cancellationToken);

    private async Task<long> DownloadPacksAsync(string buildId, IReadOnlyList<ChunkReference> chunks, PackCache physicalCache, CancellationToken cancellationToken)
    {
        var missing = chunks.Where(chunk => !File.Exists(physicalCache.GetPath(chunk.EncodedHash))).Select(chunk => chunk.EncodedHash).ToArray();
        if (missing.Length == 0) return 0;
        IReadOnlyList<ResolvedPack> packs;
        try { packs = await apiClient.ResolvePacksAsync(buildId, missing, cancellationToken).ConfigureAwait(false); }
        catch (HttpRequestException) { return 0; }
        var requested = missing.ToHashSet(StringComparer.Ordinal);
        long networkBytes = 0;
        foreach (var pack in packs.OrderBy(pack => pack.EncodedSize).ThenBy(pack => pack.PackHash, StringComparer.Ordinal))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var covered = pack.ChunkHashes.Where(requested.Contains).ToArray();
            if (covered.Length == 0 || pack.Sources.Count == 0) continue;
            var cached = await physicalCache.ReadAsync(pack.PackHash, cancellationToken).ConfigureAwait(false);
            var bytes = cached;
            if (bytes is null)
            {
                bytes = await DownloadPackWithRetryAsync(pack, physicalCache, cancellationToken).ConfigureAwait(false);
                networkBytes += bytes.Length;
            }
            var reader = PhysicalPackReader.Parse(bytes, pack.PackHash);
            foreach (var hash in covered)
            {
                if (!_packPreparedChunks.Add(hash)) continue;
                var encoded = reader.ReadEncoded(hash);
                var chunk = chunks.First(item => item.EncodedHash == hash);
                if (encoded.Length != chunk.EncodedSize) throw new InvalidDataException($"Pack chunk size mismatch for {hash}.");
                await cache.PutAsync(chunk.EncodedHash, encoded, cancellationToken).ConfigureAwait(false);
                requested.Remove(hash);
            }
            if (requested.Count == 0) break;
        }
        return networkBytes;
    }

    private async Task<byte[]> DownloadPackWithRetryAsync(ResolvedPack pack, PackCache cache, CancellationToken cancellationToken)
    {
        Exception? lastError = null;
        foreach (var source in _sourceScheduler.Rank(pack.Sources))
        {
            var started = Stopwatch.StartNew();
            try
            {
                using var request = new HttpRequestMessage(HttpMethod.Get, source.Url);
                using var response = await httpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
                if (response.StatusCode == System.Net.HttpStatusCode.NotFound || response.StatusCode == System.Net.HttpStatusCode.Gone) throw new HttpRequestException($"Pack URL returned {(int)response.StatusCode}.");
                if (response.StatusCode == System.Net.HttpStatusCode.TooManyRequests)
                {
                    var retryAfter = response.Headers.RetryAfter?.Delta ?? TimeSpan.FromMilliseconds(250);
                    await Task.Delay(retryAfter, cancellationToken).ConfigureAwait(false);
                    throw new HttpRequestException("Pack URL rate limited.");
                }
                if ((int)response.StatusCode >= 500 || response.StatusCode == System.Net.HttpStatusCode.RequestTimeout) throw new HttpRequestException($"Pack URL returned {(int)response.StatusCode}.");
                response.EnsureSuccessStatusCode();
                var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
                if (bytes.LongLength != pack.EncodedSize) throw new InvalidDataException($"Pack size mismatch for {pack.PackHash}.");
                await cache.PutAsync(pack.PackHash, bytes, cancellationToken).ConfigureAwait(false);
                _sourceScheduler.Report(source, true, started.Elapsed, bytes.Length);
                return bytes;
            }
            catch (Exception error) when (error is HttpRequestException or IOException or InvalidDataException)
            {
                lastError = error;
                _sourceScheduler.Report(source, false, started.Elapsed, 0);
            }
        }
        throw new HttpRequestException($"Physical pack download failed after source failover: {pack.PackHash}", lastError);
    }

    private static bool IsPackDownloadEnabled() => Environment.GetEnvironmentVariable("LAUNCHER_PACK_DOWNLOAD_ENABLED")?.Trim().ToLowerInvariant() is "1" or "true" or "yes" or "on";
}
