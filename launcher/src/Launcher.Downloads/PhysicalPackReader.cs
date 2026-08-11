using System.Buffers.Binary;
using System.Globalization;
using Launcher.Security;

namespace Launcher.Downloads;

public sealed record PhysicalPackEntry(
    string EncodedHash,
    string RawHash,
    ulong Offset,
    ulong EncodedLength,
    ulong RawLength,
    uint Compression,
    uint Flags);

/// <summary>
/// Strict reader for the versioned immutable pack format emitted by
/// launcher-packs. It never trusts offsets, lengths, index order, or hashes
/// supplied by a remote provider.
/// </summary>
public sealed class PhysicalPackReader
{
    private const int HeaderSize = 64;
    private const int EntrySize = 96;
    private const int FooterSize = 72;
    private const ulong MaxPackBytes = 8UL * 1024 * 1024 * 1024;
    private const ulong MaxDeclaredChunkBytes = 2UL * 1024 * 1024 * 1024;
    private const uint ZstdCompression = 1;
    private static readonly byte[] HeaderMagic = "LGRPACK1"u8.ToArray();
    private static readonly byte[] FooterMagic = "LGRPFTR1"u8.ToArray();

    private readonly byte[] bytes;
    private readonly Dictionary<string, PhysicalPackEntry> entries;

    private PhysicalPackReader(byte[] bytes, IReadOnlyList<PhysicalPackEntry> entries)
    {
        this.bytes = bytes;
        this.entries = entries.ToDictionary(entry => entry.EncodedHash, StringComparer.Ordinal);
        Entries = entries;
    }

    public IReadOnlyList<PhysicalPackEntry> Entries { get; }

    public static PhysicalPackReader Parse(byte[] bytes, string? expectedPackHash = null)
    {
        ArgumentNullException.ThrowIfNull(bytes);
        if (bytes.Length < HeaderSize + FooterSize) throw new InvalidDataException("Pack is truncated.");
        if ((ulong)bytes.Length > MaxPackBytes) throw new InvalidDataException("Pack exceeds the format size limit.");
        if (!bytes.AsSpan(0, 8).SequenceEqual(HeaderMagic)) throw new InvalidDataException("Pack header magic is invalid.");
        var version = ReadUInt16(bytes, 8);
        if (version != 1) throw new InvalidDataException($"Unsupported pack format version {version}.");
        if (ReadUInt32(bytes, 12) != HeaderSize) throw new InvalidDataException("Pack header length is invalid.");
        var entryCount = ReadUInt64(bytes, 16);
        var dataOffset = ReadUInt64(bytes, 24);
        var indexOffset = ReadUInt64(bytes, 32);
        var indexLength = ReadUInt64(bytes, 40);
        if (dataOffset != HeaderSize) throw new InvalidDataException("Pack data offset is invalid.");
        if (entryCount > int.MaxValue || checked(entryCount * (ulong)EntrySize) != indexLength) throw new InvalidDataException("Pack index length is invalid.");

        var footerOffset = checked((ulong)bytes.Length - FooterSize);
        if (!bytes.AsSpan((int)footerOffset, 8).SequenceEqual(FooterMagic)) throw new InvalidDataException("Pack footer magic is invalid.");
        if (ReadUInt16(bytes, checked((int)footerOffset + 8)) != 1) throw new InvalidDataException("Pack footer version is invalid.");
        if (ReadUInt64(bytes, checked((int)footerOffset + 12)) != indexOffset
            || ReadUInt64(bytes, checked((int)footerOffset + 20)) != indexLength
            || ReadUInt64(bytes, checked((int)footerOffset + 28)) != entryCount)
            throw new InvalidDataException("Pack footer does not match its header.");
        if (checked(indexOffset + indexLength) != footerOffset || indexOffset < dataOffset) throw new InvalidDataException("Pack index bounds are invalid.");
        if (indexOffset > int.MaxValue || indexLength > int.MaxValue) throw new InvalidDataException("Pack index is too large.");
        var indexStart = (int)indexOffset;
        var indexBytes = bytes.AsSpan(indexStart, (int)indexLength);
        var expectedIndexHash = ToHex(bytes.AsSpan(checked((int)footerOffset + 40), 32));
        if (!string.Equals(Hashing.ComputeHash(indexBytes), expectedIndexHash, StringComparison.Ordinal)) throw new InvalidDataException("Pack index digest mismatch.");

        var parsed = new List<PhysicalPackEntry>(checked((int)entryCount));
        string? previousHash = null;
        for (var position = 0UL; position < entryCount; position++)
        {
            var entryStart = checked((int)(indexOffset + position * (ulong)EntrySize));
            var encodedHash = ToHex(bytes.AsSpan(entryStart, 32));
            var rawHash = ToHex(bytes.AsSpan(entryStart + 32, 32));
            ValidateHash(encodedHash);
            ValidateHash(rawHash);
            var entry = new PhysicalPackEntry(
                encodedHash,
                rawHash,
                ReadUInt64(bytes, entryStart + 64),
                ReadUInt64(bytes, entryStart + 72),
                ReadUInt64(bytes, entryStart + 80),
                ReadUInt32(bytes, entryStart + 88),
                ReadUInt32(bytes, entryStart + 92));
            if (entry.RawLength > MaxDeclaredChunkBytes || entry.EncodedLength > MaxDeclaredChunkBytes) throw new InvalidDataException("Pack entry declares an oversized chunk.");
            if (entry.Compression != ZstdCompression) throw new InvalidDataException($"Unsupported pack compression {entry.Compression}.");
            if (previousHash is not null && string.CompareOrdinal(previousHash, entry.EncodedHash) >= 0) throw new InvalidDataException("Pack entries are not strictly sorted.");
            previousHash = entry.EncodedHash;
            if (entry.Offset < dataOffset || checked(entry.Offset + entry.EncodedLength) > indexOffset) throw new InvalidDataException("Pack entry is outside the data region.");
            parsed.Add(entry);
        }
        foreach (var pair in parsed.OrderBy(entry => entry.Offset).Zip(parsed.OrderBy(entry => entry.Offset).Skip(1)))
        {
            if (checked(pair.First.Offset + pair.First.EncodedLength) > pair.Second.Offset) throw new InvalidDataException("Pack entries overlap.");
        }
        var reader = new PhysicalPackReader(bytes, parsed);
        if (expectedPackHash is not null)
        {
            ValidateHash(expectedPackHash);
            if (!string.Equals(Hashing.ComputeHash(bytes), expectedPackHash, StringComparison.Ordinal)) throw new InvalidDataException("Pack identity digest mismatch.");
        }
        return reader;
    }

