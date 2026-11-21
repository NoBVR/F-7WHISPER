using System;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Windows.Forms;
using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;

namespace F7Whisper;

internal sealed class MainForm : Form
{
    private const string AppHost = "app.f7whisper.local";
    private const string PageUrl = "https://app.f7whisper.local/index.html";
    private const int HotkeyId = 7;
    private static readonly Color Background = Color.FromArgb(10, 10, 10);

    private readonly WebView2 _web = new();
    private readonly Timer _countTimer = new() { Interval = 200 };
    private readonly Timer _saveTimer = new() { Interval = 500 };
    private readonly KeyEngine _engine;
    private readonly Rectangle _initialBounds;
    private readonly bool _startMaximized;

    private bool _trackWindow;
    private bool _restoreMaximized;
    private bool _ready;
    private bool _hotkeyOk;
    private bool _closing;
    private bool _disposed;
    private int _sizeKind = -1;

    internal MainForm()
    {
        var settings = Settings.Load();
        _engine = new KeyEngine(settings.Rows);
        (_initialBounds, _startMaximized) = InitialPlacement(settings.Window);

        Text = "F-7WHISPER";
        using (var iconStream = typeof(MainForm).Assembly.GetManifestResourceStream("F7Whisper.app.ico"))
        {
            if (iconStream is not null) Icon = new Icon(iconStream);
        }
        BackColor = Background;
        AutoScaleMode = AutoScaleMode.None;
        StartPosition = FormStartPosition.Manual;

        // Sizable supplies the caption, sizing frame, system menu and min/max styles.
        // WM_NCCALCSIZE below gives their drawing area back to the page.
        FormBorderStyle = FormBorderStyle.Sizable;
        MinimizeBox = true;
        MaximizeBox = true;
        Bounds = _initialBounds;
        TopMost = settings.Pinned;

        _web.DefaultBackgroundColor = Background;
        _web.Dock = DockStyle.Fill;
        Controls.Add(_web);

        _countTimer.Tick += (_, _) =>
        {
            if (_ready && !_closing && _engine.TakeChangedCounts() is { } counts)
                Post(new { type = "counts", counts });
        };
        _saveTimer.Tick += (_, _) =>
        {
            _saveTimer.Stop();
            SaveSettings();
        };
    }

    private static (Rectangle Bounds, bool Maximized) InitialPlacement(SavedWindow? saved)
    {
        if (saved is { Width: > 0, Height: > 0 } &&
            (long)saved.X + saved.Width <= int.MaxValue &&
            (long)saved.Y + saved.Height <= int.MaxValue)
        {
            var bounds = new Rectangle(saved.X, saved.Y, saved.Width, saved.Height);
            if (Screen.AllScreens.Any(s => s.WorkingArea.IntersectsWith(bounds)))
                return (bounds, saved.Maximized);
        }

        var area = (Screen.PrimaryScreen ?? Screen.AllScreens[0]).WorkingArea;
        uint dpi = Native.GetDpiForSystem();
        int width = Native.Scale(720, dpi);
        int height = Native.Scale(340, dpi);
        return (new Rectangle(area.Left + (area.Width - width) / 2,
                              area.Top + (area.Height - height) / 2, width, height), false);
    }

    protected override void OnHandleCreated(EventArgs e)
    {
        base.OnHandleCreated(e);
        Native.ApplyFrame(Handle);
        _engine.SetOwner(Handle);
        _hotkeyOk = Native.RegisterHotKey(Handle, HotkeyId, Native.ModNoRepeat, Native.VkF7);
        SendState();
    }

    protected override void OnHandleDestroyed(EventArgs e)
    {
        if (_hotkeyOk)
        {
            Native.UnregisterHotKey(Handle, HotkeyId);
            _hotkeyOk = false;
        }

        if (!_closing)
            _engine.SetOwner(0);

        base.OnHandleDestroyed(e);
    }

    protected override void OnLoad(EventArgs e)
    {
        // The handle is now on the destination monitor and has that monitor's DPI.
        uint dpi = (uint)DeviceDpi;
        Bounds = new Rectangle(_initialBounds.Location, new Size(
            Math.Max(_initialBounds.Width, Native.Scale(560, dpi)),
            Math.Max(_initialBounds.Height, Native.Scale(260, dpi))));

        _trackWindow = true;
        _restoreMaximized = _startMaximized;
        if (_startMaximized)
            WindowState = FormWindowState.Maximized;

        base.OnLoad(e);
    }

