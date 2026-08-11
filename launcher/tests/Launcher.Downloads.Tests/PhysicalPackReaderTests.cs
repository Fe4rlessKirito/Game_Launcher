using System.Buffers.Binary;
using System.Text;
using Launcher.Downloads;
using Launcher.Security;

namespace Launcher.Downloads.Tests;

public sealed class PhysicalPackReaderTests
{
    [Fact]
    public void ReadsRustPackLayoutAndRejectsIdentityOrIndexCorruption()
    {
        var raw = Encoding.UTF8.GetBytes("physical-pack-test");
        using var compressor = new ZstdSharp.Compressor(3);
        var encoded = compressor.Wrap(raw).ToArray();
        var pack = BuildPack(raw, encoded);
        var packHash = Hashing.ComputeHash(pack);
        var reader = PhysicalPackReader.Parse(pack, packHash);
        var encodedHash = Hashing.ComputeHash(encoded);
        Assert.Equal(encoded, reader.ReadEncoded(encodedHash));
        Assert.Equal(raw, reader.ReadRaw(encodedHash));

        Assert.Throws<InvalidDataException>(() => PhysicalPackReader.Parse(pack, new string('0', 64)));
        var corruptIndex = pack.ToArray();
        corruptIndex[64 + 32] ^= 1;
        Assert.Throws<InvalidDataException>(() => PhysicalPackReader.Parse(corruptIndex));
    }

    private static byte[] BuildPack(byte[] raw, byte[] encoded)
    {
        var encodedHash = Convert.FromHexString(Hashing.ComputeHash(encoded));
        var rawHash = Convert.FromHexString(Hashing.ComputeHash(raw));
        const int headerSize = 64;
        const int entrySize = 96;
        const int footerSize = 72;
        var indexOffset = headerSize + encoded.Length;
        var bytes = new byte[indexOffset + entrySize + footerSize];
        Encoding.ASCII.GetBytes("LGRPACK1").CopyTo(bytes, 0);
        BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(8, 2), 1);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(12, 4), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(16, 8), 1);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(24, 8), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(32, 8), (ulong)indexOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(40, 8), entrySize);
        encoded.CopyTo(bytes, headerSize);
        encodedHash.CopyTo(bytes, indexOffset);
        rawHash.CopyTo(bytes, indexOffset + 32);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(indexOffset + 64, 8), headerSize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(indexOffset + 72, 8), (ulong)encoded.Length);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(indexOffset + 80, 8), (ulong)raw.Length);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(indexOffset + 88, 4), 1);
        var footerOffset = indexOffset + entrySize;
        Encoding.ASCII.GetBytes("LGRPFTR1").CopyTo(bytes, footerOffset);
        BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(footerOffset + 8, 2), 1);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 12, 8), (ulong)indexOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 20, 8), entrySize);
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(footerOffset + 28, 8), 1);
        Convert.FromHexString(Hashing.ComputeHash(bytes.AsSpan(indexOffset, entrySize))).CopyTo(bytes, footerOffset + 40);
        return bytes;
    }
}