    public byte[] ReadEncoded(string encodedHash)
    {
        ValidateHash(encodedHash);
        if (!entries.TryGetValue(encodedHash, out var entry)) throw new KeyNotFoundException($"Pack does not contain {encodedHash}.");
        var start = checked((int)entry.Offset);
        var length = checked((int)entry.EncodedLength);
        var chunk = bytes.AsSpan(start, length).ToArray();
        if (!string.Equals(Hashing.ComputeHash(chunk), entry.EncodedHash, StringComparison.Ordinal)) throw new InvalidDataException("Encoded chunk hash verification failed.");
        return chunk;
    }

    public byte[] ReadRaw(string encodedHash)
    {
        var entry = entries.TryGetValue(encodedHash, out var value) ? value : throw new KeyNotFoundException($"Pack does not contain {encodedHash}.");
        var raw = ZstdCodec.Decompress(ReadEncoded(encodedHash));
        if ((ulong)raw.Length != entry.RawLength || !string.Equals(Hashing.ComputeHash(raw), entry.RawHash, StringComparison.Ordinal)) throw new InvalidDataException("Raw chunk verification failed.");
        return raw;
    }

    private static ulong ReadUInt64(byte[] bytes, int offset) => BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(offset, 8));
    private static uint ReadUInt32(byte[] bytes, int offset) => BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(offset, 4));
    private static ushort ReadUInt16(byte[] bytes, int offset) => BinaryPrimitives.ReadUInt16LittleEndian(bytes.AsSpan(offset, 2));

    private static string ToHex(ReadOnlySpan<byte> bytes) => Convert.ToHexString(bytes).ToLowerInvariant();

    private static void ValidateHash(string value)
    {
        if (value.Length != 64 || value.Any(character => !Uri.IsHexDigit(character) || char.IsUpper(character))) throw new InvalidDataException($"Invalid lowercase BLAKE3 hash {value}.");
    }
}
