using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.Versioning;
using System.Text;
using Launcher.Core;
using Microsoft.Win32;

namespace Launcher.App.Runtime;

public sealed record SteamGameInstall(
    string AppId,
    string Name,
    string InstallDirectory,
    string InstallRoot,
    string LibraryRoot,
    long SizeBytes,
    string? ArtworkPath = null,
    string? IconArtworkPath = null,
    bool IsFavorite = false)
{
    public string SizeDisplay => FormatBytes(SizeBytes);

    private static string FormatBytes(long bytes)
    {
        if (bytes <= 0) return "Size unavailable";
        var units = new[] { "B", "KB", "MB", "GB", "TB" };
        var value = (double)bytes;
        var unit = 0;
        while (value >= 1024 && unit < units.Length - 1)
        {
            value /= 1024;
            unit++;
        }

        return $"{value:0.#} {units[unit]}";
    }
}

public sealed record SteamLibrarySnapshot(
    IReadOnlyList<string> LibraryRoots,
    IReadOnlyList<SteamGameInstall> Games,
    string? Error,
    SteamAccountLink? ConnectedAccount = null,
    IReadOnlyList<SteamOwnedGame>? OwnedGames = null,
    string? OwnedGamesError = null)
{
    public static SteamLibrarySnapshot Empty { get; } = new([], [], null);
    public bool IsDetected => LibraryRoots.Count > 0;
}

public static class SteamLibraryDiscovery
{
    private const int MaxVdfBytes = 8 * 1024 * 1024;
    private const ulong SteamId64Base = 76561197960265728;
    private const uint MaxSteamAppId = uint.MaxValue;

    public static SteamLibrarySnapshot Discover(string? steamRoot = null)
    {
        var roots = new List<string>();
        if (!string.IsNullOrWhiteSpace(steamRoot))
        {
            AddRoot(roots, steamRoot);
        }
        else
        {
            foreach (var candidate in FindSteamRoots())
            {
                AddRoot(roots, candidate);
            }
        }

        if (roots.Count == 0)
        {
            return new SteamLibrarySnapshot([], [], "Steam was not found on this PC.");
        }

        var libraryRoots = new List<string>();
        foreach (var root in roots)
        {
            AddRoot(libraryRoots, root);
            var libraryFoldersPath = Path.Combine(root, "steamapps", "libraryfolders.vdf");
            if (!File.Exists(libraryFoldersPath)) continue;

            foreach (var path in ReadLibraryFolderPaths(libraryFoldersPath))
            {
                AddRoot(libraryRoots, path);
            }
        }

        var favoriteAppIds = ReadFavoriteAppIds(roots);
        var games = new List<SteamGameInstall>();
        foreach (var libraryRoot in libraryRoots)
        {
            try
            {
                var steamAppsRoot = Path.Combine(libraryRoot, "steamapps");
                if (!Directory.Exists(steamAppsRoot)) continue;

                foreach (var manifestPath in Directory.EnumerateFiles(steamAppsRoot, "appmanifest_*.acf", SearchOption.TopDirectoryOnly))
                {
                    var game = ReadGameManifest(manifestPath, libraryRoot);
                    if (game is not null)
                    {
                        games.Add(game);
                    }
                }
            }
            catch (IOException)
            {
                // A disconnected or inaccessible library must not block the rest.
            }
            catch (UnauthorizedAccessException)
            {
                // A disconnected or inaccessible library must not block the rest.
            }
        }

        var distinctGames = games
            .GroupBy(game => game.AppId, StringComparer.OrdinalIgnoreCase)
            .Select(group => group.First())
            .OrderBy(game => game.Name, StringComparer.OrdinalIgnoreCase)
            .Select(game => game with
            {
                ArtworkPath = FindArtworkPath(roots, game.AppId),
                IconArtworkPath = FindIconArtworkPath(roots, game.AppId),
                IsFavorite = favoriteAppIds.Contains(game.AppId)
            })
            .ToArray();
        return new SteamLibrarySnapshot(libraryRoots, distinctGames, null);
    }

