using System.Diagnostics;
using BatteryLife.Logic;

namespace BatteryLife.App;

/// <summary>
/// Runs the Rust core (batterylife-core --watch) as a child process and raises Updated for every
/// device list it prints. The core is a separate process on purpose: a driver that crashes takes
/// down only the core, which is restarted with a backoff, never the app.
/// </summary>
public sealed class CoreClient : IDisposable
{
    private const int IntervalSeconds = 60;
    private readonly string _exe;
    private readonly CancellationTokenSource _stop = new();
    private Process? _process;

    /// <summary>Raised on a background thread with each fresh device list.</summary>
    public event Action<IReadOnlyList<DeviceReading>>? Updated;
    /// <summary>Raised on a background thread when the core cannot be started at all.</summary>
    public event Action<string>? Failed;

    public CoreClient(string exe) => _exe = exe;

    public void Start() => _ = Task.Run(RunAsync);

    /// <summary>Asks the core to poll now (device plugged in, PC woke up, user clicked refresh).</summary>
    public void Refresh()
    {
        try
        {
            var p = _process;
            if (p is { HasExited: false }) p.StandardInput.WriteLine("refresh");
        }
        catch (Exception e) when (e is IOException or InvalidOperationException)
        {
            // The core is restarting; the next run polls immediately anyway.
        }
    }

    private async Task RunAsync()
    {
        var delay = TimeSpan.FromSeconds(1);
        while (!_stop.IsCancellationRequested)
        {
            if (!File.Exists(_exe))
            {
                Failed?.Invoke($"batterylife-core.exe not found next to the app ({_exe})");
                return;
            }
            var started = DateTime.UtcNow;
            try
            {
                using var p = Process.Start(new ProcessStartInfo(_exe, $"--watch {IntervalSeconds}")
                {
                    RedirectStandardInput = true, // the core exits when this pipe closes
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                    UseShellExecute = false,
                    CreateNoWindow = true,
                })!;
                _process = p;
                p.ErrorDataReceived += (_, e) => { if (e.Data is { } line) Debug.WriteLine($"core: {line}"); };
                p.BeginErrorReadLine();

                while (await p.StandardOutput.ReadLineAsync(_stop.Token) is { } line)
                {
                    if (CoreProtocol.ParseLine(line) is { } devices) Updated?.Invoke(devices);
                }
                await p.WaitForExitAsync(_stop.Token);
            }
            catch (OperationCanceledException)
            {
                return;
            }
            catch (Exception e) when (e is System.ComponentModel.Win32Exception or IOException)
            {
                Debug.WriteLine($"core failed: {e.Message}");
            }
            finally
            {
                _process = null;
            }

            // Crashed or exited: restart, backing off up to a minute if it keeps dying quickly.
            delay = DateTime.UtcNow - started > TimeSpan.FromMinutes(5)
                ? TimeSpan.FromSeconds(1)
                : TimeSpan.FromSeconds(Math.Min(delay.TotalSeconds * 2, 60));
            try { await Task.Delay(delay, _stop.Token); } catch (OperationCanceledException) { return; }
        }
    }

    public void Dispose()
    {
        _stop.Cancel();
        try
        {
            var p = _process;
            if (p is { HasExited: false })
            {
                p.StandardInput.Close(); // asks the core to exit cleanly
                if (!p.WaitForExit(2000)) p.Kill();
            }
        }
        catch (Exception e) when (e is InvalidOperationException or IOException or System.ComponentModel.Win32Exception)
        {
        }
    }
}
