using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Net.Http;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;

namespace F7Whisper;

internal sealed class Updater
{
    private const string Latest = "https://api.github.com/repos/NoBVR/F-7WHISPER/releases/latest";
    private const string Zip = "F-7WHISPER-windows-x64.zip";
    private const string Exe = "F-7WHISPER.exe";
    private sealed record Asset(string Version, string Url, string? Digest, long? Size);

    private readonly HttpClient _http = new();
    private readonly CancellationTokenSource _stop = new();
    private readonly string _running;
    private readonly Action<object> _post;
    private readonly Action _close;
    private Asset? _known;
    private object? _last;
    private bool _started;
    private bool _busy;

    internal Updater(string running, Action<object> post, Action close)
    {
        _running = running;
        _post = post;
        _close = close;
        _http.DefaultRequestHeaders.UserAgent.ParseAdd($"F-7WHISPER/{running}");

        // The restarted process can remove the helper once Windows releases its image.
        var args = Environment.GetCommandLineArgs();
        if (args.Length is 4 or 5 && args[1] == "--update-result" &&
            Guid.TryParseExact(args[2], "N", out var id) && int.TryParse(args[3], out int helperPid))
        {
            _ = Task.Run(() =>
            {
                try { using var helper = Process.GetProcessById(helperPid); helper.WaitForExit(30000); }
                catch (ArgumentException) { } // Already exited.
                Cleanup(Root(id));
            });
            if (args.Length == 5)
            {
                try
                {
                    var asset = JsonSerializer.Deserialize<Asset>(args[4]);
                    if (asset != null && Numbers(asset.Version) > Numbers(running) &&
                        Uri.TryCreate(asset.Url, UriKind.Absolute, out var uri) &&
                        uri.Scheme == "https" && uri.IsDefaultPort && uri.Host == "github.com" &&
                        uri.AbsolutePath.StartsWith("/NoBVR/F-7WHISPER/releases/download/", StringComparison.Ordinal) &&
                        uri.AbsolutePath.EndsWith("/" + Zip, StringComparison.Ordinal))
                    {
                        _known = asset;
                        _last = Message(asset, "failed", 0);
                    }
                }
                catch (Exception e) { Log(e); }
            }
        }
    }

    private static Version Numbers(string s) =>
        Regex.IsMatch(s, @"\A[0-9]+\.[0-9]+\.[0-9]+\z") ? Version.Parse(s) : throw new FormatException(s);

    private static object Message(Asset a, string status, double progress) =>
        new { type = "update", version = a.Version, status, progress };

    private void Publish(Asset a, string status, double progress = 0)
    {
        _last = Message(a, status, progress);
        if (!_stop.IsCancellationRequested) _post(_last);
    }

    internal void Ready()
    {
        if (_last != null) _post(_last);
        if (_started) return;
        _started = true;
        _ = Poll();
    }

    internal void Stop()
    {
        _stop.Cancel();
        _http.Dispose();
    }

    private async Task Poll()
    {
        var token = _stop.Token;
        try
        {
            await Task.Delay(TimeSpan.FromSeconds(3), token);
            while (!token.IsCancellationRequested)
            {
                try
                {
                    var asset = await Task.Run(() => Check(token), token);
                    if (!_busy && !token.IsCancellationRequested)
                    {
                        bool changed = _known?.Version != asset?.Version;
                        _known = asset;
                        if (asset == null) _last = null;
                        else if (changed) Publish(asset, "available");
                    }
                }
                catch (Exception e) when (!token.IsCancellationRequested) { Log(e); }
                await Task.Delay(TimeSpan.FromHours(6), token);
            }
        }
        catch (Exception e) { if (!token.IsCancellationRequested) Log(e); }
    }