    private static SteamGameInstall? ReadGameManifest(string manifestPath, string libraryRoot)
    {
        try
        {
            var document = ParseVdf(ReadBoundedText(manifestPath));
            var manifestName = Path.GetFileNameWithoutExtension(manifestPath);
            var manifestAppId = manifestName.StartsWith("appmanifest_", StringComparison.OrdinalIgnoreCase)
                ? manifestName["appmanifest_".Length..]
                : null;
            var appId = (ReadVdfValue(document, "appid") ?? manifestAppId)?.Trim();
            var installDirectory = ReadVdfValue(document, "installdir")?.Trim();
            if (!IsValidAppId(appId) || string.IsNullOrWhiteSpace(installDirectory)) return null;
            if (Path.IsPathRooted(installDirectory)
                || installDirectory is "." or ".."
                || installDirectory.Contains(Path.DirectorySeparatorChar)
                || installDirectory.Contains(Path.AltDirectorySeparatorChar))
            {
                return null;
            }

            var commonRoot = Path.GetFullPath(Path.Combine(libraryRoot, "steamapps", "common"));
            var installRoot = Path.GetFullPath(Path.Combine(commonRoot, installDirectory));
            if (string.Equals(commonRoot, installRoot, StringComparison.OrdinalIgnoreCase)
                || !IsWithin(commonRoot, installRoot)
                || !Directory.Exists(installRoot))
            {
                return null;
            }

            var sizeBytes = long.TryParse(
                    ReadVdfValue(document, "SizeOnDisk"),
                    NumberStyles.Integer,
                    CultureInfo.InvariantCulture,
                    out var parsedSize)
                ? Math.Max(0, parsedSize)
                : 0;
            var name = ReadVdfValue(document, "name")?.Trim();
            return new SteamGameInstall(
                appId!,
                string.IsNullOrWhiteSpace(name) ? installDirectory : name,
                installDirectory,
                installRoot,
                libraryRoot,
                sizeBytes);
        }
        catch (IOException)
        {
            return null;
        }
        catch (InvalidDataException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (ArgumentException)
        {
            return null;
        }
    }

    private static string[] ReadLibraryFolderPaths(string path)
    {
        try
        {
            var document = ParseVdf(ReadBoundedText(path));
            var paths = new List<string>();
            foreach (var libraryFolders in FindObjects(document, "libraryfolders"))
            {
                foreach (var entry in libraryFolders.Values)
                {
                    if (entry.Value.Object is { } folder)
                    {
                        var folderPath = ReadScalar(folder, "path");
                        if (!string.IsNullOrWhiteSpace(folderPath)) paths.Add(folderPath);
                    }
                    else if (long.TryParse(entry.Key, NumberStyles.None, CultureInfo.InvariantCulture, out _)
                        && !string.IsNullOrWhiteSpace(entry.Value.Scalar))
                    {
                        // Steam's older libraryfolders.vdf stored the path as
                        // the value of each numeric entry instead of nesting
                        // it under a "path" key.
                        paths.Add(entry.Value.Scalar!);
                    }
                }
            }

            return paths
                .Where(value => !string.IsNullOrWhiteSpace(value))
                .Select(value => value.Trim())
                .ToArray();
        }
        catch (IOException)
        {
            return [];
        }
        catch (InvalidDataException)
        {
            return [];
        }
        catch (UnauthorizedAccessException)
        {
            return [];
        }
        catch (ArgumentException)
        {
            return [];
        }
        catch (NotSupportedException)
        {
            return [];
        }
    }

    private static HashSet<string> ReadFavoriteAppIds(IEnumerable<string> steamRoots)
    {
        var favoriteAppIds = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var steamRoot in steamRoots)
        {
            var userdataRoot = Path.Combine(steamRoot, "userdata");
            string[] accountRoots;
            try
            {
                if (!Directory.Exists(userdataRoot)) continue;
                var allAccountRoots = Directory.EnumerateDirectories(userdataRoot).ToArray();
                var activeAccountIds = ReadActiveSteamAccountIds(Path.Combine(steamRoot, "config", "loginusers.vdf"));
                accountRoots = activeAccountIds is { Count: > 0 }
                    ? allAccountRoots
                        .Where(path => activeAccountIds.Contains(Path.GetFileName(path)))
                        .ToArray()
                    : allAccountRoots;
            }
            catch (IOException)
            {
                continue;
            }
            catch (UnauthorizedAccessException)
            {
                continue;
            }

            foreach (var accountRoot in accountRoots)
            {
                var configPaths = new[]
                {
                    Path.Combine(accountRoot, "config", "localconfig.vdf"),
                    Path.Combine(accountRoot, "config", "sharedconfig.vdf"),
                    Path.Combine(accountRoot, "7", "remote", "sharedconfig.vdf"),
                    Path.Combine(accountRoot, "7", "remote", "localconfig.vdf")
                };

                foreach (var configPath in configPaths.Distinct(StringComparer.OrdinalIgnoreCase))
                {
                    if (!File.Exists(configPath)) continue;
                    try
                    {
                        favoriteAppIds.UnionWith(ReadFavoriteAppIdsFromFile(configPath));
                    }
                    catch (IOException)
                    {
                        // Steam may be rewriting a config while it is being read.
                    }
                    catch (UnauthorizedAccessException)
                    {
                        // A locked account config must not hide the installed library.
                    }
                    catch (InvalidDataException)
                    {
                        // A malformed config is optional metadata, not a discovery failure.
                    }
                }
            }
        }

        return favoriteAppIds;
    }

