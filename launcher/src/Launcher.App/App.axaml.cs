using Avalonia;
using System.Text.Json;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using Avalonia.Threading;
using Launcher.App.Runtime;
using Launcher.App.ViewModels;

namespace Launcher.App;

public partial class App : Application
{
    internal static SingleInstanceCoordinator? InstanceCoordinator { get; set; }
    private LauncherRuntime? _runtime;

    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            var shell = new ShellViewModel(seedDemoData: false);
            desktop.MainWindow = new MainWindow { DataContext = shell };
            desktop.Exit += OnDesktopExit;
            _ = InitializeRuntimeAsync(shell);
            InstanceCoordinator?.Start(() => Dispatcher.UIThread.Post(() =>
            {
                if (desktop.MainWindow is not { } window) return;
                window.Show();
                window.Activate();
                window.Topmost = true;
                window.Topmost = false;
            }));
        }
        base.OnFrameworkInitializationCompleted();
    }

    private async Task InitializeRuntimeAsync(ShellViewModel shell)
    {
        try
        {
            _runtime = await LauncherRuntime.CreateDefaultAsync().ConfigureAwait(true);
            await shell.InitializeRuntimeAsync(_runtime).ConfigureAwait(true);
        }
        catch (Exception error) when (error is IOException or InvalidDataException or JsonException)
        {
            shell.SetRuntimeError(error.Message);
        }
    }

    private void OnDesktopExit(object? sender, ControlledApplicationLifetimeExitEventArgs e)
    {
        _runtime?.DisposeAsync().AsTask().GetAwaiter().GetResult();
        _runtime = null;
    }
}
