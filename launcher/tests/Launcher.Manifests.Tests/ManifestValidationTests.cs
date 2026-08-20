using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Launcher.Manifests;
using Launcher.Security;

namespace Launcher.Manifests.Tests;

public sealed class ManifestValidationTests
{
    [Fact]
    public void ValidManifestPasses()
    {
        ManifestValidator.Validate(CreateManifest());
    }

    [Theory]
    [InlineData("../evil.exe")]
    [InlineData("../../Windows/System32/test")]
    [InlineData("C:/Windows/test")]
    [InlineData("C:\\Windows\\test")]
    [InlineData("\\\\server\\share\\file")]
    [InlineData("/absolute/path")]
    [InlineData("mixed\\separators/file")]
    [InlineData("CON/file.bin")]
    [InlineData("folder/trailing. ")]
    [InlineData("folder/../file.bin")]
    public void PathAttacksAreRejected(string path)
    {
        var manifest = CreateManifest(path: path);
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(manifest));
    }

    [Fact]
    public void UnsupportedVersionAndMissingFieldsAreRejected()
    {
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(CreateManifest() with { SchemaVersion = 99 }));
        var missing = JsonSerializer.Deserialize<Manifest>("{\"schema_version\":1}", ManifestJson.Options)!;
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(missing));
        Assert.Throws<JsonException>(() => ManifestJson.Deserialize("{\"schema_version\":1,"));
    }

    [Fact]
    public void DuplicatePathsAndImpossibleChunksAreRejected()
    {
        var valid = CreateManifest();
        var duplicate = valid with { Files = new[] { valid.Files[0], valid.Files[0] } };
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(duplicate));
        var caseOnlyDuplicate = valid with { Files = new[] { valid.Files[0], valid.Files[0] with { Path = "syntheticgame.exe" } } };
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(caseOnlyDuplicate));
        var impossible = valid with { Files = new[] { valid.Files[0] with { Chunks = new[] { valid.Files[0].Chunks[0] with { RawSize = 0 } } } } };
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(impossible));
        var conflicting = valid with { Files = new[] { valid.Files[0] with { Chunks = new[] { valid.Files[0].Chunks[0], valid.Files[0].Chunks[0] with { RawSize = 2, EncodedSize = 2 } }, Size = 3 } } };
        Assert.Throws<InvalidDataException>(() => ManifestValidator.Validate(conflicting));
    }

    [Fact]
    public void ValidSignatureVerifiesAndTamperingDoesNot()
    {
        var bytes = Encoding.UTF8.GetBytes(ManifestJson.Serialize(CreateManifest()));
        using var rsa = RSA.Create(2048);
        var signatureBytes = rsa.SignData(bytes, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        var signature = new ManifestSignature(1, "rsa-sha256-pkcs1-v1_5", "test-key", Hashing.ComputeHash(bytes), Convert.ToBase64String(signatureBytes), null);
        var publicPem = rsa.ExportSubjectPublicKeyInfoPem();
        ManifestSignatureVerifier.Verify(bytes, signature, new Dictionary<string, string> { ["test-key"] = publicPem });
        Assert.Throws<InvalidDataException>(() => ManifestSignatureVerifier.Verify(bytes.AsSpan(0, bytes.Length - 1), signature, new Dictionary<string, string> { ["test-key"] = publicPem }));
    }

    [Fact]
    public void WrongKeyMalformedSignatureWrongIdAndMissingSignatureAreRejected()
    {
        var bytes = Encoding.UTF8.GetBytes(ManifestJson.Serialize(CreateManifest()));
        using var signer = RSA.Create(2048);
        using var wrongKey = RSA.Create(2048);
        var signature = new ManifestSignature(1, "rsa-sha256-pkcs1-v1_5", "test-key", Hashing.ComputeHash(bytes), Convert.ToBase64String(signer.SignData(bytes, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1)), null);
        Assert.Throws<InvalidDataException>(() => ManifestSignatureVerifier.Verify(bytes, signature, new Dictionary<string, string> { ["test-key"] = wrongKey.ExportSubjectPublicKeyInfoPem() }));
        Assert.Throws<InvalidDataException>(() => ManifestSignatureVerifier.Verify(bytes, signature with { SignatureBase64 = "not-base64" }, new Dictionary<string, string> { ["test-key"] = signer.ExportSubjectPublicKeyInfoPem() }));
        Assert.Throws<InvalidDataException>(() => ManifestSignatureVerifier.Verify(bytes, signature with { KeyId = "other-key" }, new Dictionary<string, string> { ["test-key"] = signer.ExportSubjectPublicKeyInfoPem() }));
        Assert.Throws<InvalidDataException>(() => ManifestSignatureVerifier.Verify(bytes, signature with { SignatureBase64 = "" }, new Dictionary<string, string> { ["test-key"] = signer.ExportSubjectPublicKeyInfoPem() }));
        Assert.Throws<InvalidDataException>(() => ManifestSignatureVerifier.Verify(bytes, signature with { ManifestBlake3 = "" }, new Dictionary<string, string> { ["test-key"] = signer.ExportSubjectPublicKeyInfoPem() }));
    }

    private static Manifest CreateManifest(string path = "SyntheticGame.exe")
    {
        const string hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const string encoded = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        var chunk = new ChunkReference(hash, 1, encoded, 1, $"chunks/encoded/{encoded}.bin");
        return new Manifest(1, "manifest", "synthetic-game", "build-a", "A", DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, new[] { new FileRecipe(path, 1, hash, new[] { chunk }) }, new LaunchProfile(path, ".", Array.Empty<string>(), new Dictionary<string, string>()));
    }
}
