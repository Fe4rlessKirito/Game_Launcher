using System.Diagnostics;
using Launcher.Core;
using Launcher.Downloads;
using Launcher.Installation;
using Launcher.Manifests;
using Launcher.Networking;
using Launcher.Security;
using Launcher.Storage;

var mode = Get("LAUNCHER_E2E_MODE", "install");
var settingsPath = Environment.GetEnvironmentVariable("LAUNCHER_SETTINGS_PATH");
var settingsLoaded = !string.IsNullOrWhiteSpace(settingsPath) && File.Exists(settingsPath);
var settings = settingsLoaded
    ? await new JsonSettingsStore(settingsPath!).LoadAsync()
    : new LauncherSettings();
var apiBase = new Uri(Get("LAUNCHER_E2E_API", settings.ApiBaseUrl));
var stateRoot = Path.GetFullPath(Get("LAUNCHER_E2E_STATE_ROOT", Path.Combine(Path.GetTempPath(), "launcher-e2e-state")));
var installRoot = Path.GetFullPath(Get("LAUNCHER_E2E_INSTALL_ROOT", Path.Combine(stateRoot, "game")));
var sourceRoot = Path.GetFullPath(Get("LAUNCHER_E2E_SOURCE", Path.Combine(stateRoot, "source")));
var requestedBuild = Environment.GetEnvironmentVariable("LAUNCHER_E2E_BUILD_ID");
Directory.CreateDirectory(stateRoot);
var paths = LauncherPaths.FromRoot(stateRoot);
var cache = new ChunkCache(paths.CachePath, 2L * 1024 * 1024 * 1024);
await cache.InitializeAsync();
var state = new LocalStateStore(paths.DatabasePath);
await state.InitializeAsync();
using var httpClient = new HttpClient { Timeout = TimeSpan.FromSeconds(30) };
var api = new LauncherApiClient(httpClient, apiBase);
var games = await api.GetGamesAsync();
if (games.Count == 0) throw new InvalidOperationException("E2E API returned no published games.");
var buildId = requestedBuild ?? games[0].LatestBuild?.Id ?? throw new InvalidOperationException("E2E catalog has no latest build.");
var signedManifest = await api.GetManifestWithBytesAsync(buildId);
var manifest = signedManifest.Manifest;
var signature = await api.GetManifestSignatureAsync(buildId);
if (settingsLoaded)
{
    ManifestSignatureVerifier.Verify(
        signedManifest.RawBytes,
        signature,
        settings.TrustedManifestKeysPem,
        allowEmbeddedPublicKey: false);
}
else
{
    ManifestSignatureVerifier.Verify(signedManifest.RawBytes, signature, allowEmbeddedPublicKey: true);
}
using var downloader = new DownloadManager(httpClient, api, cache, 4, state);
var download = await downloader.DownloadAsync(manifest, $"e2e-{mode}-{manifest.BuildId}");
var installer = new Installer(cache, state);
long? reusedInstalledBytes = null;
long? reconstructedBytes = null;

switch (mode.ToLowerInvariant())
{
    case "install":
        await installer.InstallAsync(manifest, installRoot);
        await AssertByteIdenticalAsync(manifest, sourceRoot, installRoot);
        var launched = Installer.Launch(manifest, installRoot);
        await launched.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(15));
        var marker = Path.Combine(installRoot, "launched.txt");
        if (!File.Exists(marker) || !File.ReadAllText(marker).Contains(manifest.DisplayVersion, StringComparison.Ordinal)) throw new InvalidDataException("Synthetic game launch marker was not written.");
        break;
    case "update":
        var installed = (await state.GetInstalledGamesAsync()).SingleOrDefault(item => item.GameId == manifest.GameId) ?? throw new InvalidOperationException("No prior installed build found for update.");
        var previous = ManifestJson.Deserialize(installed.ManifestJson);
        var update = await installer.UpdateAsync(previous, manifest, installRoot);
        await AssertByteIdenticalAsync(manifest, sourceRoot, installRoot);
        reusedInstalledBytes = update.ReusedInstalledBytes;
        reconstructedBytes = update.ReconstructedBytes;
        Console.WriteLine($"update.reused_installed_bytes={update.ReusedInstalledBytes}");
        Console.WriteLine($"update.reconstructed_bytes={update.ReconstructedBytes}");
        break;
    case "repair":
        await installer.RepairAsync(manifest, installRoot);
        await AssertByteIdenticalAsync(manifest, sourceRoot, installRoot);
        break;
    default: throw new ArgumentException($"Unsupported E2E mode: {mode}");
}

Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new
{
    phase = mode,
    game = manifest.GameId,
    build = manifest.BuildId,
    files = manifest.Files.Count,
    total_encoded_bytes = download.TotalEncodedBytes,
    prepared_bytes = download.PreparedBytes,
    network_bytes = download.NetworkBytes,
    physical_pack_network_bytes = download.PhysicalPackNetworkBytes,
    physical_pack_logical_bytes = download.PhysicalPackLogicalBytes,
    physical_pack_amplification = download.PhysicalPackAmplification,
    reused_cache_bytes = download.ReusedBytes,
    network_savings = download.NetworkSavings,
    chunks_downloaded = download.ChunksDownloaded,
    chunks_reused = download.ChunksReused,
    reused_installed_bytes = reusedInstalledBytes,
    reconstructed_bytes = reconstructedBytes
}));

static string Get(string key, string fallback) => Environment.GetEnvironmentVariable(key) is { Length: > 0 } value ? value : fallback;

static async Task AssertByteIdenticalAsync(Manifest manifest, string sourceRoot, string installRoot)
{
    foreach (var file in manifest.Files)
    {
        var source = PathGuard.ResolveUnderRoot(sourceRoot, file.Path);
        var installed = PathGuard.ResolveUnderRoot(installRoot, file.Path);
        if (!await CompareFilesAsync(source, installed)) throw new InvalidDataException($"Installed bytes differ from source: {file.Path}");
        if (!await Hashing.VerifyFileAsync(installed, file.Size, file.Blake3)) throw new InvalidDataException($"Installed hash verification failed: {file.Path}");
    }
}

static async Task<bool> CompareFilesAsync(string first, string second)
{
    if (!File.Exists(first) || !File.Exists(second)) return false;
    var firstInfo = new FileInfo(first);
    var secondInfo = new FileInfo(second);
    if (firstInfo.Length != secondInfo.Length) return false;
    await using var left = new FileStream(first, FileMode.Open, FileAccess.Read, FileShare.Read, 1024 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
    await using var right = new FileStream(second, FileMode.Open, FileAccess.Read, FileShare.Read, 1024 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
    var leftBuffer = new byte[1024 * 1024];
    var rightBuffer = new byte[1024 * 1024];
    int leftRead;
    while ((leftRead = await left.ReadAsync(leftBuffer)) > 0)
    {
        var rightRead = await right.ReadAsync(rightBuffer);
        if (leftRead != rightRead || !leftBuffer.AsSpan(0, leftRead).SequenceEqual(rightBuffer.AsSpan(0, rightRead))) return false;
    }
    return true;
}
