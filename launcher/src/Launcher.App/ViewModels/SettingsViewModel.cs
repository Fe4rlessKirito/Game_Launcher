using System.Text.Json;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.Core;

namespace Launcher.App.ViewModels;

public partial class SettingsViewModel : ObservableObject
{
    private const string DefaultApiBaseUrl = "https://vaultnode.pp.ua";
    private const string LegacyLocalApiBaseUrl = "http://127.0.0.1:8080";
    private const string LegacyMantleIpApiBaseUrl = "http://5.231.32.191";
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web) { WriteIndented = true };
    private static readonly JsonSerializerOptions ReadOptions = new(JsonSerializerDefaults.Web);
    private static readonly string DefaultSettingsPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Vaultnode",
        "settings.json");
    private readonly string _settingsPath;

    [ObservableProperty]
    private bool _launchOnStartup;

    [ObservableProperty]
    private bool _minimizeOnClose = true;

    [ObservableProperty]
    private bool _reducedMotion;

    [ObservableProperty]
    private int _concurrentDownloads = 4;

    [ObservableProperty]
    private string _downloadDirectory = @"C:\Games\Launcher\Downloads";

    [ObservableProperty]
    private string _installDirectory = @"C:\Games";

    [ObservableProperty]
    private string _apiBaseUrl = DefaultApiBaseUrl;

    [ObservableProperty]
    private string _saveStatus = "Changes are stored locally.";

    private IReadOnlyDictionary<string, string>? _trustedManifestKeysPem;
    private bool _requireTrustedManifestKeys;

    public SettingsViewModel(string? settingsPath = null)
    {
        _settingsPath = string.IsNullOrWhiteSpace(settingsPath) ? DefaultSettingsPath : settingsPath;
        Load();
    }

    [RelayCommand]
    private void Save()
    {
        ConcurrentDownloads = Math.Clamp(ConcurrentDownloads, 1, 32);
        DownloadDirectory = string.IsNullOrWhiteSpace(DownloadDirectory)
            ? @"C:\Games\Launcher\Downloads"
            : DownloadDirectory.Trim();
        InstallDirectory = string.IsNullOrWhiteSpace(InstallDirectory)
            ? @"C:\Games"
            : InstallDirectory.Trim();
        ApiBaseUrl = string.IsNullOrWhiteSpace(ApiBaseUrl)
            ? DefaultApiBaseUrl
            : ApiBaseUrl.Trim().TrimEnd('/');

        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_settingsPath)!);
            var snapshot = new LauncherSettings(
                LaunchOnStartup,
                MinimizeOnClose,
                DownloadDirectory,
                ConcurrentDownloads,
                DefaultGameDirectory: InstallDirectory,
                ReducedMotion: ReducedMotion,
                ApiBaseUrl: ApiBaseUrl,
                TrustedManifestKeysPem: _trustedManifestKeysPem,
                RequireTrustedManifestKeys: _requireTrustedManifestKeys);
            File.WriteAllText(_settingsPath, JsonSerializer.Serialize(snapshot, JsonOptions));
            SaveStatus = "Changes saved locally.";
        }
        catch (IOException)
        {
            SaveStatus = "Could not save settings locally.";
        }
        catch (UnauthorizedAccessException)
        {
            SaveStatus = "Could not save settings locally.";
        }
    }

    [RelayCommand]
    private void Reset()
    {
        LaunchOnStartup = false;
        MinimizeOnClose = true;
        ReducedMotion = false;
        ConcurrentDownloads = 4;
        DownloadDirectory = @"C:\Games\Launcher\Downloads";
        InstallDirectory = @"C:\Games";
        ApiBaseUrl = DefaultApiBaseUrl;
        SaveStatus = "Defaults restored. Save to keep them.";
    }

    private void Load()
    {
        try
        {
            if (!File.Exists(_settingsPath))
            {
                return;
            }

            var snapshot = JsonSerializer.Deserialize<LauncherSettings>(File.ReadAllText(_settingsPath), ReadOptions);
            if (snapshot is null)
            {
                return;
            }

            var migratedApiEndpoint = !string.IsNullOrWhiteSpace(snapshot.ApiBaseUrl)
                && IsLegacyApiBaseUrl(snapshot.ApiBaseUrl);
            LaunchOnStartup = snapshot.LaunchOnStartup;
            MinimizeOnClose = snapshot.MinimizeOnClose;
            ReducedMotion = snapshot.ReducedMotion;
            ConcurrentDownloads = Math.Clamp(snapshot.ConcurrentDownloads, 1, 32);
            DownloadDirectory = string.IsNullOrWhiteSpace(snapshot.DownloadDirectory)
                ? @"C:\Games\Launcher\Downloads"
                : snapshot.DownloadDirectory;
            InstallDirectory = string.IsNullOrWhiteSpace(snapshot.DefaultGameDirectory)
                ? @"C:\Games"
                : snapshot.DefaultGameDirectory;
            ApiBaseUrl = string.IsNullOrWhiteSpace(snapshot.ApiBaseUrl)
                ? DefaultApiBaseUrl
                : IsLegacyApiBaseUrl(snapshot.ApiBaseUrl)
                    ? DefaultApiBaseUrl
                    : snapshot.ApiBaseUrl;
            _trustedManifestKeysPem = snapshot.TrustedManifestKeysPem;
            _requireTrustedManifestKeys = snapshot.RequireTrustedManifestKeys;

            if (migratedApiEndpoint)
            {
                Save();
            }
        }
        catch (JsonException)
        {
            SaveStatus = "Using default settings.";
        }
        catch (IOException)
        {
            SaveStatus = "Using default settings.";
        }
    }

    private static bool IsLegacyApiBaseUrl(string value)
    {
        var normalized = value.Trim().TrimEnd('/');
        return normalized.Equals(LegacyLocalApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals(LegacyMantleIpApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals("https://5.231.32.191", StringComparison.OrdinalIgnoreCase);
    }

}
