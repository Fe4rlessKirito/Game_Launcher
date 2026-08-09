using System.Security.Cryptography;
using Launcher.Manifests;

namespace Launcher.Security;

public static class ManifestSignatureVerifier
{
    public static void Verify(
        ReadOnlySpan<byte> manifestBytes,
        ManifestSignature signature,
        IReadOnlyDictionary<string, string>? trustedPublicKeysPem = null,
        bool allowEmbeddedPublicKey = false)
    {
        ArgumentNullException.ThrowIfNull(signature);
        if (signature.SchemaVersion != 1 || !string.Equals(signature.Algorithm, "rsa-sha256-pkcs1-v1_5", StringComparison.Ordinal) || string.IsNullOrWhiteSpace(signature.KeyId)) throw new InvalidDataException("Unsupported or incomplete manifest signature.");
        if (!string.Equals(Hashing.ComputeHash(manifestBytes), signature.ManifestBlake3, StringComparison.Ordinal)) throw new InvalidDataException("Manifest digest does not match its signature envelope.");

        string? pem = null;
        if (trustedPublicKeysPem is not null) trustedPublicKeysPem.TryGetValue(signature.KeyId, out pem);
        if (pem is null && allowEmbeddedPublicKey) pem = ToPem(signature.PublicKeyBase64);
        if (pem is null) throw new InvalidDataException($"Manifest signature key ID is not trusted: {signature.KeyId}");

        byte[] encodedSignature;
        try { encodedSignature = Convert.FromBase64String(signature.SignatureBase64); }
        catch (FormatException error) { throw new InvalidDataException("Manifest signature is not valid base64.", error); }

        using var rsa = RSA.Create();
        try { rsa.ImportFromPem(pem); }
        catch (Exception error) when (error is CryptographicException or ArgumentException) { throw new InvalidDataException("Manifest public key is malformed.", error); }
        if (!rsa.VerifyData(manifestBytes, encodedSignature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1)) throw new InvalidDataException("Manifest signature verification failed.");
    }

    private static string? ToPem(string? derBase64)
    {
        if (string.IsNullOrWhiteSpace(derBase64)) return null;
        try
        {
            var der = Convert.FromBase64String(derBase64);
            return $"-----BEGIN PUBLIC KEY-----\n{Convert.ToBase64String(der, Base64FormattingOptions.InsertLineBreaks)}\n-----END PUBLIC KEY-----";
        }
        catch (FormatException error) { throw new InvalidDataException("Embedded manifest public key is not valid base64.", error); }
    }
}
