using Launcher.Security;

namespace Launcher.Installation.Tests;

public class PathSafetyTests
{
    [Theory]
    [InlineData("../escape.txt")]
    [InlineData("../../Windows/System32/test")]
    [InlineData("C:/Windows/win.ini")]
    [InlineData("C:\\Windows\\win.ini")]
    [InlineData("\\\\server\\share\\file")]
    [InlineData("/absolute/path")]
    [InlineData("a\\b.txt")]
    [InlineData("foo/../bar")]
    [InlineData("CON/file.txt")]
    [InlineData("folder/name. ")]
    public void RejectsEscapingManifestPaths(string path) => Assert.Throws<InvalidDataException>(() => PathGuard.ResolveUnderRoot(Path.Combine(Path.GetTempPath(), "launcher-test"), path));

    [Fact]
    public void RejectsSymlinkedDirectoryEscapeWhenSupported()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-path-" + Guid.NewGuid().ToString("N"));
        var outside = Path.Combine(Path.GetTempPath(), "launcher-outside-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        Directory.CreateDirectory(outside);
        try
        {
            try { Directory.CreateSymbolicLink(Path.Combine(root, "link"), outside); }
            catch (UnauthorizedAccessException) { return; }
            catch (IOException) { return; }
            Assert.Throws<IOException>(() => PathGuard.ResolveUnderRoot(root, "link/escaped.txt"));
        }
        finally
        {
            if (Directory.Exists(root)) Directory.Delete(root, true);
            if (Directory.Exists(outside)) Directory.Delete(outside, true);
        }
    }
}
