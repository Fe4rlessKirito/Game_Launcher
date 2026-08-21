using System.Text.Json;
using Avalonia;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.App.Runtime;
using Launcher.Core;
using Launcher.Networking;
using Launcher.App;

namespace Launcher.App.ViewModels;

public partial class SettingsViewModel : ObservableObject
{
    private const string DefaultApiBaseUrl = "https://vaultnode.pp.ua";
    private const string LegacyLocalApiBaseUrl = "http://127.0.0.1:8080";
    private const string LegacyMantleIpApiBaseUrl = "http://5.231.32.191";
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web) { WriteIndented = true };
    private static readonly JsonSerializerOptions ReadOptions = new(JsonSerializerDefaults.Web);
    private static readonly string DefaultSettingsPath = GetDefaultSettingsPath();
    private readonly string _settingsPath;
    private string AppearanceAssetDirectory => Path.Combine(
        Path.GetDirectoryName(Path.GetFullPath(_settingsPath)) ?? AppContext.BaseDirectory,
        "appearance");

    [ObservableProperty]
    private bool _launchOnStartup;

    [ObservableProperty]
    private bool _minimizeOnClose = true;

    [ObservableProperty]
    private bool _reducedMotion;

    [ObservableProperty]
    private int _concurrentDownloads = 4;

    [ObservableProperty]
    private int _cacheSizeGigabytes = 20;

    [ObservableProperty]
    private string _themePreset = "Slate";

    [ObservableProperty]
    private string _accentColor = "#1A9FFF";

    private Color _accentColorValue = Color.Parse("#1A9FFF");
    private Color? _backgroundAccentColor;

    [ObservableProperty]
    private string _accountEmail = string.Empty;

    [ObservableProperty]
    private string _accountPassword = string.Empty;

    [ObservableProperty]
    private string _accountUsername = string.Empty;

    [ObservableProperty]
    private string _authStatus = "Not signed in.";

    [ObservableProperty]
    private bool _isSignedIn;

    [ObservableProperty]
    private bool _isSigningIn;

    [ObservableProperty]
    private bool _compactMode;

    [ObservableProperty]
    private bool _automaticUpdatesEnabled = true;

    [ObservableProperty]
    private double _interfaceTransparency;

    [ObservableProperty]
    private double _backgroundImageOpacity = 0.30;

    [ObservableProperty]
    private bool _matchBackgroundColors = true;

    [ObservableProperty]
    private string _downloadDirectory = @"C:\Games\Launcher\Downloads";

    [ObservableProperty]
    private string _installDirectory = @"C:\Games";

    [ObservableProperty]
    private string _apiBaseUrl = DefaultApiBaseUrl;

    [ObservableProperty]
    private string _profileImagePath = string.Empty;

    [ObservableProperty]
    private Bitmap? _profileImage;

    [ObservableProperty]
    private string _backgroundImagePath = string.Empty;

    [ObservableProperty]
    private Bitmap? _backgroundImage;

    [ObservableProperty]
    private string _saveStatus = "Changes are stored locally.";

    [ObservableProperty]
    private string _selectedSection = "Profile";

    [ObservableProperty]
    private string _steamId64 = string.Empty;

    [ObservableProperty]
    private string _steamPersonaName = string.Empty;

    [ObservableProperty]
    private bool _isSteamConnecting;

    [ObservableProperty]
    private string _steamConnectionStatus = "Not connected.";

    [ObservableProperty]
    private bool _gogIntegrationEnabled;

    [ObservableProperty]
    private bool _ubisoftIntegrationEnabled;

    [ObservableProperty]
    private bool _eaIntegrationEnabled;

    [ObservableProperty]
    private bool _battleNetIntegrationEnabled;

    [ObservableProperty]
    private bool _xboxIntegrationEnabled;

    [ObservableProperty]
    private bool _itchIntegrationEnabled;

    private SteamLibrarySnapshot _steamSnapshot = SteamLibrarySnapshot.Empty;
    private IReadOnlyList<SteamOwnedGame> _cachedSteamOwnedGames = [];
    private EpicLibrarySnapshot _epicSnapshot = EpicLibrarySnapshot.Empty;
    private IReadOnlyList<OptionalStoreSnapshot> _optionalStoreSnapshots = [];

    public event Action? AutomaticUpdatesPreferenceChanged;

    public bool HasProfileImage => ProfileImage is not null;
    public bool ShowDefaultProfile => !HasProfileImage;
    public bool HasBackgroundImage => BackgroundImage is not null;
    public double InterfaceTransparencyPercent
    {
        get => InterfaceTransparency * 100;
        set => InterfaceTransparency = Math.Clamp(value, 0, 70) / 100;
    }
    public double BackgroundImageOpacityPercent
    {
        get => BackgroundImageOpacity * 100;
        set => BackgroundImageOpacity = Math.Clamp(value, 0, 100) / 100;
    }
    public string InterfaceTransparencyDisplay => $"{Math.Round(InterfaceTransparencyPercent):0}%";
    public string BackgroundImageOpacityDisplay => $"{Math.Round(BackgroundImageOpacityPercent):0}%";
    public bool IsBackgroundAccentActive => MatchBackgroundColors && _backgroundAccentColor is not null;
    public string AccentColorDescription => IsBackgroundAccentActive
        ? "Using the sampled accent. Choose a color here to keep a manual accent."
        : "Choose an accent from the color spectrum or palette.";
    public bool IsProfileSelected => SelectedSection == "Profile";
    public bool IsGeneralSelected => SelectedSection == "General";
    public bool IsDownloadsSelected => SelectedSection == "Downloads";
    public bool IsAppearanceSelected => SelectedSection == "Appearance";
    public bool IsAdvancedSelected => SelectedSection == "Advanced";
    public bool IsSteamSelected => SelectedSection == "Steam";
    public bool IsEpicSelected => SelectedSection == "Epic";
    public bool IsIntegrationsSelected => SelectedSection == "Integrations";
    public bool IsNotSignedIn => !IsSignedIn;
    public bool CanSubmitAuth => !IsSigningIn;
    public bool HasSteamAccount => !string.IsNullOrWhiteSpace(SteamId64);
    public bool CanConnectSteam => !IsSteamConnecting;
    public string SteamConnectButtonLabel => HasSteamAccount ? "Sync Steam library" : "Connect Steam account";
    public IReadOnlyList<SteamGameInstall> SteamGames => _steamSnapshot.Games;
    public IReadOnlyList<SteamOwnedGame> SteamOwnedGames => _cachedSteamOwnedGames;
    public IReadOnlyList<string> SteamLibraryRoots => _steamSnapshot.LibraryRoots;
    public bool HasSteamGames => SteamGames.Count > 0;
    public bool HasSteamOwnedGames => SteamOwnedGames.Count > 0;
    public string SteamStatus => HasSteamAccount
        ? _steamSnapshot.OwnedGamesError
            ?? $"Connected · {SteamOwnedGames.Count} owned, {SteamGames.Count} installed"
        : _steamSnapshot.Error
            ?? (!_steamSnapshot.IsDetected
                ? "Steam was not found on this PC."
                : SteamGames.Count == 0
                    ? "Steam detected · No installed games found."
                    : $"{SteamGames.Count} installed Steam game{(SteamGames.Count == 1 ? string.Empty : "s")} detected.");
    public string SteamLibrarySummary => HasSteamAccount
        ? SteamLibraryRoots.Count == 0
            ? $"Account {SteamId64} connected · local Steam install scan unavailable"
            : $"{SteamLibraryRoots.Count} local location{(SteamLibraryRoots.Count == 1 ? string.Empty : "s")} · account {SteamId64}"
        : SteamLibraryRoots.Count == 0
            ? "Steam library locations will appear here when Steam is installed."
            : $"{SteamLibraryRoots.Count} Steam library location{(SteamLibraryRoots.Count == 1 ? string.Empty : "s")} detected.";
    public IReadOnlyList<EpicGameInstall> EpicGames => _epicSnapshot.Games;
    public IReadOnlyList<string> EpicManifestRoots => _epicSnapshot.ManifestRoots;
    public bool HasEpicGames => EpicGames.Count > 0;
    public string EpicStatus => _epicSnapshot.Error
        ?? (!_epicSnapshot.IsDetected
            ? "Epic Games Launcher was not found on this PC."
            : EpicGames.Count == 0
                ? "Epic Games Launcher detected · No installed games found."
                : $"{EpicGames.Count} installed Epic game{(EpicGames.Count == 1 ? string.Empty : "s")} detected.");
    public string EpicLibrarySummary => EpicManifestRoots.Count == 0
        ? "Epic manifest locations will appear here when Epic Games Launcher is installed."
        : $"{EpicManifestRoots.Count} Epic manifest location{(EpicManifestRoots.Count == 1 ? string.Empty : "s")} detected.";
    public string EpicAccountStatus => EpicManifestRoots.Count == 0
        ? "Epic does not provide a supported public consumer-library connection for third-party launchers. Install detection will begin when Epic Games Launcher manifests are available."
        : "Epic does not provide a supported public consumer-library connection for third-party launchers. Vaultnode detects these installed titles locally and hands launching back to Epic Games Launcher.";
    public string GogIntegrationStatus => GetOptionalStoreStatus(OptionalStoreProvider.Gog);
    public string UbisoftIntegrationStatus => GetOptionalStoreStatus(OptionalStoreProvider.UbisoftConnect);
    public string EaIntegrationStatus => GetOptionalStoreStatus(OptionalStoreProvider.EaApp);
    public string BattleNetIntegrationStatus => GetOptionalStoreStatus(OptionalStoreProvider.BattleNet);
    public string XboxIntegrationStatus => GetOptionalStoreStatus(OptionalStoreProvider.Xbox);
    public string ItchIntegrationStatus => GetOptionalStoreStatus(OptionalStoreProvider.Itch);
    public IReadOnlyList<OptionalStoreSnapshot> OptionalStoreSnapshots => _optionalStoreSnapshots;
    public Color AccentColorValue
    {
        get => IsBackgroundAccentActive ? _backgroundAccentColor!.Value : _accentColorValue;
        set
        {
            if (AccentColorValue == value)
            {
                return;
            }

            if (IsBackgroundAccentActive)
            {
                MatchBackgroundColors = false;
                SaveStatus = "Manual accent selected. Save to keep it.";
            }

            _accentColorValue = value;
            AccentColor = ToHex(value);
            OnPropertyChanged();
        }
    }
    public IReadOnlyList<string> ThemeOptions { get; } = ["Slate", "Midnight", "Graphite"];

    private LauncherRuntime? _runtime;
    private IReadOnlyDictionary<string, string>? _trustedManifestKeysPem;
    private bool _requireTrustedManifestKeys;

    private static string GetDefaultSettingsPath()
    {
        var settingsOverride = Environment.GetEnvironmentVariable("LAUNCHER_SETTINGS_PATH");
        if (!string.IsNullOrWhiteSpace(settingsOverride))
        {
            return settingsOverride;
        }

        var localAppData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        if (string.IsNullOrWhiteSpace(localAppData))
        {
            localAppData = AppContext.BaseDirectory;
        }

        var stateRoot = Environment.GetEnvironmentVariable("LAUNCHER_STATE_ROOT");
        stateRoot = string.IsNullOrWhiteSpace(stateRoot)
            ? Path.Combine(localAppData, "Vaultnode")
            : stateRoot;
        return Path.Combine(stateRoot, "settings.json");
    }

    public SettingsViewModel(string? settingsPath = null)
    {
        _settingsPath = string.IsNullOrWhiteSpace(settingsPath) ? DefaultSettingsPath : settingsPath;
        Load();
    }

    public void AttachRuntime(LauncherRuntime runtime)
    {
        _runtime = runtime;
        var enabled = runtime.OptionalStoreEnabled;
        GogIntegrationEnabled = enabled.TryGetValue(OptionalStoreProvider.Gog, out var gog) && gog;
        UbisoftIntegrationEnabled = enabled.TryGetValue(OptionalStoreProvider.UbisoftConnect, out var ubisoft) && ubisoft;
        EaIntegrationEnabled = enabled.TryGetValue(OptionalStoreProvider.EaApp, out var ea) && ea;
        BattleNetIntegrationEnabled = enabled.TryGetValue(OptionalStoreProvider.BattleNet, out var battleNet) && battleNet;
        XboxIntegrationEnabled = enabled.TryGetValue(OptionalStoreProvider.Xbox, out var xbox) && xbox;
        ItchIntegrationEnabled = enabled.TryGetValue(OptionalStoreProvider.Itch, out var itch) && itch;
        ApplySteamSnapshot(runtime.Snapshot.Steam ?? SteamLibrarySnapshot.Empty);
        ApplyEpicSnapshot(runtime.Snapshot.Epic ?? EpicLibrarySnapshot.Empty);
        ApplyOptionalStoreSnapshots(runtime.Snapshot.OptionalStores ?? []);
    }

    public void ApplySteamSnapshot(SteamLibrarySnapshot snapshot)
    {
        _steamSnapshot = snapshot;
        if (snapshot.ConnectedAccount is { } account)
        {
            SteamId64 = account.SteamId64;
            SteamPersonaName = account.PersonaName ?? string.Empty;
            SteamConnectionStatus = string.IsNullOrWhiteSpace(account.PersonaName)
                ? $"Connected · {account.SteamId64}"
                : $"Connected as {account.PersonaName}";
        }
        else if (string.IsNullOrWhiteSpace(SteamId64))
        {
            SteamConnectionStatus = "Not connected.";
        }

        _cachedSteamOwnedGames = snapshot.OwnedGames ?? [];
        OnPropertyChanged(nameof(SteamGames));
        OnPropertyChanged(nameof(SteamOwnedGames));
        OnPropertyChanged(nameof(SteamLibraryRoots));
        OnPropertyChanged(nameof(HasSteamGames));
        OnPropertyChanged(nameof(HasSteamOwnedGames));
        OnPropertyChanged(nameof(HasSteamAccount));
        OnPropertyChanged(nameof(SteamConnectButtonLabel));
        OnPropertyChanged(nameof(SteamStatus));
        OnPropertyChanged(nameof(SteamLibrarySummary));
    }

    public void ApplyEpicSnapshot(EpicLibrarySnapshot snapshot)
    {
        _epicSnapshot = snapshot;
        OnPropertyChanged(nameof(EpicGames));
        OnPropertyChanged(nameof(EpicManifestRoots));
        OnPropertyChanged(nameof(HasEpicGames));
        OnPropertyChanged(nameof(EpicStatus));
        OnPropertyChanged(nameof(EpicLibrarySummary));
    }

    public void ApplyOptionalStoreSnapshots(IReadOnlyList<OptionalStoreSnapshot> snapshots)
    {
        _optionalStoreSnapshots = snapshots;
        OnPropertyChanged(nameof(OptionalStoreSnapshots));
        OnPropertyChanged(nameof(GogIntegrationStatus));
        OnPropertyChanged(nameof(UbisoftIntegrationStatus));
        OnPropertyChanged(nameof(EaIntegrationStatus));
        OnPropertyChanged(nameof(BattleNetIntegrationStatus));
        OnPropertyChanged(nameof(XboxIntegrationStatus));
        OnPropertyChanged(nameof(ItchIntegrationStatus));
    }

    public void ApplyUser(LauncherUserProfile? user)
    {
        IsSignedIn = user is not null;
        AccountUsername = user?.Username ?? string.Empty;
        AccountEmail = user?.Email ?? string.Empty;
        if (user is null)
        {
            AuthStatus = "Not signed in.";
        }
    }

    [RelayCommand]
    private async Task SignIn()
    {
        if (_runtime is null)
        {
            AuthStatus = "The launcher is still connecting. Try again in a moment.";
            return;
        }

        if (string.IsNullOrWhiteSpace(AccountEmail) || !AccountEmail.Contains('@', StringComparison.Ordinal)
            || string.IsNullOrWhiteSpace(AccountPassword) || AccountPassword.Length < 8)
        {
            AuthStatus = "Enter a valid email and an 8-character password.";
            return;
        }

        IsSigningIn = true;
        AuthStatus = "Signing in…";
        try
        {
            var user = await _runtime.SignInAsync(AccountEmail.Trim(), AccountPassword);
            AccountUsername = user.Username ?? string.Empty;
            AccountEmail = user.Email ?? AccountEmail.Trim();
            AccountPassword = string.Empty;
            AuthStatus = "Signed in. Your library is connected.";
        }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or TaskCanceledException)
        {
            AuthStatus = error.Message;
        }
        finally
        {
            IsSigningIn = false;
        }
    }

    [RelayCommand]
    private async Task SignOut()
    {
        if (_runtime is null)
        {
            ApplyUser(null);
            return;
        }

        IsSigningIn = true;
        AuthStatus = "Signing out…";
        try
        {
            await _runtime.SignOutAsync();
            ApplyUser(null);
        }
        catch (Exception error) when (error is HttpRequestException or TaskCanceledException)
        {
            AuthStatus = error.Message;
        }
        finally
        {
            IsSigningIn = false;
        }
    }

    [RelayCommand]
    private async Task ConnectSteam()
    {
        if (_runtime is null)
        {
            SteamConnectionStatus = "The launcher is still connecting. Try again in a moment.";
            return;
        }

        IsSteamConnecting = true;
        SteamConnectionStatus = "Waiting for Steam sign-in…";
        try
        {
            var library = await _runtime.ConnectSteamAsync().ConfigureAwait(true);
            SteamId64 = library.SteamId64;
            SteamPersonaName = library.PersonaName ?? string.Empty;
            _cachedSteamOwnedGames = library.Games;
            ApplySteamSnapshot(_runtime.Snapshot.Steam ?? SteamLibrarySnapshot.Empty);
            Save();
            SteamConnectionStatus = string.IsNullOrWhiteSpace(SteamPersonaName)
                ? $"Connected · {SteamId64}"
                : $"Connected as {SteamPersonaName}";
        }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or InvalidDataException or IOException or LauncherOperationException or TaskCanceledException)
        {
            SteamConnectionStatus = error.Message;
        }
        finally
        {
            IsSteamConnecting = false;
        }
    }

    [RelayCommand]
    private async Task DisconnectSteam()
    {
        if (_runtime is null)
        {
            SteamId64 = string.Empty;
            SteamPersonaName = string.Empty;
            _cachedSteamOwnedGames = [];
            SteamConnectionStatus = "Not connected.";
            Save();
            return;
        }

        IsSteamConnecting = true;
        SteamConnectionStatus = "Disconnecting Steam…";
        try
        {
            await _runtime.DisconnectSteamAsync().ConfigureAwait(true);
            SteamId64 = string.Empty;
            SteamPersonaName = string.Empty;
            _cachedSteamOwnedGames = [];
            ApplySteamSnapshot(_runtime.Snapshot.Steam ?? SteamLibrarySnapshot.Empty);
            Save();
            SteamConnectionStatus = "Not connected.";
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException)
        {
            SteamConnectionStatus = error.Message;
        }
        finally
        {
            IsSteamConnecting = false;
        }
    }

    [RelayCommand]
    private async Task OpenEpicLauncher()
    {
        if (_runtime is null)
        {
            ApplyEpicSnapshot(new EpicLibrarySnapshot([], [], "The launcher is still connecting. Try again in a moment."));
            return;
        }

        try
        {
            await LauncherRuntime.OpenEpicLauncherAsync().ConfigureAwait(true);
        }
        catch (Exception error) when (error is LauncherOperationException or IOException or InvalidOperationException)
        {
            ApplyEpicSnapshot(new EpicLibrarySnapshot(_epicSnapshot.ManifestRoots, _epicSnapshot.Games, error.Message));
        }
    }

    [RelayCommand]
    private async Task SaveUsername()
    {
        if (_runtime is null || !IsSignedIn)
        {
            AuthStatus = "Sign in before changing your username.";
            return;
        }

        var username = AccountUsername.Trim();
        if (!IsValidUsername(username))
        {
            AuthStatus = "Use 3–24 letters, numbers, or underscores.";
            return;
        }

        IsSigningIn = true;
        AuthStatus = "Saving username…";
        try
        {
            var user = await _runtime.UpdateUsernameAsync(username);
            ApplyUser(user);
            AuthStatus = "Username saved.";
        }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or TaskCanceledException)
        {
            AuthStatus = error.Message;
        }
        finally
        {
            IsSigningIn = false;
        }
    }

    [RelayCommand]
    private void Save()
    {
        ConcurrentDownloads = Math.Clamp(ConcurrentDownloads, 1, 32);
        CacheSizeGigabytes = Math.Clamp(CacheSizeGigabytes, 1, 256);
        DownloadDirectory = string.IsNullOrWhiteSpace(DownloadDirectory)
            ? @"C:\Games\Launcher\Downloads"
            : DownloadDirectory.Trim();
        InstallDirectory = string.IsNullOrWhiteSpace(InstallDirectory)
            ? @"C:\Games"
            : InstallDirectory.Trim();
        ApiBaseUrl = string.IsNullOrWhiteSpace(ApiBaseUrl)
            ? DefaultApiBaseUrl
            : ApiBaseUrl.Trim().TrimEnd('/');

        if (!StartupRegistration.TrySetEnabled(LaunchOnStartup, out var startupError))
        {
            SaveStatus = $"Could not update startup setting: {startupError}";
            return;
        }

        try
        {
            var snapshot = new LauncherSettings(
                LaunchOnStartup,
                MinimizeOnClose,
                DownloadDirectory,
                ConcurrentDownloads,
                CacheSizeBytes: (long)CacheSizeGigabytes * 1024L * 1024L * 1024L,
                DefaultGameDirectory: InstallDirectory,
                ReducedMotion: ReducedMotion,
                ApiBaseUrl: ApiBaseUrl,
                TrustedManifestKeysPem: _trustedManifestKeysPem,
                RequireTrustedManifestKeys: _requireTrustedManifestKeys,
                ProfileImagePath: ProfileImagePath,
                ThemePreset: ThemePreset,
                AccentColor: AccentColor,
                CompactMode: CompactMode,
                BackgroundImagePath: BackgroundImagePath,
                AutomaticUpdatesEnabled: AutomaticUpdatesEnabled,
                InterfaceTransparency: InterfaceTransparency,
                BackgroundImageOpacity: BackgroundImageOpacity,
                MatchBackgroundColors: MatchBackgroundColors,
                SteamId64: SteamId64,
                SteamPersonaName: SteamPersonaName,
                SteamOwnedGames: _cachedSteamOwnedGames,
                GogIntegrationEnabled: GogIntegrationEnabled,
                UbisoftIntegrationEnabled: UbisoftIntegrationEnabled,
                EaIntegrationEnabled: EaIntegrationEnabled,
                BattleNetIntegrationEnabled: BattleNetIntegrationEnabled,
                XboxIntegrationEnabled: XboxIntegrationEnabled,
                ItchIntegrationEnabled: ItchIntegrationEnabled);
            WriteSettingsAtomically(_settingsPath, JsonSerializer.Serialize(snapshot, JsonOptions));
            ApplyOptionalStoreSettingsToRuntime();
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

    private static void WriteSettingsAtomically(string path, string contents)
    {
        var fullPath = Path.GetFullPath(path);
        var directory = Path.GetDirectoryName(fullPath)
            ?? throw new InvalidOperationException("Settings path has no parent directory.");
        Directory.CreateDirectory(directory);
        var temporaryPath = Path.Combine(
            directory,
            $".{Path.GetFileName(fullPath)}.{Guid.NewGuid():N}.part");
        try
        {
            File.WriteAllText(temporaryPath, contents);
            File.Move(temporaryPath, fullPath, true);
        }
        finally
        {
            try
            {
                if (File.Exists(temporaryPath)) File.Delete(temporaryPath);
            }
            catch (IOException)
            {
                // The committed settings file is already safe if cleanup is
                // interrupted; the next save will use another unique temp.
            }
        }
    }

    [RelayCommand]
    private void Reset()
    {
        LaunchOnStartup = false;
        MinimizeOnClose = true;
        ReducedMotion = false;
        ConcurrentDownloads = 4;
        CacheSizeGigabytes = 20;
        ThemePreset = "Slate";
        AccentColor = "#1A9FFF";
        CompactMode = false;
        AutomaticUpdatesEnabled = true;
        InterfaceTransparency = 0;
        BackgroundImageOpacity = 0.30;
        MatchBackgroundColors = true;
        DownloadDirectory = @"C:\Games\Launcher\Downloads";
        InstallDirectory = @"C:\Games";
        ApiBaseUrl = DefaultApiBaseUrl;
        ProfileImagePath = string.Empty;
        BackgroundImagePath = string.Empty;
        SteamId64 = string.Empty;
        SteamPersonaName = string.Empty;
        _cachedSteamOwnedGames = [];
        SteamConnectionStatus = "Not connected.";
        GogIntegrationEnabled = false;
        UbisoftIntegrationEnabled = false;
        EaIntegrationEnabled = false;
        BattleNetIntegrationEnabled = false;
        XboxIntegrationEnabled = false;
        ItchIntegrationEnabled = false;
        _trustedManifestKeysPem = null;
        _requireTrustedManifestKeys = false;
        SaveStatus = "Defaults restored. Save to keep them.";
    }

    [RelayCommand]
    private void SelectSection(string? section)
    {
        SelectedSection = section switch
        {
            "General" => "General",
            "Downloads" => "Downloads",
            "Appearance" => "Appearance",
            "Advanced" => "Advanced",
            "Steam" => "Steam",
            "Epic" => "Epic",
            "Integrations" => "Integrations",
            _ => "Profile"
        };
    }

    [RelayCommand]
    private async Task RefreshSteam()
    {
        if (_runtime is null)
        {
            ApplySteamSnapshot(SteamLibraryDiscovery.Discover());
            return;
        }

        try
        {
            var snapshot = await _runtime.RefreshAsync().ConfigureAwait(true);
            ApplySteamSnapshot(snapshot.Steam ?? SteamLibrarySnapshot.Empty);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException)
        {
            ApplySteamSnapshot(new SteamLibrarySnapshot([], [], error.Message));
        }
    }

    [RelayCommand]
    private async Task RefreshEpic()
    {
        if (_runtime is null)
        {
            ApplyEpicSnapshot(EpicLibraryDiscovery.Discover());
            return;
        }

        try
        {
            var snapshot = await _runtime.RefreshAsync().ConfigureAwait(true);
            ApplyEpicSnapshot(snapshot.Epic ?? EpicLibrarySnapshot.Empty);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException)
        {
            ApplyEpicSnapshot(new EpicLibrarySnapshot([], [], error.Message));
        }
    }

    [RelayCommand]
    private async Task RefreshOptionalStores()
    {
        ApplyOptionalStoreSettingsToRuntime();
        if (_runtime is null)
        {
            try
            {
                var snapshots = await Task.Run(() => OptionalStoreDiscovery.Discover(GetOptionalStoreSettings())).ConfigureAwait(true);
                ApplyOptionalStoreSnapshots(snapshots);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException or ArgumentException)
            {
                SaveStatus = error.Message;
            }

            return;
        }

        try
        {
            var snapshot = await _runtime.RefreshAsync().ConfigureAwait(true);
            ApplyOptionalStoreSnapshots(snapshot.OptionalStores ?? []);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException or ArgumentException)
        {
            SaveStatus = error.Message;
        }
    }

    public void SetProfileImagePath(string? path)
    {
        ProfileImagePath = path?.Trim() ?? string.Empty;
        SaveStatus = string.IsNullOrWhiteSpace(ProfileImagePath)
            ? "Default profile picture selected. Save to keep it."
            : "Profile picture selected. Save to keep it.";
    }

    [RelayCommand]
    private void ClearProfileImage()
    {
        SetProfileImagePath(string.Empty);
    }

    public void SetBackgroundImagePath(string? path)
    {
        var sourcePath = path?.Trim() ?? string.Empty;
        if (string.IsNullOrWhiteSpace(sourcePath))
        {
            var previousPath = BackgroundImagePath;
            BackgroundImagePath = string.Empty;
            DeleteManagedAppearanceAsset(previousPath, "background");
            SaveStatus = "Default launcher background selected. Save to keep it.";
            return;
        }

        try
        {
            BackgroundImagePath = StoreAppearanceAsset(sourcePath, "background");
            SaveStatus = "Launcher background copied into AppData. Save to keep it.";
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            SaveStatus = $"Could not store that launcher background: {error.Message}";
        }
    }

    [RelayCommand]
    private void ClearBackgroundImage()
    {
        SetBackgroundImagePath(string.Empty);
    }

    private string StoreAppearanceAsset(string sourcePath, string assetName)
    {
        var fullSourcePath = Path.GetFullPath(sourcePath);
        if (!File.Exists(fullSourcePath))
        {
            throw new FileNotFoundException("The selected image could not be found.", fullSourcePath);
        }

        var assetDirectory = Path.GetFullPath(AppearanceAssetDirectory);
        Directory.CreateDirectory(assetDirectory);
        var targetPath = Path.Combine(assetDirectory, assetName + GetImageExtension(fullSourcePath));
        if (!PathsEqual(fullSourcePath, targetPath))
        {
            var temporaryPath = Path.Combine(
                assetDirectory,
                $".{assetName}.{Guid.NewGuid():N}.part");
            try
            {
                File.Copy(fullSourcePath, temporaryPath, true);
                File.Move(temporaryPath, targetPath, true);
            }
            finally
            {
                try
                {
                    if (File.Exists(temporaryPath)) File.Delete(temporaryPath);
                }
                catch (IOException)
                {
                    // The completed asset is safe even if cleanup is interrupted.
                }
            }
        }

        DeleteOtherManagedAppearanceAssets(assetDirectory, assetName, targetPath);
        return targetPath;
    }

    private string MigrateAppearanceAsset(string path, string assetName)
    {
        if (string.IsNullOrWhiteSpace(path)
            || !File.Exists(path)
            || IsManagedAppearanceAsset(path, assetName))
        {
            return path;
        }

        try
        {
            return StoreAppearanceAsset(path, assetName);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            SaveStatus = "The existing launcher background could not be copied into AppData.";
            return path;
        }
    }

    private void DeleteManagedAppearanceAsset(string path, string assetName)
    {
        if (!IsManagedAppearanceAsset(path, assetName))
        {
            return;
        }

        try
        {
            File.Delete(path);
        }
        catch (IOException)
        {
            // A locked asset can be replaced or cleaned up on the next selection.
        }
        catch (UnauthorizedAccessException)
        {
            // Keep the setting usable even when cleanup is denied by the profile.
        }
    }

    private void DeleteOtherManagedAppearanceAssets(string assetDirectory, string assetName, string retainedPath)
    {
        try
        {
            foreach (var path in Directory.EnumerateFiles(assetDirectory, assetName + ".*", SearchOption.TopDirectoryOnly))
            {
                if (!PathsEqual(path, retainedPath))
                {
                    DeleteManagedAppearanceAsset(path, assetName);
                }
            }
        }
        catch (DirectoryNotFoundException)
        {
            // The directory was removed between the copy and cleanup.
        }
        catch (IOException)
        {
            // Cleanup is best effort; the retained asset is already usable.
        }
        catch (UnauthorizedAccessException)
        {
            // Cleanup is best effort; the retained asset is already usable.
        }
    }

    private bool IsManagedAppearanceAsset(string path, string assetName)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return false;
        }

        try
        {
            var fullPath = Path.GetFullPath(path);
            var expectedDirectory = Path.GetFullPath(AppearanceAssetDirectory);
            return fullPath.StartsWith(expectedDirectory + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
                && Path.GetFileName(fullPath).StartsWith(assetName + ".", StringComparison.OrdinalIgnoreCase);
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (NotSupportedException)
        {
            return false;
        }
    }

    private static string GetImageExtension(string path)
    {
        var extension = Path.GetExtension(path).ToLowerInvariant();
        return extension is ".bmp" or ".gif" or ".ico" or ".jpeg" or ".jpg" or ".png" or ".tif" or ".tiff" or ".webp"
            ? extension
            : ".image";
    }

    private static bool PathsEqual(string first, string second) =>
        string.Equals(
            Path.GetFullPath(first).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar),
            Path.GetFullPath(second).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar),
            StringComparison.OrdinalIgnoreCase);

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
            var savedBackgroundImagePath = snapshot.BackgroundImagePath ?? string.Empty;
            var migratedBackgroundImagePath = MigrateAppearanceAsset(savedBackgroundImagePath, "background");
            var migratedBackgroundImage = !string.Equals(
                savedBackgroundImagePath,
                migratedBackgroundImagePath,
                StringComparison.OrdinalIgnoreCase);
            savedBackgroundImagePath = migratedBackgroundImagePath;
            LaunchOnStartup = snapshot.LaunchOnStartup;
            MinimizeOnClose = snapshot.MinimizeOnClose;
            ReducedMotion = snapshot.ReducedMotion;
            ConcurrentDownloads = Math.Clamp(snapshot.ConcurrentDownloads, 1, 32);
            CacheSizeGigabytes = (int)Math.Clamp(
                Math.Round(snapshot.CacheSizeBytes / (double)(1024L * 1024L * 1024L)),
                1,
                256);
            ThemePreset = ThemeOptions.Contains(snapshot.ThemePreset, StringComparer.OrdinalIgnoreCase)
                ? ThemeOptions.First(option => option.Equals(snapshot.ThemePreset, StringComparison.OrdinalIgnoreCase))
                : "Slate";
            AccentColor = string.IsNullOrWhiteSpace(snapshot.AccentColor) ? "#1A9FFF" : snapshot.AccentColor;
            CompactMode = snapshot.CompactMode;
            AutomaticUpdatesEnabled = snapshot.AutomaticUpdatesEnabled;
            InterfaceTransparency = Math.Clamp(snapshot.InterfaceTransparency, 0, 0.70);
            BackgroundImageOpacity = Math.Clamp(snapshot.BackgroundImageOpacity, 0, 1);
            MatchBackgroundColors = snapshot.MatchBackgroundColors;
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
            ProfileImagePath = snapshot.ProfileImagePath ?? string.Empty;
            BackgroundImagePath = savedBackgroundImagePath;
            SteamId64 = SteamLibraryDiscovery.IsValidSteamId64(snapshot.SteamId64) ? snapshot.SteamId64 : string.Empty;
            SteamPersonaName = string.IsNullOrWhiteSpace(snapshot.SteamPersonaName) ? string.Empty : snapshot.SteamPersonaName;
            _cachedSteamOwnedGames = snapshot.SteamOwnedGames ?? [];
            GogIntegrationEnabled = snapshot.GogIntegrationEnabled;
            UbisoftIntegrationEnabled = snapshot.UbisoftIntegrationEnabled;
            EaIntegrationEnabled = snapshot.EaIntegrationEnabled;
            BattleNetIntegrationEnabled = snapshot.BattleNetIntegrationEnabled;
            XboxIntegrationEnabled = snapshot.XboxIntegrationEnabled;
            ItchIntegrationEnabled = snapshot.ItchIntegrationEnabled;
            SteamConnectionStatus = string.IsNullOrWhiteSpace(SteamId64)
                ? "Not connected."
                : string.IsNullOrWhiteSpace(SteamPersonaName)
                    ? $"Connected · {SteamId64}"
                    : $"Connected as {SteamPersonaName}";
            _trustedManifestKeysPem = snapshot.TrustedManifestKeysPem;
            _requireTrustedManifestKeys = snapshot.RequireTrustedManifestKeys;

            if (migratedApiEndpoint || migratedBackgroundImage)
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
        catch (UnauthorizedAccessException)
        {
            SaveStatus = "Using default settings.";
        }
    }

    partial void OnProfileImagePathChanged(string value)
    {
        ProfileImage?.Dispose();
        ProfileImage = null;
        if (string.IsNullOrWhiteSpace(value) || !File.Exists(value))
        {
            return;
        }

        try
        {
            ProfileImage = new Bitmap(value);
        }
        catch (ArgumentException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
        catch (IOException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
        catch (InvalidDataException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
        catch (InvalidOperationException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
        catch (UnauthorizedAccessException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
    }

    partial void OnBackgroundImagePathChanged(string value)
    {
        BackgroundImage?.Dispose();
        BackgroundImage = null;
        if (string.IsNullOrWhiteSpace(value) || !File.Exists(value))
        {
            return;
        }

        try
        {
            BackgroundImage = new Bitmap(value);
        }
        catch (ArgumentException)
        {
            SaveStatus = "That launcher background could not be loaded.";
        }
        catch (IOException)
        {
            SaveStatus = "That launcher background could not be loaded.";
        }
        catch (InvalidDataException)
        {
            SaveStatus = "That launcher background could not be loaded.";
        }
        catch (InvalidOperationException)
        {
            SaveStatus = "That launcher background could not be loaded.";
        }
        catch (UnauthorizedAccessException)
        {
            SaveStatus = "That launcher background could not be loaded.";
        }
    }

    partial void OnSelectedSectionChanged(string value)
    {
        OnPropertyChanged(nameof(IsProfileSelected));
        OnPropertyChanged(nameof(IsGeneralSelected));
        OnPropertyChanged(nameof(IsDownloadsSelected));
        OnPropertyChanged(nameof(IsAppearanceSelected));
        OnPropertyChanged(nameof(IsAdvancedSelected));
        OnPropertyChanged(nameof(IsSteamSelected));
        OnPropertyChanged(nameof(IsEpicSelected));
        OnPropertyChanged(nameof(IsIntegrationsSelected));
    }

    partial void OnThemePresetChanged(string value) => ApplyAppearance();

    partial void OnAccentColorChanged(string value)
    {
        var parsed = ParseColorOrDefault(value, Color.Parse("#1A9FFF"));
        if (_accentColorValue != parsed)
        {
            _accentColorValue = parsed;
            OnPropertyChanged(nameof(AccentColorValue));
        }

        ApplyAppearance();
    }

    partial void OnCompactModeChanged(bool value) => ApplyAppearance();

    partial void OnAutomaticUpdatesEnabledChanged(bool value) => AutomaticUpdatesPreferenceChanged?.Invoke();

    partial void OnInterfaceTransparencyChanged(double value)
    {
        var clamped = Math.Clamp(value, 0, 0.70);
        if (Math.Abs(clamped - value) > 0.0001)
        {
            InterfaceTransparency = clamped;
            return;
        }

        OnPropertyChanged(nameof(InterfaceTransparencyPercent));
        OnPropertyChanged(nameof(InterfaceTransparencyDisplay));
        ApplyAppearance();
    }

    partial void OnBackgroundImageOpacityChanged(double value)
    {
        var clamped = Math.Clamp(value, 0, 1);
        if (Math.Abs(clamped - value) > 0.0001)
        {
            BackgroundImageOpacity = clamped;
            return;
        }

        OnPropertyChanged(nameof(BackgroundImageOpacityPercent));
        OnPropertyChanged(nameof(BackgroundImageOpacityDisplay));
    }

    partial void OnMatchBackgroundColorsChanged(bool value)
    {
        OnPropertyChanged(nameof(AccentColorValue));
        OnPropertyChanged(nameof(IsBackgroundAccentActive));
        OnPropertyChanged(nameof(AccentColorDescription));
        ApplyAppearance();
    }

    partial void OnIsSignedInChanged(bool value) => OnPropertyChanged(nameof(IsNotSignedIn));

    partial void OnIsSigningInChanged(bool value) => OnPropertyChanged(nameof(CanSubmitAuth));

    partial void OnIsSteamConnectingChanged(bool value)
    {
        OnPropertyChanged(nameof(CanConnectSteam));
    }

    partial void OnGogIntegrationEnabledChanged(bool value) => OnOptionalStoreSettingChanged();

    partial void OnUbisoftIntegrationEnabledChanged(bool value) => OnOptionalStoreSettingChanged();

    partial void OnEaIntegrationEnabledChanged(bool value) => OnOptionalStoreSettingChanged();

    partial void OnBattleNetIntegrationEnabledChanged(bool value) => OnOptionalStoreSettingChanged();

    partial void OnXboxIntegrationEnabledChanged(bool value) => OnOptionalStoreSettingChanged();

    partial void OnItchIntegrationEnabledChanged(bool value) => OnOptionalStoreSettingChanged();

    partial void OnSteamId64Changed(string value)
    {
        OnPropertyChanged(nameof(HasSteamAccount));
        OnPropertyChanged(nameof(SteamConnectButtonLabel));
        OnPropertyChanged(nameof(SteamStatus));
        OnPropertyChanged(nameof(SteamLibrarySummary));
    }

    partial void OnSteamPersonaNameChanged(string value)
    {
        if (HasSteamAccount)
        {
            SteamConnectionStatus = string.IsNullOrWhiteSpace(value)
                ? $"Connected · {SteamId64}"
                : $"Connected as {value}";
        }
    }

    partial void OnProfileImageChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasProfileImage));
        OnPropertyChanged(nameof(ShowDefaultProfile));
    }

    partial void OnBackgroundImageChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasBackgroundImage));
        OnPropertyChanged(nameof(AccentColorValue));
        OnPropertyChanged(nameof(IsBackgroundAccentActive));
        OnPropertyChanged(nameof(AccentColorDescription));
        ApplyAppearance();
    }

    private Dictionary<OptionalStoreProvider, bool> GetOptionalStoreSettings() =>
        new Dictionary<OptionalStoreProvider, bool>
        {
            [OptionalStoreProvider.Gog] = GogIntegrationEnabled,
            [OptionalStoreProvider.UbisoftConnect] = UbisoftIntegrationEnabled,
            [OptionalStoreProvider.EaApp] = EaIntegrationEnabled,
            [OptionalStoreProvider.BattleNet] = BattleNetIntegrationEnabled,
            [OptionalStoreProvider.Xbox] = XboxIntegrationEnabled,
            [OptionalStoreProvider.Itch] = ItchIntegrationEnabled,
        };

    private void ApplyOptionalStoreSettingsToRuntime()
    {
        if (_runtime is null) return;
        foreach (var setting in GetOptionalStoreSettings())
        {
            _runtime.SetOptionalStoreEnabled(setting.Key, setting.Value);
        }
    }

    private void OnOptionalStoreSettingChanged()
    {
        _optionalStoreSnapshots = [];
        OnPropertyChanged(nameof(OptionalStoreSnapshots));
        OnPropertyChanged(nameof(GogIntegrationStatus));
        OnPropertyChanged(nameof(UbisoftIntegrationStatus));
        OnPropertyChanged(nameof(EaIntegrationStatus));
        OnPropertyChanged(nameof(BattleNetIntegrationStatus));
        OnPropertyChanged(nameof(XboxIntegrationStatus));
        OnPropertyChanged(nameof(ItchIntegrationStatus));
    }

    private string GetOptionalStoreStatus(OptionalStoreProvider provider)
    {
        if (!GetOptionalStoreSettings().TryGetValue(provider, out var enabled) || !enabled)
        {
            return "Disabled";
        }

        return _optionalStoreSnapshots.FirstOrDefault(snapshot => snapshot.Provider == provider)?.StatusText
            ?? "Enabled · refresh to scan";
    }

    private static bool IsLegacyApiBaseUrl(string value)
    {
        var normalized = value.Trim().TrimEnd('/');
        return normalized.Equals(LegacyLocalApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals(LegacyMantleIpApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals("https://5.231.32.191", StringComparison.OrdinalIgnoreCase);
    }

    private void ApplyAppearance()
    {
        if (Application.Current is not { } application)
        {
            return;
        }

        var palette = ThemePreset switch
        {
            "Midnight" => ThemePalette.Midnight,
            "Graphite" => ThemePalette.Graphite,
            _ => ThemePalette.Slate
        };
        var imagePalette = MatchBackgroundColors && BackgroundImage is not null
            ? BackgroundPaletteExtractor.Extract(BackgroundImage)
            : null;
        var previousBackgroundAccent = _backgroundAccentColor;
        _backgroundAccentColor = imagePalette?.Accent;
        var background = TintColor(Color.Parse(palette.Background), imagePalette, 0.18);
        var chromeBar = TintColor(Color.Parse(palette.ChromeBar), imagePalette, 0.11);
        var sidebar = TintColor(Color.Parse(palette.Sidebar), imagePalette, 0.16);
        var surface = TintColor(Color.Parse(palette.Surface), imagePalette, 0.16);
        var elevated = TintColor(Color.Parse(palette.Elevated), imagePalette, 0.22);
        var border = TintColor(Color.Parse(palette.Border), imagePalette, 0.18);
        var downloadChart = TintColor(Color.Parse(palette.DownloadChart), imagePalette, 0.08);
        var downloadPanel = TintColor(Color.Parse(palette.DownloadPanel), imagePalette, 0.15);
        var resources = application.Resources;
        var surfaceAlpha = ToAlpha(1 - InterfaceTransparency);
        var borderAlpha = Math.Max((byte)96, surfaceAlpha);
        resources["BackgroundPrimaryColor"] = WithAlpha(background, surfaceAlpha);
        resources["ChromeBarColor"] = WithAlpha(chromeBar, surfaceAlpha);
        resources["SidebarBackgroundColor"] = WithAlpha(sidebar, surfaceAlpha);
        resources["SurfacePrimaryColor"] = WithAlpha(surface, surfaceAlpha);
        resources["SurfaceElevatedColor"] = WithAlpha(elevated, surfaceAlpha);
        resources["BorderSubtleColor"] = WithAlpha(border, borderAlpha);
        resources["TextPrimaryColor"] = Color.Parse(palette.TextPrimary);
        resources["TextSecondaryColor"] = Color.Parse(palette.TextSecondary);
        resources["TextMutedColor"] = Color.Parse(palette.TextMuted);
        resources["ArtworkPrimaryColor"] = Color.Parse(palette.ArtworkPrimary);
        resources["ArtworkSecondaryColor"] = Color.Parse(palette.ArtworkSecondary);
        resources["ArtworkTertiaryColor"] = Color.Parse(palette.ArtworkTertiary);
        resources["DownloadChartColor"] = WithAlpha(downloadChart, surfaceAlpha);
        resources["DownloadPanelColor"] = WithAlpha(downloadPanel, surfaceAlpha);
        var overlayAlpha = (byte)Math.Clamp(
            (int)Math.Round(199 * (0.55 + ((100 - InterfaceTransparency * 100) / 100 * 0.45))),
            90,
            199);
        resources["BackgroundImageOverlayColor"] = Color.FromArgb(overlayAlpha, 11, 17, 27);

        var accent = _backgroundAccentColor ?? ParseColorOrDefault(AccentColor, Color.Parse(palette.Accent));
        resources["AccentPrimaryColor"] = accent;
        resources["AccentSoftColor"] = Color.FromArgb(64, accent.R, accent.G, accent.B);
        // Fluent controls (including ToggleSwitch) use the theme's system
        // accent resources rather than the launcher-specific accent brush.
        // Keep both resource families synchronized so checked controls follow
        // the color selected in Appearance immediately.
        resources["SystemAccentColor"] = accent;
        resources["SystemAccentColorLight1"] = ScaleColor(accent, 1.15);
        resources["SystemAccentColorLight2"] = ScaleColor(accent, 1.30);
        resources["SystemAccentColorLight3"] = ScaleColor(accent, 1.45);
        resources["SystemAccentColorDark1"] = ScaleColor(accent, 0.70);
        resources["SystemAccentColorDark2"] = ScaleColor(accent, 0.50);
        resources["SystemAccentColorDark3"] = ScaleColor(accent, 0.30);
        resources["SidebarWidth"] = CompactMode ? 220d : 244d;
        resources["NavButtonPadding"] = CompactMode ? new Thickness(10, 8) : new Thickness(12, 11);
        resources["HomeButtonPadding"] = CompactMode ? new Thickness(8, 6) : new Thickness(10, 7);
        resources["GamesHeaderPadding"] = CompactMode ? new Thickness(8, 5) : new Thickness(10, 7);
        resources["SidebarGamePadding"] = CompactMode ? new Thickness(4, 2) : new Thickness(5, 4);
        if (previousBackgroundAccent != _backgroundAccentColor)
        {
            OnPropertyChanged(nameof(AccentColorValue));
            OnPropertyChanged(nameof(IsBackgroundAccentActive));
            OnPropertyChanged(nameof(AccentColorDescription));
        }
    }

    private static Color ParseColorOrDefault(string value, Color fallback)
    {
        if (Color.TryParse((value ?? string.Empty).AsSpan(), out var color))
        {
            return color;
        }

        return fallback;
    }

    private static Color ScaleColor(Color color, double factor)
    {
        return Color.FromArgb(
            color.A,
            ScaleChannel(color.R, factor),
            ScaleChannel(color.G, factor),
            ScaleChannel(color.B, factor));
    }

    private static Color TintColor(Color baseColor, BackgroundPalette? imagePalette, double amount)
    {
        return imagePalette is null
            ? baseColor
            : BlendColor(baseColor, imagePalette.Tint, amount);
    }

    private static Color BlendColor(Color first, Color second, double amount)
    {
        var factor = Math.Clamp(amount, 0, 1);
        return Color.FromArgb(
            first.A,
            BlendChannel(first.R, second.R, factor),
            BlendChannel(first.G, second.G, factor),
            BlendChannel(first.B, second.B, factor));
    }

    private static byte BlendChannel(byte first, byte second, double amount) =>
        (byte)Math.Clamp((int)Math.Round(first + (second - first) * amount), 0, byte.MaxValue);

    private static Color WithAlpha(Color color, byte alpha) =>
        Color.FromArgb(alpha, color.R, color.G, color.B);

    private static byte ToAlpha(double opacity) =>
        (byte)Math.Clamp((int)Math.Round(Math.Clamp(opacity, 0, 1) * byte.MaxValue), 0, byte.MaxValue);

    private static byte ScaleChannel(byte channel, double factor)
    {
        return (byte)Math.Clamp((int)Math.Round(channel * factor), 0, byte.MaxValue);
    }

    private static string ToHex(Color color) => $"#{color.R:X2}{color.G:X2}{color.B:X2}";

    private static bool IsValidUsername(string value)
    {
        return value.Length is >= 3 and <= 24
            && value.All(character => character is >= 'A' and <= 'Z'
                or >= 'a' and <= 'z'
                or >= '0' and <= '9'
                or '_');
    }

    private sealed record ThemePalette(
        string Background,
        string ChromeBar,
        string Sidebar,
        string Surface,
        string Elevated,
        string Border,
        string TextPrimary,
        string TextSecondary,
        string TextMuted,
        string Accent,
        string ArtworkPrimary,
        string ArtworkSecondary,
        string ArtworkTertiary,
        string DownloadChart,
        string DownloadPanel)
    {
        public static ThemePalette Slate { get; } = new(
            "#2A2F3A", "#171D25", "#2A2F3A", "#2A2F3A", "#3A4352", "#3D4A5A",
            "#D6D7D8", "#A4B3C2", "#6D7886", "#1A9FFF", "#1D4D78", "#4B3D5E", "#64483D", "#0E141B", "#2A2F3A");

        public static ThemePalette Midnight { get; } = new(
            "#171A21", "#101318", "#202631", "#20242D", "#303746", "#384252",
            "#EEF1F5", "#B7C1CF", "#7C8899", "#5EA7FF", "#214C76", "#4C4267", "#6A4D42", "#0B1017", "#20242D");

        public static ThemePalette Graphite { get; } = new(
            "#292929", "#1C1C1C", "#303030", "#292929", "#414141", "#4C4C4C",
            "#F0F0F0", "#C3C3C3", "#8F8F8F", "#D59B55", "#63513E", "#574E66", "#6C4D40", "#111111", "#292929");
    }

}