    private static HashSet<string>? ReadActiveSteamAccountIds(string path)
    {
        if (!File.Exists(path)) return null;

        try
        {
            var document = ParseVdf(ReadBoundedText(path));
            var users = FindObjects(document, "users").FirstOrDefault();
            if (users is null) return null;

            var candidates = users.Values
                .Where(entry => ulong.TryParse(entry.Key, out _)
                    && entry.Value.Object is not null)
                .Select(entry =>
                {
                    var user = entry.Value.Object!;
                    var accountId = ToSteamAccountId(entry.Key);
                    var autoLogin = user.Values.TryGetValue("AutoLogin", out var autoLoginValue)
                        && string.Equals(autoLoginValue.Scalar, "1", StringComparison.Ordinal);
                    var timestamp = user.Values.TryGetValue("Timestamp", out var timestampValue)
                        && long.TryParse(timestampValue.Scalar, out var parsedTimestamp)
                            ? parsedTimestamp
                            : 0;
                    var mostRecent = ReadFlag(user, "MostRecent");
                    var allowAutoLogin = ReadFlag(user, "AllowAutoLogin") || ReadFlag(user, "AutoLogin");
                    return (accountId, mostRecent, allowAutoLogin, timestamp);
                })
                .Where(candidate => candidate.accountId is not null)
                .ToArray();
            if (candidates.Length == 0) return null;

            var mostRecent = candidates
                .Where(candidate => candidate.mostRecent)
                .Select(candidate => candidate.accountId!)
                .ToHashSet(StringComparer.OrdinalIgnoreCase);
            if (mostRecent.Count > 0) return mostRecent;

            var autoLogin = candidates
                .Where(candidate => candidate.allowAutoLogin)
                .Select(candidate => candidate.accountId!)
                .ToHashSet(StringComparer.OrdinalIgnoreCase);
            if (autoLogin.Count > 0) return autoLogin;

            // Timestamp is not present in every Steam version. Only use it
            // when it identifies one unambiguous account; otherwise read all
            // accounts rather than silently choosing the wrong user's tags.
            var timestamped = candidates
                .Where(candidate => candidate.timestamp > 0)
                .OrderByDescending(candidate => candidate.timestamp)
                .ToArray();
            return timestamped.Length == 1
                ? new HashSet<string>([timestamped[0].accountId!], StringComparer.OrdinalIgnoreCase)
                : null;
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (InvalidDataException)
        {
            return null;
        }
    }

    private static string? ToSteamAccountId(string steamId64)
    {
        if (!ulong.TryParse(steamId64, out var parsed)) return null;
        if (parsed >= SteamId64Base) return (parsed - SteamId64Base).ToString(System.Globalization.CultureInfo.InvariantCulture);
        return parsed <= uint.MaxValue ? parsed.ToString(System.Globalization.CultureInfo.InvariantCulture) : null;
    }

    private static HashSet<string> ReadFavoriteAppIdsFromFile(string path)
    {
        var document = ParseVdf(ReadBoundedText(path));
        var favoriteAppIds = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var apps in FindObjects(document, "apps"))
        {
            foreach (var entry in apps.Values)
            {
                if (!long.TryParse(entry.Key, out _) || entry.Value.Object is not { } app) continue;
                if (!app.Values.TryGetValue("tags", out var tagValue) || tagValue.Object is not { } tags) continue;
                if (tags.Values.Values.Any(value => string.Equals(value.Scalar, "favorite", StringComparison.OrdinalIgnoreCase)))
                {
                    favoriteAppIds.Add(entry.Key);
                }
            }
        }

        return favoriteAppIds;
    }

