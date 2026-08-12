using System.Collections.ObjectModel;

namespace Launcher.App.ViewModels;

public sealed class CollectionsViewModel
{
    public CollectionsViewModel(ObservableCollection<SidebarCategory> categories)
    {
        Categories = categories;
    }

    public ObservableCollection<SidebarCategory> Categories { get; }
}
