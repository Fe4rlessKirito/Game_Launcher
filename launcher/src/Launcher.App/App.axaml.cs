using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using Avalonia.Threading;
using Launcher.App.ViewModels;

namespace Launcher.App;

public partial class App : Application
{
    internal static SingleInstanceCoordinator? InstanceCoordinator { get; set; }

    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            desktop.MainWindow = new MainWindow { DataContext = new ShellViewModel() };
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
}
