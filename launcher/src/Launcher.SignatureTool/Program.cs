using System.Text.Json;
using System.Security.Cryptography;
using Launcher.Manifests;
using Launcher.Security;

if (args.Length != 3 || !string.Equals(args[0], "verify", StringComparison.OrdinalIgnoreCase))
{
    Console.Error.WriteLine("usage: Launcher.SignatureTool verify <manifest.json> <manifest.sig.json>");
    return 2;
}

try
{
    var manifestBytes = await File.ReadAllBytesAsync(args[1]);
    var signature = JsonSerializer.Deserialize<ManifestSignature>(await File.ReadAllTextAsync(args[2]), ManifestJson.Options) ?? throw new InvalidDataException("signature envelope is empty");
    ManifestSignatureVerifier.Verify(manifestBytes, signature, allowEmbeddedPublicKey: true);
    Console.WriteLine("signature status=VALID");
    return 0;
}
catch (Exception error) when (error is IOException or JsonException or InvalidDataException or CryptographicException)
{
    Console.Error.WriteLine($"signature status=INVALID reason={error.Message}");
    return 1;
}
