using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text;

namespace Launcher.App.Runtime;

public sealed record SteamOpenIdAssertion(IReadOnlyDictionary<string, string> Parameters);

public static class SteamAccountConnector
{
    private static readonly TimeSpan CallbackTimeout = TimeSpan.FromMinutes(3);
    private const string OpenIdProvider = "https://steamcommunity.com/openid/login";
    private const string OpenIdNamespace = "http://specs.openid.net/auth/2.0";
    private const string IdentifierSelect = "http://specs.openid.net/auth/2.0/identifier_select";

    public static async Task<SteamOpenIdAssertion> AuthorizeAsync(CancellationToken cancellationToken = default)
    {
        var port = FindFreeLoopbackPort();
        var prefix = $"http://127.0.0.1:{port}/";
        var callback = new Uri(new Uri(prefix), "steam-callback/");
        using var listener = new HttpListener();
        listener.Prefixes.Add(prefix);

        try
        {
            listener.Start();
        }
        catch (HttpListenerException error)
        {
            throw new Launcher.Core.LauncherOperationException(
                "Vaultnode could not start the local Steam sign-in callback.",
                error);
        }

        try
        {
            OpenBrowser(BuildLoginUri(callback, prefix));
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            timeout.CancelAfter(CallbackTimeout);
            var context = await listener.GetContextAsync().WaitAsync(timeout.Token).ConfigureAwait(false);
            var parameters = ReadParameters(context.Request);
            await WriteResponseAsync(context.Response, parameters.ContainsKey("openid.mode")
                ? "Steam sign-in received. You can return to Vaultnode Launcher."
                : "Steam did not return a sign-in response. You can close this tab.",
                timeout.Token).ConfigureAwait(false);

            if (string.Equals(parameters.GetValueOrDefault("openid.mode"), "cancel", StringComparison.OrdinalIgnoreCase))
            {
                throw new Launcher.Core.LauncherOperationException("Steam sign-in was cancelled.");
            }

            if (!string.Equals(parameters.GetValueOrDefault("openid.mode"), "id_res", StringComparison.OrdinalIgnoreCase))
            {
                throw new Launcher.Core.LauncherOperationException("Steam returned an incomplete sign-in response.");
            }

            if (SteamLibraryDiscovery.TryGetSteamId64FromClaimedId(parameters.GetValueOrDefault("openid.claimed_id")) is null)
            {
                throw new Launcher.Core.LauncherOperationException("Steam returned an invalid account identifier.");
            }

            return new SteamOpenIdAssertion(parameters);
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            throw new Launcher.Core.LauncherOperationException("Steam sign-in timed out. Try connecting again.");
        }
        catch (HttpListenerException error)
        {
            throw new Launcher.Core.LauncherOperationException("Steam sign-in could not receive its callback.", error);
        }
        finally
        {
            listener.Stop();
        }
    }

    private static Uri BuildLoginUri(Uri callback, string realm)
    {
        var values = new Dictionary<string, string>(StringComparer.Ordinal)
        {
            ["openid.ns"] = OpenIdNamespace,
            ["openid.mode"] = "checkid_setup",
            ["openid.return_to"] = callback.AbsoluteUri,
            ["openid.realm"] = realm,
            ["openid.identity"] = IdentifierSelect,
            ["openid.claimed_id"] = IdentifierSelect
        };
        var query = string.Join(
            "&",
            values.Select(pair => $"{Uri.EscapeDataString(pair.Key)}={Uri.EscapeDataString(pair.Value)}"));
        return new Uri($"{OpenIdProvider}?{query}");
    }

    private static Dictionary<string, string> ReadParameters(HttpListenerRequest request)
    {
        var parameters = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var key in request.QueryString.AllKeys)
        {
            if (key is null || !key.StartsWith("openid.", StringComparison.Ordinal)
                || request.QueryString[key] is not { } value
                || value.Length > 8192)
            {
                continue;
            }

            parameters[key] = value;
        }

        return parameters;
    }

    private static async Task WriteResponseAsync(
        HttpListenerResponse response,
        string message,
        CancellationToken cancellationToken)
    {
        const string closingPage = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Vaultnode</title></head>"
            + "<body style=\"font-family:Segoe UI,sans-serif;background:#101722;color:#e8eef7;padding:32px\">"
            + "<h2>Vaultnode</h2><p>";
        var body = Encoding.UTF8.GetBytes(closingPage + WebUtility.HtmlEncode(message) + "</p></body></html>");
        response.StatusCode = (int)HttpStatusCode.OK;
        response.ContentType = "text/html; charset=utf-8";
        response.ContentLength64 = body.Length;
        await response.OutputStream.WriteAsync(body, cancellationToken).ConfigureAwait(false);
        response.Close();
    }

    private static void OpenBrowser(Uri uri)
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = uri.AbsoluteUri,
                UseShellExecute = true
            });
            if (process is not null) return;
        }
        catch (ArgumentException)
        {
            // Convert to one actionable launcher error below.
        }
        catch (InvalidOperationException)
        {
            // Convert to one actionable launcher error below.
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // Convert to one actionable launcher error below.
        }

        throw new Launcher.Core.LauncherOperationException("Vaultnode could not open a browser for Steam sign-in.");
    }

    private static int FindFreeLoopbackPort()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        return ((IPEndPoint)listener.LocalEndpoint).Port;
    }
}
