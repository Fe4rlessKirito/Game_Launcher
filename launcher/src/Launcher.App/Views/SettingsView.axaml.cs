using System.Linq;
using Avalonia.Controls;
using Avalonia.Platform.Storage;
using Launcher.App.ViewModels;

namespace Launcher.App.Views;

public partial class SettingsView : UserControl
{
    public SettingsView() => InitializeComponent();

    private async void OnChooseProfileImageClick(object? sender, Avalonia.Interactivity.RoutedEventArgs e)
    {
        if (DataContext is not SettingsViewModel settings || TopLevel.GetTopLevel(this) is not { } topLevel)
        {
            return;
        }

        var files = await topLevel.StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = "Choose profile picture",
            AllowMultiple = false,
            FileTypeFilter = new[] { FilePickerFileTypes.ImageAll }
        });
        var file = files.Count > 0 ? files[0] : null;
        if (file?.Path.IsFile == true)
        {
            settings.SetProfileImagePath(file.Path.LocalPath);
        }
    }
}
