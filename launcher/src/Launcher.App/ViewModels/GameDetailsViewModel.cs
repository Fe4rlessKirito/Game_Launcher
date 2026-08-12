using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.App.Runtime;

namespace Launcher.App.ViewModels;

public partial class GameDetailsViewModel : ObservableObject
{
    private readonly LauncherRuntime? _runtime;
    private RuntimeGame? _game;

    [ObservableProperty]
    private string _status;

    [ObservableProperty]
    private string _actionMessage;

    [ObservableProperty]
    private bool _isInstalled;

    [ObservableProperty]
    private bool _isBusy;

    public GameDetailsViewModel(string title) : this(title, null, null)
    {
    }

    public GameDetailsViewModel(string title, LauncherRuntime? runtime, RuntimeGame? game)
    {
        Title = title;
        _runtime = runtime;
        _game = game;
        _status = game?.StatusText ?? "Installed and launchable";
        _actionMessage = game is null ? "Ready to launch." : "Build metadata is loaded from the launcher service.";
        _isInstalled = game?.IsInstalled ?? true;
    }

    public string Title { get; }
    public string Monogram => _game?.Monogram ?? BuildMonogram(Title);
    public string Description => _game?.Description ?? "A verified local build with content-addressed updates, repairable files, and an offline-ready launch profile.";
    public string Version => _game?.DisplayVersion ?? "1.0.0";
    public string InstallSize => _game?.SizeDisplay ?? "90 B";
    public string InstallLocation => _game?.InstallRoot ?? @"C:\Games\Synthetic Game";
    public bool ShowPlay => IsInstalled && !IsBusy;
    public bool ShowInstall => (!IsInstalled || _game?.State == Launcher.Core.GameState.UpdateAvailable) && !IsBusy;

    public void ApplyRuntimeGame(RuntimeGame? game)
    {
        if (game is null || !string.Equals(game.Title, Title, StringComparison.OrdinalIgnoreCase)) return;
        _game = game;
        IsInstalled = game.IsInstalled;
        Status = game.StatusText;
        OnPropertyChanged(nameof(Monogram));
        OnPropertyChanged(nameof(Description));
        OnPropertyChanged(nameof(Version));
        OnPropertyChanged(nameof(InstallSize));
        OnPropertyChanged(nameof(InstallLocation));
        OnPropertyChanged(nameof(ShowPlay));
        OnPropertyChanged(nameof(ShowInstall));
    }

    [RelayCommand]
    private async Task Play()
    {
        if (_runtime is null || _game is null)
        {
            Status = "Running locally";
            ActionMessage = $"{Title} is ready to launch from the configured install directory.";
            return;
        }

        try
        {
            IsBusy = true;
            Status = "Launching";
            await _runtime.LaunchAsync(_game.Id).ConfigureAwait(true);
            ActionMessage = $"{Title} is running locally.";
        }
        catch (Exception error)
        {
            Status = "Launch failed";
            ActionMessage = error.Message;
        }
        finally
        {
            IsBusy = false;
            OnPropertyChanged(nameof(ShowPlay));
            OnPropertyChanged(nameof(ShowInstall));
        }
    }

    [RelayCommand]
    private async Task Install()
    {
        if (_runtime is null || _game is null)
        {
            IsInstalled = true;
            Status = "Installed and launchable";
            ActionMessage = $"{Title} is installed and BLAKE3 verified.";
            NotifyActionVisibility();
            return;
        }

        try
        {
            IsBusy = true;
            Status = "Downloading";
            ActionMessage = "Resolving signed manifest and storage locations…";
            var progress = new Progress<Launcher.Core.DownloadProgress>(value =>
            {
                Status = value.State.ToString();
                ActionMessage = value.TotalBytes > 0
                    ? $"{FormatBytes(value.DownloadedBytes)} / {FormatBytes(value.TotalBytes)}"
                    : "Preparing verified content…";
            });
            await _runtime.InstallAsync(_game.Id, progress).ConfigureAwait(true);
            ApplyRuntimeGame(_runtime.FindGame(_game.Id));
            ActionMessage = $"{Title} is installed and BLAKE3 verified.";
        }
        catch (Exception error)
        {
            Status = "Install failed";
            ActionMessage = error.Message;
        }
        finally
        {
            IsBusy = false;
            NotifyActionVisibility();
        }
    }

    [RelayCommand]
    private async Task Repair()
    {
        if (_runtime is null || _game is null)
        {
            ActionMessage = $"{Title} passed the local integrity check.";
            Status = IsInstalled ? "Installed and launchable" : "Not installed";
            return;
        }

        try
        {
            IsBusy = true;
            Status = "Repairing";
            ActionMessage = "Verifying files and restoring invalid content…";
            await _runtime.RepairAsync(_game.Id).ConfigureAwait(true);
            ApplyRuntimeGame(_runtime.FindGame(_game.Id));
            ActionMessage = $"{Title} passed the local integrity check.";
        }
        catch (Exception error)
        {
            Status = "Repair failed";
            ActionMessage = error.Message;
        }
        finally
        {
            IsBusy = false;
            NotifyActionVisibility();
        }
    }

    [RelayCommand]
    private async Task Uninstall()
    {
        if (_runtime is null || _game is null)
        {
            IsInstalled = false;
            Status = "Not installed";
            ActionMessage = $"{Title} was removed from the local library.";
            NotifyActionVisibility();
            return;
        }

        try
        {
            IsBusy = true;
            Status = "Uninstalling";
            await _runtime.UninstallAsync(_game.Id).ConfigureAwait(true);
            ApplyRuntimeGame(_runtime.FindGame(_game.Id));
            ActionMessage = $"{Title} was removed from the local library.";
        }
        catch (Exception error)
        {
            Status = "Uninstall failed";
            ActionMessage = error.Message;
        }
        finally
        {
            IsBusy = false;
            NotifyActionVisibility();
        }
    }

    private void NotifyActionVisibility()
    {
        OnPropertyChanged(nameof(ShowPlay));
        OnPropertyChanged(nameof(ShowInstall));
    }

    private static string BuildMonogram(string title)
    {
        var words = title.Split(' ', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var monogram = words.Length > 1
            ? string.Concat(words.Take(2).Select(word => char.ToUpperInvariant(word[0])))
            : new string(title.Where(char.IsLetterOrDigit).Take(2).Select(char.ToUpperInvariant).ToArray());
        return monogram.Length == 0 ? "G" : monogram;
    }

    private static string FormatBytes(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        var value = (double)bytes;
        var units = new[] { "KB", "MB", "GB", "TB" };
        var index = -1;
        while (value >= 1024 && index < units.Length - 1)
        {
            value /= 1024;
            index++;
        }

        return $"{value:0.#} {units[index]}";
    }
}
