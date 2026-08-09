using System.IO.Pipes;
using System.Text;

namespace Launcher.App;

internal sealed class SingleInstanceCoordinator : IDisposable
{
    private readonly Mutex _mutex;
    private readonly string _pipeName;
    private readonly CancellationTokenSource _cancellation = new();
    private Task? _listener;

    private SingleInstanceCoordinator(Mutex mutex, string pipeName)
    {
        _mutex = mutex;
        _pipeName = pipeName;
    }

    public static bool TryAcquire(string name, out SingleInstanceCoordinator? coordinator)
    {
        var mutex = new Mutex(true, $"Local\\{name}", out var createdNew);
        if (createdNew)
        {
            coordinator = new SingleInstanceCoordinator(mutex, name + ".Activation");
            return true;
        }
        coordinator = null;
        mutex.Dispose();
        TrySignal(name + ".Activation");
        return false;
    }

    public void Start(Action activation)
    {
        _listener = Task.Run(async () =>
        {
            while (!_cancellation.IsCancellationRequested)
            {
                try
                {
                    await using var server = new NamedPipeServerStream(_pipeName, PipeDirection.In, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
                    await server.WaitForConnectionAsync(_cancellation.Token).ConfigureAwait(false);
                    using var reader = new StreamReader(server, Encoding.UTF8, leaveOpen: false);
                    _ = await reader.ReadLineAsync(_cancellation.Token).ConfigureAwait(false);
                    activation();
                }
                catch (OperationCanceledException) when (_cancellation.IsCancellationRequested) { return; }
                catch (IOException) when (!_cancellation.IsCancellationRequested) { }
            }
        }, _cancellation.Token);
    }

    public void Dispose()
    {
        _cancellation.Cancel();
        try { _listener?.Wait(TimeSpan.FromSeconds(1)); } catch (AggregateException) { }
        _cancellation.Dispose();
        _mutex.ReleaseMutex();
        _mutex.Dispose();
    }

    private static void TrySignal(string pipeName)
    {
        try
        {
            using var client = new NamedPipeClientStream(".", pipeName, PipeDirection.Out, PipeOptions.Asynchronous);
            client.Connect(250);
            using var writer = new StreamWriter(client, Encoding.UTF8, leaveOpen: false) { AutoFlush = true };
            writer.WriteLine("activate");
        }
        catch (IOException) { }
    }
}
