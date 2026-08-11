using Launcher.Networking;

namespace Launcher.Downloads;

/// <summary>
/// Small adaptive source scorer. It keeps concurrency bounded by the download
/// manager while preferring sources that have recently completed quickly and
/// successfully. Provider identity, not URL query strings, is used as the
/// stable key so presigned URL refreshes do not reset the estimate.
/// </summary>
public sealed class HotSourceScheduler
{
    private readonly object gate = new();
    private readonly Dictionary<string, SourceStats> stats = new(StringComparer.Ordinal);

    public IReadOnlyList<ResolvedPackSource> Rank(IEnumerable<ResolvedPackSource> sources)
    {
        lock (gate)
        {
            return sources
                .OrderBy(source => source.Priority)
                .ThenByDescending(source => Score(source))
                .ThenBy(source => source.Provider, StringComparer.Ordinal)
                .ThenBy(source => source.Url, StringComparer.Ordinal)
                .ToArray();
        }
    }

    public void Report(ResolvedPackSource source, bool success, TimeSpan elapsed, long bytes)
    {
        lock (gate)
        {
            if (!stats.TryGetValue(source.Provider, out var value)) value = new SourceStats();
            value.Attempts++;
            value.Successes += success ? 1 : 0;
            value.LatencyMs = Ewma(value.LatencyMs, Math.Max(1, elapsed.TotalMilliseconds));
            if (success && bytes > 0) value.BytesPerSecond = Ewma(value.BytesPerSecond, bytes / Math.Max(0.001, elapsed.TotalSeconds));
            stats[source.Provider] = value;
        }
    }

    public IReadOnlyDictionary<string, (int Attempts, int Successes, double BytesPerSecond)> Snapshot()
    {
        lock (gate) return stats.ToDictionary(pair => pair.Key, pair => (pair.Value.Attempts, pair.Value.Successes, pair.Value.BytesPerSecond), StringComparer.Ordinal);
    }

    private double Score(ResolvedPackSource source)
    {
        if (!stats.TryGetValue(source.Provider, out var value)) return source.StableUrl ? 1 : 0;
        var successRate = value.Attempts == 0 ? 0 : (double)value.Successes / value.Attempts;
        return successRate * 100 + value.BytesPerSecond / 1024 / 1024 - value.LatencyMs / 1000;
    }

    private static double Ewma(double current, double next) => current <= 0 ? next : current * 0.7 + next * 0.3;
    private sealed class SourceStats { public int Attempts; public int Successes; public double LatencyMs; public double BytesPerSecond; }
}