    private static bool ReadFlag(VdfObject document, string key) =>
        document.Values.TryGetValue(key, out var value)
        && string.Equals(value.Scalar, "1", StringComparison.Ordinal);

    private static string? ReadScalar(VdfObject document, string key) =>
        document.Values.TryGetValue(key, out var value) ? value.Scalar : null;

    private static IEnumerable<VdfObject> FindObjects(VdfObject parent, string name)
    {
        foreach (var entry in parent.Values)
        {
            if (entry.Value.Object is not { } child) continue;
            if (string.Equals(entry.Key, name, StringComparison.OrdinalIgnoreCase))
            {
                yield return child;
            }

            foreach (var nested in FindObjects(child, name))
            {
                yield return nested;
            }
        }
    }

    private static VdfObject ParseVdf(string text)
    {
        var tokens = TokenizeVdf(text);
        var index = 0;
        var root = new VdfObject();
        ParseVdfObject(tokens, ref index, root, depth: 0);
        return root;
    }

    private static void ParseVdfObject(IReadOnlyList<VdfToken> tokens, ref int index, VdfObject target, int depth)
    {
        if (depth > 64) throw new InvalidDataException("Steam metadata nesting is too deep.");

        while (index < tokens.Count)
        {
            var key = tokens[index++];
            if (key.IsCloseBrace) return;
            if (key.IsOpenBrace) continue;
            if (index >= tokens.Count) return;

            var value = tokens[index++];
            if (value.IsCloseBrace) return;
            if (value.IsOpenBrace)
            {
                var child = new VdfObject();
                ParseVdfObject(tokens, ref index, child, depth + 1);
                target.Values[key.Value] = new VdfValue(null, child);
            }
            else
            {
                target.Values[key.Value] = new VdfValue(value.Value, null);
            }
        }
    }

    private static List<VdfToken> TokenizeVdf(string text)
    {
        var tokens = new List<VdfToken>();
        var index = 0;
        while (index < text.Length)
        {
            while (index < text.Length && char.IsWhiteSpace(text[index])) index++;
            if (index >= text.Length) break;

            if (text[index] == '/' && index + 1 < text.Length && text[index + 1] == '/')
            {
                index += 2;
                while (index < text.Length && text[index] != '\n') index++;
                continue;
            }

            if (text[index] == '{')
            {
                tokens.Add(new VdfToken("{", IsOpenBrace: true));
                index++;
                continue;
            }

            if (text[index] == '}')
            {
                tokens.Add(new VdfToken("}", IsCloseBrace: true));
                index++;
                continue;
            }

            if (text[index] == '"')
            {
                index++;
                var value = new StringBuilder();
                var closed = false;
                while (index < text.Length)
                {
                    var character = text[index++];
                    if (character == '"')
                    {
                        closed = true;
                        break;
                    }

                    if (character == '\\' && index < text.Length)
                    {
                        value.Append(character);
                        value.Append(text[index++]);
                    }
                    else
                    {
                        value.Append(character);
                    }
                }

                if (!closed) throw new InvalidDataException("Steam metadata contains an unterminated string.");
                tokens.Add(new VdfToken(UnescapeVdf(value.ToString())));
                continue;
            }

            var start = index;
            while (index < text.Length && !char.IsWhiteSpace(text[index]) && text[index] is not ('{' or '}')) index++;
            if (index > start)
            {
                tokens.Add(new VdfToken(text[start..index]));
            }
        }

        return tokens;
    }

