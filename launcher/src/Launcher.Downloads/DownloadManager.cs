using System.Diagnostics;
using Launcher.Core;
using Launcher.Manifests;
using Launcher.Networking;
using Launcher.Security;
using Launcher.Storage;

namespace Launcher.Downloads;

public sealed class DownloadManager(
    HttpClient httpClient,
    LauncherApiClient apiClient,
    ChunkCache cache,
    int maxConcurrency = 4) : IDisposable
{
    private readonly SemaphoreSlim _pauseGate = new(1, 1);
    private readonly SemaphoreSlim _concurrency = new(Math.Clamp(maxConcurrency, 1, 32));

    public void Pause() { _pauseGate.Wait(0); }
    public void Resume() { if (_pauseGate.CurrentCount == 0) _pauseGate.Release(); }

    public void Dispose()
    {
        _concurrency.Dispose();
        _pauseGate.Dispose();
    }

    public async Task DownloadAsync(Manifest manifest, string jobId, IProgress<DownloadProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(manifest);
        var uniqueChunks = manifest.Files.SelectMany(file => file.Chunks).GroupBy(chunk => chunk.EncodedHash, StringComparer.Ordinal).Select(group => group.First()).ToArray();
        var totalBytes = uniqueChunks.Sum(chunk => chunk.EncodedSize);
        var downloadedBytes = 0L;
        var stopwatch = Stopwatch.StartNew();
        var resolved = await apiClient.ResolveChunksAsync(manifest.BuildId, uniqueChunks.Select(chunk => chunk.EncodedHash).ToArray(), cancellationToken).ConfigureAwait(false);
        progress?.Report(new DownloadProgress(jobId, DownloadJobState.Resolving, 0, totalBytes, 0, null));

        var tasks = uniqueChunks.Select(async chunk =>
        {
            var cached = await cache.ReadAsync(chunk.EncodedHash, cancellationToken).ConfigureAwait(false);
            if (cached is not null)
            {
                Interlocked.Add(ref downloadedBytes, chunk.EncodedSize);
                Report(DownloadJobState.Ready, chunk.EncodedHash);
                return;
            }
            if (!resolved.TryGetValue(chunk.EncodedHash, out var location) || location.Urls.Count == 0) throw new LauncherOperationException($"No storage locations for chunk {chunk.EncodedHash}.");
            await _concurrency.WaitAsync(cancellationToken).ConfigureAwait(false);
            try
            {
                await DownloadChunkWithRetryAsync(chunk, location.Urls, cancellationToken).ConfigureAwait(false);
                Interlocked.Add(ref downloadedBytes, chunk.EncodedSize);
                Report(DownloadJobState.Ready, chunk.EncodedHash);
            }
            finally { _concurrency.Release(); }
        });
        await Task.WhenAll(tasks).ConfigureAwait(false);
        progress?.Report(new DownloadProgress(jobId, DownloadJobState.Ready, downloadedBytes, totalBytes, Rate(), TimeSpan.Zero));

        void Report(DownloadJobState state, string hash)
        {
            var current = Interlocked.Read(ref downloadedBytes);
            var rate = Rate();
            progress?.Report(new DownloadProgress(jobId, state, current, totalBytes, rate, rate > 0 ? TimeSpan.FromSeconds(Math.Max(0, totalBytes - current) / rate) : null, hash));
        }

        double Rate() => stopwatch.Elapsed.TotalSeconds <= 0 ? 0 : downloadedBytes / stopwatch.Elapsed.TotalSeconds;
    }

    private async Task DownloadChunkWithRetryAsync(ChunkReference chunk, IReadOnlyList<string> urls, CancellationToken cancellationToken)
    {
        Exception? lastError = null;
        for (var attempt = 0; attempt < 4; attempt++)
        {
            await _pauseGate.WaitAsync(cancellationToken).ConfigureAwait(false);
            _pauseGate.Release();
            foreach (var url in urls)
            {
                try
                {
                    using var request = new HttpRequestMessage(HttpMethod.Get, url);
                    using var response = await httpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
                    response.EnsureSuccessStatusCode();
                    await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
                    await using var memory = new MemoryStream(capacity: checked((int)Math.Min(chunk.EncodedSize, int.MaxValue)));
                    await stream.CopyToAsync(memory, cancellationToken).ConfigureAwait(false);
                    var encoded = memory.ToArray();
                    if (encoded.LongLength != chunk.EncodedSize || !string.Equals(Hashing.ComputeHash(encoded), chunk.EncodedHash, StringComparison.Ordinal)) throw new InvalidDataException($"Encoded chunk verification failed for {chunk.EncodedHash}.");
                    await cache.PutAsync(chunk.EncodedHash, encoded, cancellationToken).ConfigureAwait(false);
                    return;
                }
                catch (Exception error) when (error is HttpRequestException or IOException or InvalidDataException)
                {
                    lastError = error;
                }
            }
            if (attempt < 3) await Task.Delay(TimeSpan.FromMilliseconds(250 * Math.Pow(2, attempt)), cancellationToken).ConfigureAwait(false);
        }
        throw new LauncherOperationException($"Chunk download failed after retries: {chunk.EncodedHash}", lastError);
    }
}
