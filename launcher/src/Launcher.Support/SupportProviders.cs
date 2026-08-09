using Launcher.Manifests;

namespace Launcher.Support;

public sealed record SupportFinding(string Provider, string Name, string Value, string? Evidence = null);

public interface ISupportProvider
{
    string Id { get; }
    Task<IReadOnlyList<SupportFinding>> AnalyzeAsync(string buildDirectory, CancellationToken cancellationToken = default);
    void ValidateLaunchProfile(LaunchProfile profile);
}

public sealed class GenericSupportProvider : ISupportProvider
{
    public string Id => "generic";

    public Task<IReadOnlyList<SupportFinding>> AnalyzeAsync(string buildDirectory, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return Task.FromResult<IReadOnlyList<SupportFinding>>([new(Id, "build-directory", Path.GetFullPath(buildDirectory))]);
    }

    public void ValidateLaunchProfile(LaunchProfile profile)
    {
        ManifestValidator.ValidatePortablePath(profile.Executable);
    }
}