    private static string? ReadVdfValue(VdfObject document, string key)
    {
        foreach (var entry in document.Values)
        {
            if (string.Equals(entry.Key, key, StringComparison.OrdinalIgnoreCase))
            {
                return entry.Value.Scalar;
            }
        }

        foreach (var child in document.Values.Values
            .Where(value => value.Object is not null)
            .Select(value => value.Object!))
        {
            var result = ReadVdfValue(child, key);
            if (result is not null) return result;
        }

        return null;
    }

    private static string ReadBoundedText(string path)
    {
        var info = new FileInfo(path);
        if (info.Length > MaxVdfBytes)
        {
            throw new InvalidDataException($"Steam metadata is too large: {path}");
        }

        return File.ReadAllText(path, Encoding.UTF8);
    }

    private static string? FindArtworkPath(IEnumerable<string> steamRoots, string appId)
    {
        foreach (var steamRoot in steamRoots)
        {
            var cacheRoot = Path.Combine(steamRoot, "appcache", "librarycache", appId);
            if (!Directory.Exists(cacheRoot)) continue;

            var preferredNames = new[]
            {
                "header.jpg",
                "library_header.jpg",
                "library_hero.jpg",
                "library_600x900.jpg",
                "logo.png"
            };
            foreach (var name in preferredNames)
            {
                var candidate = Path.Combine(cacheRoot, name);
                if (File.Exists(candidate)) return candidate;
            }

            try
            {
                var fallback = Directory.EnumerateFiles(cacheRoot, "*.*", SearchOption.AllDirectories)
                    .Where(path => path.EndsWith(".jpg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".jpeg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".png", StringComparison.OrdinalIgnoreCase))
                    .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
                    .FirstOrDefault();
                if (fallback is not null) return fallback;
            }
            catch (IOException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
            catch (UnauthorizedAccessException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
        }

        return null;
    }

    private static string? FindIconArtworkPath(IEnumerable<string> steamRoots, string appId)
    {
        foreach (var steamRoot in steamRoots)
        {
            var cacheRoot = Path.Combine(steamRoot, "appcache", "librarycache", appId);
            if (!Directory.Exists(cacheRoot)) continue;

            try
            {
                // Steam's hash-named cache image is the compact 32x32 app icon.
                // Prefer it over logo.png, which is usually a wide capsule logo
                // and looks wrong when rendered in the sidebar.
                var hashedArtwork = Directory.EnumerateFiles(cacheRoot, "*.*", SearchOption.TopDirectoryOnly)
                    .Where(path => path.EndsWith(".jpg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".jpeg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".png", StringComparison.OrdinalIgnoreCase))
                    .Where(path => !Path.GetFileName(path).StartsWith("library_", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(Path.GetFileName(path), "header.jpg", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(Path.GetFileName(path), "header.jpeg", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(Path.GetFileName(path), "header.png", StringComparison.OrdinalIgnoreCase))
                    .Where(path => Path.GetFileNameWithoutExtension(path).Length >= 16)
                    .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
                    .FirstOrDefault();
                if (hashedArtwork is not null) return hashedArtwork;

                var logo = Path.Combine(cacheRoot, "logo.png");
                if (File.Exists(logo)) return logo;

                foreach (var name in new[] { "header.jpg", "library_header.jpg", "library_hero.jpg", "library_600x900.jpg" })
                {
                    var candidate = Path.Combine(cacheRoot, name);
                    if (File.Exists(candidate)) return candidate;
                }
            }
            catch (IOException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
            catch (UnauthorizedAccessException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
        }

        return null;
    }

    private static string UnescapeVdf(string value)
    {
        var builder = new StringBuilder(value.Length);
        for (var index = 0; index < value.Length; index++)
        {
            if (value[index] != '\\' || index + 1 >= value.Length)
            {
                builder.Append(value[index]);
                continue;
            }

            var escaped = value[++index];
            builder.Append(escaped switch
            {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => escaped
            });
        }

        return builder.ToString();
    }

    private static List<string> FindSteamRoots()
    {
        var candidates = new List<string>();
        AddCandidate(candidates, Environment.GetEnvironmentVariable("STEAM_PATH"));
        if (OperatingSystem.IsWindows())
        {
            foreach (var view in new[] { RegistryView.Default, RegistryView.Registry64, RegistryView.Registry32 }.Distinct())
            {
                AddCandidate(candidates, ReadSteamRegistryValue(RegistryHive.CurrentUser, @"Software\Valve\Steam", "SteamPath", view));
                AddCandidate(candidates, ReadSteamRegistryValue(RegistryHive.LocalMachine, @"Software\Valve\Steam", "InstallPath", view));
                AddCandidate(candidates, ReadSteamRegistryValue(RegistryHive.LocalMachine, @"Software\WOW6432Node\Valve\Steam", "InstallPath", view));
            }
        }

        var programFilesX86 = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86);
        var programFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
        AddCandidate(candidates, Path.Combine(programFilesX86, "Steam"));
        AddCandidate(candidates, Path.Combine(programFiles, "Steam"));
        return candidates;
    }

    [SupportedOSPlatform("windows")]
    private static string? ReadSteamRegistryValue(RegistryHive hive, string subKey, string valueName, RegistryView view)
    {
        try
        {
            using var baseKey = RegistryKey.OpenBaseKey(hive, view);
            using var key = baseKey.OpenSubKey(subKey);
            return key?.GetValue(valueName) as string;
        }
        catch (Exception) when (OperatingSystem.IsWindows())
        {
            return null;
        }
        catch (PlatformNotSupportedException)
        {
            return null;
        }
    }

    private static void AddCandidate(List<string> candidates, string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return;

        var candidate = path.Trim().Trim('"');
        try
        {
            if (File.Exists(candidate)
                && string.Equals(Path.GetFileName(candidate), "steam.exe", StringComparison.OrdinalIgnoreCase))
            {
                candidate = Path.GetDirectoryName(candidate) ?? candidate;
            }
        }
        catch (ArgumentException)
        {
            return;
        }

        if (!candidates.Any(existing => string.Equals(existing, candidate, StringComparison.OrdinalIgnoreCase)))
        {
            candidates.Add(candidate);
        }
    }

    private static void AddRoot(List<string> roots, string path)
    {
        try
        {
            var candidate = path.Trim().Trim('"');
            if (File.Exists(candidate)
                && string.Equals(Path.GetFileName(candidate), "steam.exe", StringComparison.OrdinalIgnoreCase))
            {
                candidate = Path.GetDirectoryName(candidate) ?? candidate;
            }

            var fullPath = Path.GetFullPath(candidate);
            if (!Directory.Exists(fullPath)) return;
            if (!roots.Any(existing => string.Equals(existing, fullPath, StringComparison.OrdinalIgnoreCase)))
            {
                roots.Add(fullPath);
            }
        }
        catch (ArgumentException)
        {
            // Ignore malformed paths in Steam metadata.
        }
        catch (NotSupportedException)
        {
            // Ignore malformed paths in Steam metadata.
        }
    }

    public static string? FindSteamExecutable(string? steamRoot = null)
    {
        if (!OperatingSystem.IsWindows()) return null;

        var roots = new List<string>();
        if (!string.IsNullOrWhiteSpace(steamRoot))
        {
            AddRoot(roots, steamRoot);
        }
        else
        {
            foreach (var candidate in FindSteamRoots()) AddRoot(roots, candidate);
        }

        foreach (var root in roots)
        {
            var executable = Path.Combine(root, "steam.exe");
            if (File.Exists(executable)) return executable;
        }

        return null;
    }

    public static bool IsValidAppId(string? appId) =>
        uint.TryParse(appId, NumberStyles.None, CultureInfo.InvariantCulture, out var parsed)
        && parsed > 0
        && parsed <= MaxSteamAppId;

    public static bool IsValidSteamId64(string? steamId64) =>
        ulong.TryParse(steamId64, NumberStyles.None, CultureInfo.InvariantCulture, out var parsed)
        && parsed >= SteamId64Base;

    public static string? TryGetSteamId64FromClaimedId(string? claimedId)
    {
        const string httpsPrefix = "https://steamcommunity.com/openid/id/";
        const string httpPrefix = "http://steamcommunity.com/openid/id/";
        var value = claimedId?.Trim();
        var steamId64 = value?.StartsWith(httpsPrefix, StringComparison.OrdinalIgnoreCase) == true
            ? value[httpsPrefix.Length..]
            : value?.StartsWith(httpPrefix, StringComparison.OrdinalIgnoreCase) == true
                ? value[httpPrefix.Length..]
                : null;
        return IsValidSteamId64(steamId64) ? steamId64 : null;
    }

    private static bool IsWithin(string root, string candidate)
    {
        var normalizedRoot = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root)) + Path.DirectorySeparatorChar;
        var normalizedCandidate = Path.TrimEndingDirectorySeparator(Path.GetFullPath(candidate)) + Path.DirectorySeparatorChar;
        return normalizedCandidate.StartsWith(normalizedRoot, StringComparison.OrdinalIgnoreCase)
            || string.Equals(
                Path.TrimEndingDirectorySeparator(Path.GetFullPath(root)),
                Path.TrimEndingDirectorySeparator(Path.GetFullPath(candidate)),
                StringComparison.OrdinalIgnoreCase);
    }

    private sealed class VdfObject
    {
        public Dictionary<string, VdfValue> Values { get; } = new(StringComparer.OrdinalIgnoreCase);
    }

    private sealed record VdfValue(string? Scalar, VdfObject? Object);

    private readonly record struct VdfToken(string Value, bool IsOpenBrace = false, bool IsCloseBrace = false);
}

public static class SteamLauncher
{
    public static Task LaunchAsync(SteamGameInstall game)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new Launcher.Core.LauncherOperationException("Steam launching is currently supported on Windows only.");
        }

        if (!SteamLibraryDiscovery.IsValidAppId(game.AppId))
        {
            throw new Launcher.Core.LauncherOperationException("Steam returned an invalid application id.");
        }

        return OpenSteamUriAsync(
            $"steam://rungameid/{game.AppId}",
            ["-applaunch", game.AppId],
            "Steam could not open this game.");
    }

    public static Task InstallAsync(string appId)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new Launcher.Core.LauncherOperationException("Steam installation handoff is currently supported on Windows only.");
        }

        if (!SteamLibraryDiscovery.IsValidAppId(appId))
        {
            throw new Launcher.Core.LauncherOperationException("Steam returned an invalid application id.");
        }

        return OpenSteamUriAsync(
            $"steam://install/{appId}",
            [$"steam://install/{appId}"],
            "Steam could not open the install request.");
    }

