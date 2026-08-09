using ZstdSharp;

namespace Launcher.Downloads;

public static class ZstdCodec
{
    public static byte[] Decompress(ReadOnlySpan<byte> encoded)
    {
        using var decompressor = new Decompressor();
        return decompressor.Unwrap(encoded.ToArray()).ToArray();
    }
}