    private async Task<Asset?> Check(CancellationToken token)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, Latest);
        request.Headers.Accept.ParseAdd("application/vnd.github+json");
        using var response = await _http.SendAsync(request, token);
        response.EnsureSuccessStatusCode();
        using var doc = JsonDocument.Parse(await response.Content.ReadAsByteArrayAsync(token));
        var release = doc.RootElement;
        string tag = release.GetProperty("tag_name").GetString()!;
        string version = tag.StartsWith('v') ? tag[1..] : tag;
        if (Numbers(version) <= Numbers(_running)) return null;
        foreach (var file in release.GetProperty("assets").EnumerateArray())
        {
            if (file.GetProperty("name").GetString() != Zip) continue;
            string url = file.GetProperty("browser_download_url").GetString()!;
            if (!Uri.TryCreate(url, UriKind.Absolute, out var uri) || uri.Scheme != "https")
                throw new InvalidDataException("Invalid release asset URL.");
            return new Asset(version, url, file.Field("digest").Text(),
                file.TryGetProperty("size", out var size) ? size.GetInt64() : null);
        }
        return null;
    }

    internal async void Install()
    {
        if (_busy || _known is not { } asset) return;
        _busy = true;
        var id = Guid.NewGuid();
        string root = Root(id);
        bool handedOff = false;
        try
        {
            Publish(asset, "downloading");
            var progress = new Progress<double>(p => Publish(asset, "downloading", p));
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(_stop.Token);
            timeout.CancelAfter(TimeSpan.FromMinutes(30));
            await Task.Run(() => Prepare(root, asset, progress, timeout.Token));
            timeout.Token.ThrowIfCancellationRequested();
            Publish(asset, "installing");
            await Task.Run(() => StartHelper(id, asset));
            handedOff = true;
            if (!_stop.IsCancellationRequested) _close();
        }
        catch (Exception e)
        {
            Log(e);
            if (!_stop.IsCancellationRequested) Publish(asset, "failed");
        }
        finally
        {
            if (!handedOff) _ = Task.Run(() => Cleanup(root));
            _busy = handedOff;
        }
    }

    private async Task Prepare(string root, Asset asset, IProgress<double> progress, CancellationToken token)
    {
        string exe = Environment.ProcessPath ?? throw new IOException("Missing executable path.");
        if (Path.GetFileName(exe).Equals("dotnet.exe", StringComparison.OrdinalIgnoreCase))
            throw new IOException("Start the installed F-7WHISPER executable to update it.");
        Directory.CreateDirectory(root);
        string zip = Path.Combine(root, Zip);
        using var response = await _http.GetAsync(asset.Url, HttpCompletionOption.ResponseHeadersRead, token);
        response.EnsureSuccessStatusCode();
        long? length = response.Content.Headers.ContentLength;
        using (var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256))
        using (var input = await response.Content.ReadAsStreamAsync(token))
        using (var output = File.Create(zip))
        {
            var clock = Stopwatch.StartNew();
            byte[] buffer = new byte[65536];
            long bytes = 0;
            int count;
            while ((count = await input.ReadAsync(buffer.AsMemory(), token)) != 0)
            {
                await output.WriteAsync(buffer.AsMemory(0, count), token);
                hash.AppendData(buffer, 0, count);
                bytes += count;
                long total = length ?? asset.Size ?? 0;
                if (total > 0 && clock.ElapsedMilliseconds >= 100)
                {
                    progress.Report(Math.Min(1, (double)bytes / total));
                    clock.Restart();
                }
            }
            if ((length.HasValue && length != bytes) || (asset.Size.HasValue && asset.Size != bytes))
                throw new InvalidDataException("Incomplete update archive.");
            if (asset.Digest is { } digest && digest.StartsWith("sha256:", StringComparison.Ordinal) &&
                !Convert.ToHexString(hash.GetHashAndReset()).Equals(digest[7..], StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("SHA-256 mismatch.");
        }

        progress.Report(1);
        string stage = Path.Combine(root, "stage");
        ZipFile.ExtractToDirectory(zip, stage);
        if (!File.Exists(Path.Combine(stage, Exe))) throw new InvalidDataException($"Missing {Exe}.");
        token.ThrowIfCancellationRequested();

        // Run the current updater outside the installation, including loose runtime files
        // when using an unpacked apphost. The published single-file app needs only its exe.
        string helper = Directory.CreateDirectory(Path.Combine(root, "helper")).FullName;
        File.Copy(exe, Path.Combine(helper, Exe));
        if (!string.IsNullOrEmpty(typeof(Updater).Assembly.Location))
        {
            foreach (var file in Directory.EnumerateFiles(Path.GetDirectoryName(exe)!))
                if (file.EndsWith(".dll", StringComparison.OrdinalIgnoreCase) ||
                    file.EndsWith(".runtimeconfig.json", StringComparison.OrdinalIgnoreCase) ||
                    file.EndsWith(".deps.json", StringComparison.OrdinalIgnoreCase))
                    File.Copy(file, Path.Combine(helper, Path.GetFileName(file)));
        }
    }

    private static string Root(Guid id) => Path.Combine(Path.GetTempPath(), $"F-7WHISPER-{id:N}");
    private static void Log(Exception e) => Trace.TraceWarning("Update: {0}", e);

    private static void Retry(Action operation)
    {
        var clock = Stopwatch.StartNew();
        while (true)
        {
            try { operation(); return; }
            catch (Exception e) when ((e is IOException or UnauthorizedAccessException) && clock.Elapsed.TotalSeconds < 6)
            {
                Thread.Sleep(175);
            }
        }
    }

    private static void Cleanup(string root)
    {
        try { if (Directory.Exists(root)) Retry(() => Directory.Delete(root, true)); }
        catch (Exception e) { Log(e); }
    }

    private static void StartHelper(Guid id, Asset asset)
    {
        string helper = Path.Combine(Root(id), "helper", Exe);
        var info = new ProcessStartInfo(helper)
        {
            WorkingDirectory = Path.GetDirectoryName(helper)!,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardInput = true,
            ArgumentList = { "--apply-update", id.ToString("N"), Environment.ProcessId.ToString(), JsonSerializer.Serialize(asset) }
        };
        using var process = Process.Start(info) ?? throw new IOException("Could not start update helper.");
        using var command = process.StandardInput;
        // The helper opens the parent process before the form proceeds with shutdown.
        if (process.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(20)).GetAwaiter().GetResult() != "ready")
            throw new IOException("Update helper did not start.");
        command.WriteLine("apply");
    }

    internal static bool RunHelper(string[] args)
    {
        if (args.Length == 0 || args[0] != "--apply-update") return false;
        try
        {
            if (args.Length != 4 || !Guid.TryParseExact(args[1], "N", out var id)) return true;
            string root = Root(id);
            if (!string.Equals(Environment.ProcessPath, Path.Combine(root, "helper", Exe), StringComparison.OrdinalIgnoreCase))
                return true;
            using var parent = Process.GetProcessById(int.Parse(args[2]));
            _ = parent.SafeHandle; // Keep this process identity through shutdown.
            string exe = parent.MainModule?.FileName ?? throw new IOException("Missing parent path.");
            string target = Path.GetDirectoryName(exe)!;
            Console.WriteLine("ready");
            if (Console.ReadLine() != "apply") return true;
            parent.WaitForExit();

            var restart = new ProcessStartInfo(exe)
            {
                UseShellExecute = false, WorkingDirectory = target,
                ArgumentList = { "--update-result", args[1], Environment.ProcessId.ToString() }
            };
            try { ReplaceFiles(Path.Combine(root, "stage"), exe, id, () => Launch(restart)); }
            catch (Exception e)
            {
                Log(e);
                restart.ArgumentList.Add(args[3]);
                Launch(restart);
            }
        }
        catch (Exception e) { Log(e); }
        return true;
    }

    private static void Launch(ProcessStartInfo info)
    {
        using var app = Process.Start(info) ?? throw new IOException("Could not relaunch F-7WHISPER.");
    }

    private static void ReplaceFiles(string stage, string exe, Guid id, Action launch)
    {
        string target = Path.GetDirectoryName(exe)!;
        var changes = new Stack<(string Destination, string? Backup)>();
        try
        {
            string stagedExe = Path.Combine(stage, Exe);
            var files = Directory.EnumerateFiles(stage, "*", SearchOption.AllDirectories);
            foreach (var source in files.OrderBy(f => f.Equals(stagedExe, StringComparison.OrdinalIgnoreCase)))
            {
                string name = Path.GetRelativePath(stage, source);
                string dest = name.Equals(Exe, StringComparison.OrdinalIgnoreCase) ? exe : Path.Combine(target, name);
                Directory.CreateDirectory(Path.GetDirectoryName(dest)!);
                string incoming = dest + $".{id:N}.new";
                string? backup = File.Exists(dest) ? dest + $".{id:N}.bak" : null;
                try
                {
                    Retry(() => File.Copy(source, incoming, true));
                    if (backup == null)
                    {
                        Retry(() => File.Move(incoming, dest));
                        changes.Push((dest, null));
                    }
                    else
                    {
                        Retry(() => File.Move(dest, backup));
                        changes.Push((dest, backup));
                        Retry(() => File.Move(incoming, dest));
                    }
                }
                finally { File.Delete(incoming); }
            }
            launch();
        }
        catch
        {
            foreach (var (dest, backup) in changes)
            {
                try
                {
                    if (backup == null) Retry(() => File.Delete(dest));
                    else Retry(() => File.Move(backup, dest, true));
                }
                catch (Exception e) { Log(e); }
            }
            throw;
        }
        foreach (var (_, backup) in changes)
            if (backup != null)
                try { Retry(() => File.Delete(backup)); } catch (Exception e) { Log(e); }
    }
}