    private static Task OpenSteamUriAsync(string uri, IReadOnlyList<string> fallbackArguments, string failureMessage)
    {
        try
        {
            using var started = Process.Start(new ProcessStartInfo
            {
                FileName = uri,
                UseShellExecute = true
            });
            if (started is not null)
            {
                return Task.CompletedTask;
            }
        }
        catch (ArgumentException)
        {
            // Fall through to the direct Steam executable handoff.
        }
        catch (InvalidOperationException)
        {
            // Fall through to the direct Steam executable handoff.
        }
        catch (Win32Exception)
        {
            // Fall through to the direct Steam executable handoff.
        }
        catch (PlatformNotSupportedException)
        {
            // Fall through to the direct Steam executable handoff.
        }

        var steamExecutable = SteamLibraryDiscovery.FindSteamExecutable();
        if (steamExecutable is not null)
        {
            try
            {
                var startInfo = new ProcessStartInfo
                {
                    FileName = steamExecutable,
                    UseShellExecute = false,
                    WorkingDirectory = Path.GetDirectoryName(steamExecutable) ?? string.Empty
                };
                foreach (var argument in fallbackArguments)
                {
                    startInfo.ArgumentList.Add(argument);
                }
                using var started = Process.Start(startInfo);
                if (started is not null)
                {
                    return Task.CompletedTask;
                }
            }
            catch (ArgumentException)
            {
                // Convert all launch failures into one actionable message.
            }
            catch (InvalidOperationException)
            {
                // Convert all launch failures into one actionable message.
            }
            catch (Win32Exception)
            {
                // Convert all launch failures into one actionable message.
            }
        }

        if (steamExecutable is null)
        {
            throw new Launcher.Core.LauncherOperationException("Steam is not installed or its executable could not be found.");
        }

        throw new Launcher.Core.LauncherOperationException(failureMessage);
    }
}
