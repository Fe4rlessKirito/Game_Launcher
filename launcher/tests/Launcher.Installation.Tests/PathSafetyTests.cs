using Launcher.Security;

namespace Launcher.Installation.Tests;

public class PathSafetyTests
{
    [Theory]
    [InlineData("../escape.txt")]
    [InlineData("C:/Windows/win.ini")]
    [InlineData("a\\b.txt")]
    public void RejectsEscapingManifestPaths(string path) => Assert.Throws<InvalidDataException>(() => PathGuard.ResolveUnderRoot(Path.Combine(Path.GetTempPath(), "launcher-test"), path));
}
