using System.Net.Http.Headers;
using System.Reflection;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Launcher.App.Runtime;

public sealed record LauncherUpdateInfo(
    string Version,
    Uri ReleaseUri,
    Uri InstallerUri,
    string? Sha256,
    long? SizeBytes);

public sealed class LauncherUpdateService
{
    public const string Repository = "Fe4rlessKirito/Game_Launcher";
    private const long MaximumInstallerBytes = 750L * 1024L * 1024L;
    private static readonly Uri LatestReleaseUri = new($"https://api.github.com/repos/{Repository}/releases/latest");
    private static readonly HttpClient SharedHttpClient = CreateHttpClient();
    private readonly HttpClient _httpClient;

    public LauncherUpdateService(HttpClient? httpClient = null, string? currentVersion = null)
    {
        _httpClient = httpClient ?? SharedHttpClient;
        CurrentVersion = string.IsNullOrWhiteSpace(currentVersion)
            ? GetCurrentVersion()
            : currentVersion.Trim();
    }

    public string CurrentVersion { get; }

    public async Task<LauncherUpdateInfo?> CheckAsync(CancellationToken cancellationToken = default)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, LatestReleaseUri);
        request.Headers.UserAgent.ParseAdd($"VaultnodeLauncher/{CurrentVersion}");
        request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/vnd.github+json"));
        request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));

        using var response = await _httpClient.SendAsync(
            request,
            HttpCompletionOption.ResponseHeadersRead,
            cancellationToken).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();

        await using var responseStream = await response.Content
            .ReadAsStreamAsync(cancellationToken)
            .ConfigureAwait(false);
        var release = await JsonSerializer.DeserializeAsync<GitHubRelease>(
            responseStream,
            cancellationToken: cancellationToken).ConfigureAwait(false);
        if (release is null
            || release.Draft
            || release.Prerelease
            || string.IsNullOrWhiteSpace(release.TagName)
            || !IsNewerVersion(release.TagName, CurrentVersion))
        {
            return null;
        }

        var installer = release.Assets?
            .FirstOrDefault(asset => string.Equals(asset.Name, "Vaultnode-Setup.exe", StringComparison.OrdinalIgnoreCase));
        if (installer is null
            || !TryCreateTrustedUri(installer.BrowserDownloadUrl, out var installerUri)
            || !TryCreateTrustedUri(release.HtmlUrl, out var releaseUri))
        {
            return null;
        }

        return new LauncherUpdateInfo(
            release.TagName.Trim(),
            releaseUri,
            installerUri,
            NormalizeSha256(installer.Digest),
            installer.Size);
    }

    public async Task<string> DownloadInstallerAsync(
        LauncherUpdateInfo update,
        CancellationToken cancellationToken = default)
    {
        if (!TryCreateTrustedUri(update.InstallerUri.ToString(), out var installerUri))
        {
            throw new InvalidDataException("The update download location is not trusted.");
        }

        if (string.IsNullOrWhiteSpace(update.Sha256))
        {
            throw new InvalidDataException("The release did not provide a SHA-256 checksum.");
        }

        var updateDirectory = Path.Combine(Path.GetTempPath(), "Vaultnode", "updates");
        Directory.CreateDirectory(updateDirectory);
        var installerPath = Path.Combine(updateDirectory, $"Vaultnode-Setup-{Guid.NewGuid():N}.exe");

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, installerUri);
            request.Headers.UserAgent.ParseAdd($"VaultnodeLauncher/{CurrentVersion}");
            using var response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                cancellationToken).ConfigureAwait(false);
            response.EnsureSuccessStatusCode();

            var contentLength = response.Content.Headers.ContentLength;
            if (contentLength is > MaximumInstallerBytes)
            {
                throw new InvalidDataException("The update installer is larger than the allowed limit.");
            }

            await using (var source = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false))
            await using (var destination = new FileStream(
                installerPath,
                FileMode.CreateNew,
                FileAccess.Write,
                FileShare.None,
                bufferSize: 128 * 1024,
                options: FileOptions.Asynchronous | FileOptions.SequentialScan))
            {
                var buffer = new byte[128 * 1024];
                long totalBytes = 0;
                int bytesRead;
                while ((bytesRead = await source.ReadAsync(buffer.AsMemory(), cancellationToken).ConfigureAwait(false)) > 0)
                {
                    totalBytes = checked(totalBytes + bytesRead);
                    if (totalBytes > MaximumInstallerBytes)
                    {
                        throw new InvalidDataException("The update installer is larger than the allowed limit.");
                    }

                    await destination.WriteAsync(buffer.AsMemory(0, bytesRead), cancellationToken).ConfigureAwait(false);
                }
            }

            await using var verificationStream = new FileStream(
                installerPath,
                FileMode.Open,
                FileAccess.Read,
                FileShare.Read,
                bufferSize: 128 * 1024,
                options: FileOptions.Asynchronous | FileOptions.SequentialScan);
            var actualSha256 = Convert.ToHexString(await SHA256.HashDataAsync(
                verificationStream,
                cancellationToken).ConfigureAwait(false));
            if (!actualSha256.Equals(NormalizeSha256(update.Sha256), StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException("The downloaded update failed its SHA-256 verification.");
            }

            return installerPath;
        }
        catch
        {
            TryDelete(installerPath);
            throw;
        }
    }

    public static string GetCurrentVersion()
    {
        var informationalVersion = Assembly.GetEntryAssembly()?
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()?
            .InformationalVersion;
        return string.IsNullOrWhiteSpace(informationalVersion)
            ? "0.0.0"
            : informationalVersion.Split('+', 2)[0];
    }

    public static bool IsNewerVersion(string candidate, string current)
    {
        if (!TryParseVersion(candidate, out var candidateVersion)
            || !TryParseVersion(current, out var currentVersion))
        {
            return false;
        }

        var coreComparison = candidateVersion.Core.CompareTo(currentVersion.Core);
        if (coreComparison != 0)
        {
            return coreComparison > 0;
        }

        if (candidateVersion.PreRelease.Length == 0 || currentVersion.PreRelease.Length == 0)
        {
            return candidateVersion.PreRelease.Length == 0 && currentVersion.PreRelease.Length > 0;
        }

        return ComparePreRelease(candidateVersion.PreRelease, currentVersion.PreRelease) > 0;
    }

    private static HttpClient CreateHttpClient() => new()
    {
        Timeout = TimeSpan.FromMinutes(10),
    };

    private static bool TryParseVersion(string value, out ComparableVersion version)
    {
        version = default;
        var normalized = value.Trim().TrimStart('v', 'V');
        var buildSeparator = normalized.IndexOf('+');
        if (buildSeparator >= 0)
        {
            normalized = normalized[..buildSeparator];
        }

        var preRelease = string.Empty;
        var preReleaseSeparator = normalized.IndexOf('-');
        if (preReleaseSeparator >= 0)
        {
            preRelease = normalized[(preReleaseSeparator + 1)..];
            normalized = normalized[..preReleaseSeparator];
            if (preRelease.Length == 0)
            {
                return false;
            }
        }

        var numbers = normalized.Split('.', StringSplitOptions.RemoveEmptyEntries);
        if (numbers.Length is < 1 or > 4
            || numbers.Any(number => !int.TryParse(number, out var parsed) || parsed < 0))
        {
            return false;
        }

        var numeric = numbers.Select(int.Parse).ToArray();
        version = new ComparableVersion(
            new Version(
                numeric.ElementAtOrDefault(0),
                numeric.ElementAtOrDefault(1),
                numeric.ElementAtOrDefault(2),
                numeric.ElementAtOrDefault(3)),
            preRelease);
        return true;
    }

    private static int ComparePreRelease(string candidate, string current)
    {
        var candidateParts = candidate.Split('.');
        var currentParts = current.Split('.');
        for (var index = 0; index < Math.Min(candidateParts.Length, currentParts.Length); index++)
        {
            var candidatePart = candidateParts[index];
            var currentPart = currentParts[index];
            var candidateIsNumber = int.TryParse(candidatePart, out var candidateNumber);
            var currentIsNumber = int.TryParse(currentPart, out var currentNumber);
            if (candidateIsNumber && currentIsNumber)
            {
                var numericComparison = candidateNumber.CompareTo(currentNumber);
                if (numericComparison != 0) return numericComparison;
                continue;
            }

            if (candidateIsNumber != currentIsNumber)
            {
                return candidateIsNumber ? -1 : 1;
            }

            var textComparison = string.Compare(candidatePart, currentPart, StringComparison.OrdinalIgnoreCase);
            if (textComparison != 0) return textComparison;
        }

        return candidateParts.Length.CompareTo(currentParts.Length);
    }

    private static bool TryCreateTrustedUri(string? value, out Uri uri)
    {
        if (Uri.TryCreate(value, UriKind.Absolute, out uri!)
            && uri.Scheme == Uri.UriSchemeHttps
            && (uri.Host.Equals("github.com", StringComparison.OrdinalIgnoreCase)
                || uri.Host.Equals("objects.githubusercontent.com", StringComparison.OrdinalIgnoreCase)
                || uri.Host.Equals("release-assets.githubusercontent.com", StringComparison.OrdinalIgnoreCase)))
        {
            return true;
        }

        uri = null!;
        return false;
    }

    private static string? NormalizeSha256(string? digest)
    {
        if (string.IsNullOrWhiteSpace(digest)) return null;
        var normalized = digest.Trim();
        if (normalized.StartsWith("sha256:", StringComparison.OrdinalIgnoreCase))
        {
            normalized = normalized[7..];
        }

        if (normalized.Length != 64 || normalized.Any(character => !Uri.IsHexDigit(character)))
        {
            return null;
        }

        return normalized.ToUpperInvariant();
    }

    private static void TryDelete(string path)
    {
        try
        {
            if (File.Exists(path)) File.Delete(path);
        }
        catch (IOException)
        {
            // A failed update must not hide its verification error.
        }
        catch (UnauthorizedAccessException)
        {
            // A failed update must not hide its verification error.
        }
    }

    private readonly record struct ComparableVersion(Version Core, string PreRelease);

    private sealed class GitHubRelease
    {
        [JsonPropertyName("tag_name")]
        public string TagName { get; init; } = string.Empty;

        [JsonPropertyName("html_url")]
        public string HtmlUrl { get; init; } = string.Empty;

        [JsonPropertyName("draft")]
        public bool Draft { get; init; }

        [JsonPropertyName("prerelease")]
        public bool Prerelease { get; init; }

        [JsonPropertyName("assets")]
        public GitHubAsset[]? Assets { get; init; }
    }

    private sealed class GitHubAsset
    {
        [JsonPropertyName("name")]
        public string Name { get; init; } = string.Empty;

        [JsonPropertyName("browser_download_url")]
        public string BrowserDownloadUrl { get; init; } = string.Empty;

        [JsonPropertyName("digest")]
        public string? Digest { get; init; }

        [JsonPropertyName("size")]
        public long? Size { get; init; }
    }
}