    protected override async void OnShown(EventArgs e)
    {
        base.OnShown(e);
        try
        {
            string profile = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "F-7WHISPER", "WebView2");

            var environment = await CoreWebView2Environment.CreateAsync(userDataFolder: profile);
            if (_closing || IsDisposed)
                return;

            await _web.EnsureCoreWebView2Async(environment);
            if (_closing || IsDisposed)
                return;

            var core = _web.CoreWebView2
                ?? throw new InvalidOperationException("WebView2 initialization did not complete.");
            var options = core.Settings;
            options.AreDefaultContextMenusEnabled = false;
            options.IsZoomControlEnabled = false;
            options.IsStatusBarEnabled = false;
            options.AreBrowserAcceleratorKeysEnabled = false;
#if DEBUG
            options.AreDevToolsEnabled = true;
#else
            options.AreDevToolsEnabled = false;
#endif
            core.SetVirtualHostNameToFolderMapping(
                AppHost, Path.Combine(AppContext.BaseDirectory, "ui"),
                CoreWebView2HostResourceAccessKind.Deny);

            core.NavigationStarting += (_, args) =>
            {
                if (!string.Equals(args.Uri, PageUrl, StringComparison.Ordinal))
                    args.Cancel = true;
                else
                    _ready = false;
            };
            core.NewWindowRequested += (_, args) => args.Handled = true;
            core.WebMessageReceived += OnWebMessage;
            _countTimer.Start();
            core.Navigate(PageUrl);
        }
        catch (WebView2RuntimeNotFoundException)
        {
            StartupError("The Microsoft Edge WebView2 Runtime is missing. Install it to run F-7WHISPER.");
        }
        catch (Exception ex)
        {
            StartupError($"F-7WHISPER could not open its interface.\n\n{ex.Message}");
        }
    }

    private void StartupError(string message)
    {
        if (_closing || IsDisposed)
            return;

        MessageBox.Show(this, message, Text, MessageBoxButtons.OK, MessageBoxIcon.Error);
        Close();
    }

    private void OnWebMessage(object? sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        if (_closing || !string.Equals(e.Source, PageUrl, StringComparison.Ordinal))
            return;

        try
        {
            using var document = JsonDocument.Parse(e.WebMessageAsJson);
            var message = document.RootElement;
            if (message.ValueKind != JsonValueKind.Object)
                return;

            string? id = StringField(message, "id");
            switch (StringField(message, "type"))
            {
                case "ready":
                    _ready = true;
                    SendState();
                    return;

                case "window":
                    string? action = StringField(message, "action");
                    string? edge = StringField(message, "edge");
                    // Leave the WebView2 callback before entering a native modal loop.
                    BeginInvoke(() => WindowCommand(action, edge));
                    return;

                case "pin":
                    if (!message.TryGetProperty("on", out var pin) ||
                        pin.ValueKind is not (JsonValueKind.True or JsonValueKind.False))
                        return;
                    TopMost = pin.GetBoolean();
                    break;

                case "toggle" when id is not null:
                    _engine.Toggle(id);
                    break;

                case "startAll":
                    _engine.SetAll(true);
                    break;

                case "stopAll":
                    _engine.SetAll(false);
                    break;

                case "add":
                    if (!message.TryGetProperty("key", out var addKey) ||
                        ReadKey(addKey) is not { } key ||
                        !message.TryGetProperty("intervalMs", out var addInterval) ||
                        ReadInterval(addInterval) is not int interval)
                        return;
                    _engine.Add(key, interval);
                    break;

                case "edit" when id is not null:
                    KeyChoice? replacement = null;
                    int? duration = null;
                    if (message.TryGetProperty("key", out var editKey))
                    {
                        replacement = ReadKey(editKey);
                        if (replacement is null)
                            return;
                    }
                    if (message.TryGetProperty("intervalMs", out var editInterval))
                    {
                        duration = ReadInterval(editInterval);
                        if (duration is null)
                            return;
                    }
                    if (replacement is null && duration is null)
                        return;
                    _engine.Edit(id, replacement, duration);
                    break;

                case "remove" when id is not null:
                    _engine.Remove(id);
                    break;

                default:
                    return;
            }

            Changed();
        }
        catch (JsonException)
        {
            // An incomplete or malformed message cannot change a row.
        }
    }

    private static string? StringField(JsonElement obj, string name) =>
        obj.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    private static KeyChoice? ReadKey(JsonElement element)
    {
        if (element.ValueKind != JsonValueKind.Object ||
            StringField(element, "label") is not string label ||
            StringField(element, "code") is not string code ||
            !element.TryGetProperty("vk", out var value) ||
            value.ValueKind != JsonValueKind.Number || !value.TryGetInt32(out int vk) ||
            !KeyChoice.IsValid(label, code, vk))
            return null;

        return new KeyChoice(label, code, vk);
    }

    private static int? ReadInterval(JsonElement value) =>
        value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out long ms)
            ? Settings.ClampInterval(ms)
            : null;

    private void WindowCommand(string? action, string? edge)
    {
        if (_closing || IsDisposed)
            return;

        switch (action)
        {
            case "minimize":
                WindowState = FormWindowState.Minimized;
                break;
            case "maximize":
                WindowState = WindowState == FormWindowState.Maximized
                    ? FormWindowState.Normal : FormWindowState.Maximized;
                break;
            case "close":
                Close();
                break;
            case "drag":
                Native.BeginMouseOperation(Handle, Native.HtCaption);
                break;
            case "resize" when WindowState != FormWindowState.Maximized:
                int hit = Native.EdgeHit(edge);
                if (hit != 0)
                    Native.BeginMouseOperation(Handle, hit);
                break;
        }
    }

    protected override void WndProc(ref Message m)
    {
        switch (m.Msg)
        {
            case Native.WmNcCalcSize:
                Native.AdjustClient(m.HWnd, m.LParam);
                m.Result = 0;
                return;

            case Native.WmGetMinMaxInfo:
                base.WndProc(ref m);
                Native.AdjustTracking(m.HWnd, m.LParam);
                return;

            case Native.WmHotkey when m.WParam == HotkeyId && !_closing:
                _engine.SetAll(false);
                Changed();
                m.Result = 0;
                return;
        }

        base.WndProc(ref m);

        if (!_trackWindow || _closing)
            return;

        if (m.Msg == Native.WmSize)
        {
            int kind = (int)m.WParam;
            if (kind != Native.SizeMinimized)
                _restoreMaximized = kind == Native.SizeMaximized;
            if (_sizeKind != kind)
            {
                _sizeKind = kind;
                SendState();
            }
        }

        if (m.Msg is Native.WmSize or Native.WmMove)
            ScheduleSave();
    }

    private void Changed()
    {
        SendState();
        ScheduleSave();
    }

    private void SendState()
    {
        if (!_ready || _closing)
            return;

        Post(new
        {
            type = "state",
            rows = _engine.Snapshot(),
            pinned = TopMost,
            maximized = WindowState == FormWindowState.Maximized,
            hotkey = "F7",
            hotkeyOk = _hotkeyOk
        });
    }

    private void Post(object message) =>
        _web.CoreWebView2?.PostWebMessageAsJson(JsonSerializer.Serialize(message, Settings.Json));

    private void ScheduleSave()
    {
        if (_closing)
            return;
        _saveTimer.Stop();
        _saveTimer.Start();
    }

    private void SaveSettings()
    {
        var bounds = WindowState == FormWindowState.Normal ? Bounds : RestoreBounds;
        if (bounds.Width <= 0 || bounds.Height <= 0)
            bounds = _initialBounds;

        new Settings(_engine.Configuration(), TopMost,
            new SavedWindow(bounds.X, bounds.Y, bounds.Width, bounds.Height, _restoreMaximized))
            .Save();
    }

    protected override void OnFormClosing(FormClosingEventArgs e)
    {
        base.OnFormClosing(e);
        if (e.Cancel)
            return;

        _closing = true;
        _countTimer.Stop();
        _saveTimer.Stop();
        _engine.SetAll(false);
        SaveSettings();
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing && !_disposed)
        {
            _disposed = true;
            _closing = true;
            _countTimer.Dispose();
            _saveTimer.Dispose();
            _engine.Dispose();
        }
        base.Dispose(disposing);
    }
}
