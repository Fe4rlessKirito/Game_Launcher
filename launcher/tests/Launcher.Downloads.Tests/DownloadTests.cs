using Launcher.Downloads;

namespace Launcher.Downloads.Tests;

public class DownloadTests
{
    [Fact]
    public void ZstdRoundTripPreservesBytes()
    {
        var source = Enumerable.Range(0, 1024).Select(index => (byte)(index % 251)).ToArray();
        using var compressor = new ZstdSharp.Compressor(3);
        var encoded = compressor.Wrap(source);
        Assert.Equal(source, ZstdCodec.Decompress(encoded));
    }
}
