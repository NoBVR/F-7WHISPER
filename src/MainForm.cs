using System;
using System.Collections.Generic;
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
    private const int StartId = 6;
    private const int StopId = 7;
    private const int FirstTriggerId = 0x100;
    private const int LastTriggerId = 0xBFFF;
    private const int WidthCss = 800;
    private static readonly Color Background = Color.FromArgb(10, 10, 10);
    private static readonly string AppVersion =
        typeof(MainForm).Assembly.GetName().Version!.ToString(3);

    private readonly WebView2 _web = new();
    private readonly Timer _countTimer = new() { Interval = 200 };
    private readonly Timer _saveTimer = new() { Interval = 500 };
    private readonly Timer _pickTimer = new() { Interval = 15 };
    private readonly KeyEngine _engine;
    private readonly Screen _initialScreen;
    private readonly Point? _initialPosition;

    // Registered row triggers by hotkey id; one id per distinct chord.
    private readonly Dictionary<int, Chord> _triggerIds = new();
    // Last registration result per chord, kept through a pause so the page shows no false error.
    private Dictionary<Chord, bool> _triggerOk = new();

    private HotkeyPair _hotkeys;
    private bool _startOk;
    private bool _stopOk;
    private bool _paused;
    private bool _pinned;
    private bool _pickButtonDown;
    private bool _trackWindow;
    private bool _wasMinimized;
    private bool _ready;
    private bool _closing;
    private bool _disposed;
    private int? _fitHeight;
    private int? _fitWidth;
    private Size _fixedSize;

    internal MainForm()
    {
        var settings = Settings.Load();
        _engine = new KeyEngine(settings.Rows, settings.SwitchStop);
        _hotkeys = settings.Hotkeys;
        _pinned = settings.Pinned;

        var primary = Screen.PrimaryScreen ?? Screen.AllScreens[0];
        _initialScreen = primary;
        if (settings.Window is { } saved)
        {
            var point = new Point(saved.X, saved.Y);
            if (Screen.AllScreens.FirstOrDefault(screen => screen.Bounds.Contains(point)) is { } screen)
            {
                _initialScreen = screen;
                _initialPosition = point;
            }
        }

        Text = "F-7WHISPER";
        using (var iconStream = typeof(MainForm).Assembly.GetManifestResourceStream("F7Whisper.app.ico"))
        {
            if (iconStream is not null) Icon = new Icon(iconStream);
        }

        BackColor = Background;
        AutoScaleMode = AutoScaleMode.None;
        StartPosition = FormStartPosition.Manual;
        FormBorderStyle = FormBorderStyle.FixedSingle;
        MinimizeBox = true;
        MaximizeBox = false;
        SizeGripStyle = SizeGripStyle.Hide;
        PlaceWindow(_initialScreen.WorkingArea, _initialPosition, (int)Native.GetDpiForSystem());
        TopMost = _pinned;

        _web.DefaultBackgroundColor = Background;
        _web.Dock = DockStyle.Fill;
        Controls.Add(_web);

        _countTimer.Tick += (_, _) =>
        {
            if (!_ready || _closing)
                return;

            if (_engine.TakeStateNotification())
                SendState();
            else if (_engine.TakeChangedCounts() is { } counts)
                Post(new { type = "counts", counts });
        };

        _saveTimer.Tick += (_, _) =>
        {
            _saveTimer.Stop();
            SaveSettings();
        };

        _pickTimer.Tick += (_, _) => PollPoint();
    }

    private void PlaceWindow(Rectangle area, Point? location, int dpi)
    {
        if (area.Width <= 0 || area.Height <= 0)
            return;

        int rows = _engine.RowCount;
        long heightCss = _fitHeight ?? (40L + 56 + 36 + (rows == 0 ? 96 : 48L * rows) + 28);
        double scale = (dpi > 0 ? dpi : 96) / 96.0;
        int width = (int)Math.Clamp(Math.Round((_fitWidth ?? WidthCss) * scale), 1, area.Width);
        int height = (int)Math.Clamp(Math.Round(heightCss * scale), 1, area.Height);
        int x = location?.X ?? area.Left + (area.Width - width) / 2;
        int y = location?.Y ?? area.Top + (area.Height - height) / 2;
        var bounds = new Rectangle(Math.Clamp(x, area.Left, area.Right - width),
                                   Math.Clamp(y, area.Top, area.Bottom - height), width, height);

        // WM_GETMINMAXINFO can arrive synchronously during the bounds change.
        _fixedSize = bounds.Size;
        if (Bounds != bounds)
            Bounds = bounds;
    }

    private void FitWindow()
    {
        if (_trackWindow && !_closing && WindowState == FormWindowState.Normal)
            PlaceWindow(Screen.FromHandle(Handle).WorkingArea, Location, DeviceDpi);
    }

    protected override void OnHandleCreated(EventArgs e)
    {
        base.OnHandleCreated(e);
        Native.ApplyFrame(Handle);
        _engine.SetOwner(Handle);
        RegisterHotkeys();
        SendState();
    }

    protected override void OnHandleDestroyed(EventArgs e)
    {
        UnregisterHotkeys();
        if (!_closing)
            _engine.SetOwner(0);
        base.OnHandleDestroyed(e);
    }

    protected override void OnLoad(EventArgs e)
    {
        // The hidden HWND now has the destination monitor's actual DPI.
        PlaceWindow(_initialScreen.WorkingArea, _initialPosition, DeviceDpi);
        _trackWindow = true;
        base.OnLoad(e);
    }

    private void UnregisterHotkeys()
    {
        if (_startOk) Native.UnregisterHotKey(Handle, StartId);
        if (_stopOk) Native.UnregisterHotKey(Handle, StopId);
        foreach (int id in _triggerIds.Keys)
            Native.UnregisterHotKey(Handle, id);
        _startOk = false;
        _stopOk = false;
        _triggerIds.Clear();
    }

    private void RegisterHotkeys()
    {
        UnregisterHotkeys();
        if (!_paused && !_closing && IsHandleCreated)
        {
            _startOk = Register(StartId, _hotkeys.Start?.Chord);
            _stopOk = Register(StopId, _hotkeys.Stop?.Chord);
        }
        RegisterTriggers();
    }

    // Brings the row-trigger registrations in line with the rows; unchanged chords stay registered.
    private void RegisterTriggers()
    {
        bool active = !_paused && !_closing && IsHandleCreated;
        var wanted = _engine.TriggerChords();
        var ok = new Dictionary<Chord, bool>();
        var registered = new HashSet<Chord>();

        foreach (var (id, chord) in _triggerIds.ToArray())
        {
            if (active && !_hotkeys.Uses(chord) && Array.IndexOf(wanted, chord) >= 0)
            {
                registered.Add(chord);
                continue;
            }
            Native.UnregisterHotKey(Handle, id);
            _triggerIds.Remove(id);
        }

        int nextId = FirstTriggerId;
        foreach (var chord in wanted)
        {
            bool registrationOk;
            if (_hotkeys.Uses(chord))
            {
                // Start all / Stop all keeps the key.
                registrationOk = false;
            }
            else if (!active)
            {
                registrationOk = _triggerOk.GetValueOrDefault(chord, true);
            }
            else if (registered.Contains(chord))
            {
                registrationOk = true;
            }
            else
            {
                while (_triggerIds.ContainsKey(nextId))
                    nextId++;
                registrationOk = nextId <= LastTriggerId && Register(nextId, chord);
                if (registrationOk)
                    _triggerIds.Add(nextId, chord);
            }
            ok[chord] = registrationOk;
        }

        _triggerOk = ok;
    }

    private bool Register(int id, Chord? chord) =>
        chord is { } key &&
        Native.RegisterHotKey(Handle, id, Native.ModNoRepeat | key.Modifiers, (uint)key.Vk);

    private bool TriggerOk(Chord chord) => _triggerOk.GetValueOrDefault(chord);

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
            options.IsPinchZoomEnabled = false;
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
                {
                    args.Cancel = true;
                    return;
                }

                _ready = false;
                CancelPick();
                if (_paused)
                {
                    _paused = false;
                    RegisterHotkeys();
                }
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
            string? id = message.Field("id").Text();

            switch (message.Field("type").Text())
            {
                case "ready":
                    CancelPick();
                    _paused = false;
                    RegisterHotkeys();
                    _ready = true;
                    SendState();
                    return;

                case "window":
                    string? action = message.Field("action").Text();
                    if (action is "minimize" or "close" or "drag")
                        BeginInvoke(() => WindowCommand(action));
                    return;

                case "fit":
                    var widthField = message.Field("width");
                    int? fitWidth = widthField.Integer();
                    if (message.Field("height").Integer() is int height && height > 0 &&
                        (widthField.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null ||
                         fitWidth > 0))
                    {
                        // WebView2 delivers this callback on its owning WinForms UI thread.
                        _fitHeight = height;
                        _fitWidth = fitWidth;
                        FitWindow();
                    }
                    return;

                case "pin":
                    if (message.Field("on").Boolean() is not bool pin)
                        return;
                    _pinned = pin;
                    TopMost = _pinned || _pickTimer.Enabled;
                    break;

                case "toggle" when id is not null:
                    _engine.Toggle(id);
                    SendState();
                    return;

                case "startAll":
                    _engine.SetAll(true);
                    SendState();
                    return;

                case "stopAll":
                    _engine.SetAll(false);
                    SendState();
                    return;

                case "add":
                    if (RowSpec.Read(message.Field("input"), "", fromFile: false) is not { } added)
                        return;
                    _engine.Add(added);
                    RegisterTriggers();
                    if (_fitHeight is null) FitWindow();
                    break;

                case "edit" when id is not null:
                    if (RowSpec.Read(message.Field("input"), id, fromFile: false) is not { } edited)
                        return;
                    _engine.Edit(edited);
                    RegisterTriggers();
                    break;

                case "remove" when id is not null:
                    _engine.Remove(id);
                    RegisterTriggers();
                    if (_fitHeight is null) FitWindow();
                    break;

                case "settings":
                    if (message.Field("switchStop").Boolean() is not bool switchStop ||
                        HotkeyPair.Read(message.Field("hotkeys"), fromFile: false) is not { } keys)
                        return;
                    _hotkeys = keys;
                    _engine.SwitchStop = switchStop;
                    RegisterHotkeys();
                    break;

                case "pauseHotkeys":
                    if (message.Field("on").Boolean() is bool paused)
                    {
                        _paused = paused;
                        RegisterHotkeys();
                        SendState();
                    }
                    return;

                case "listApps":
                    Post(new { type = "apps", apps = Native.ListApps(Handle) });
                    return;

                case "pickPoint":
                    _pickButtonDown = Native.LeftButtonDown;
                    TopMost = true;
                    _pickTimer.Start();
                    return;

                case "pickCancel":
                    CancelPick();
                    return;

                default:
                    return;
            }

            ScheduleSave();
            SendState();
        }
        catch (JsonException)
        {
            // Invalid JSON cannot change the configuration.
        }
        catch (InvalidOperationException ex)
        {
            // JsonElement can also reject a string with an invalid escaped surrogate.
            System.Diagnostics.Debug.WriteLine(ex.Message);
        }
    }

    private void CancelPick()
    {
        if (!_pickTimer.Enabled)
            return;

        _pickTimer.Stop();
        TopMost = _pinned;
    }

    private void PollPoint()
    {
        bool down = Native.LeftButtonDown;
        bool pressed = down && !_pickButtonDown;
        _pickButtonDown = down;
        if (!pressed || !Native.TryGetOutsidePoint(Handle, out var point) || point is null)
            return;

        CancelPick();
        if (WindowState == FormWindowState.Minimized)
            Native.RestoreWindow(Handle);
        Activate();
        if (_ready && !_closing)
            Post(new { type = "point", point.X, point.Y });
    }

    private void WindowCommand(string action)
    {
        if (_closing || IsDisposed)
            return;

        switch (action)
        {
            case "minimize":
                WindowState = FormWindowState.Minimized;
                break;
            case "close":
                Close();
                break;
            case "drag":
                // Leave the WebView2 callback before entering the native move loop.
                Native.BeginDrag(Handle);
                break;
        }
    }

    protected override void WndProc(ref Message m)
    {
        switch (m.Msg)
        {
            case Native.WmNcCalcSize:
                // Retain the DWM frame styles and give the entire window to WebView2.
                m.Result = 0;
                return;

            case Native.WmNcHitTest:
                m.Result = 1; // HTCLIENT, including every edge and corner
                return;

            case Native.WmGetMinMaxInfo:
                base.WndProc(ref m);
                Native.FixTracking(m.LParam, _fixedSize);
                return;

            case Native.WmSysCommand when ((int)m.WParam & 0xfff0) is 0xf000 or 0xf030:
                // SC_SIZE and SC_MAXIMIZE, including system-menu and keyboard requests.
                m.Result = 0;
                return;

            case Native.WmHotkey when m.WParam == StartId || m.WParam == StopId:
                bool start = m.WParam == StartId;
                var key = start ? _hotkeys.Start : _hotkeys.Stop;
                uint chord = unchecked((uint)(long)m.LParam);
                if (!_closing && !_paused && (start ? _startOk : _stopOk) && key is not null &&
                    (chord >> 16) == (uint)key.Vk && (chord & 0x0f) == key.Modifiers)
                {
                    _engine.SetAll(start);
                    SendState();
                }
                m.Result = 0;
                return;

            case Native.WmHotkey when _triggerIds.TryGetValue((int)m.WParam, out var trigger):
                uint pressed = unchecked((uint)(long)m.LParam);
                if (!_closing && !_paused && (pressed >> 16) == (uint)trigger.Vk &&
                    (pressed & 0x0f) == trigger.Modifiers && _engine.Activate(trigger))
                    SendState();
                m.Result = 0;
                return;

            case Native.WmStateChanged:
                if (_ready && !_closing && _engine.TakeStateNotification())
                    SendState();
                m.Result = 0;
                return;
        }

        base.WndProc(ref m);
        if (!_trackWindow || _closing)
            return;

        switch (m.Msg)
        {
            case Native.WmMove:
                if (WindowState == FormWindowState.Normal)
                    ScheduleSave();
                break;

            case Native.WmSize:
                if (m.WParam == 1) // SIZE_MINIMIZED
                {
                    _wasMinimized = true;
                }
                else if (_wasMinimized)
                {
                    _wasMinimized = false;
                    BeginInvoke((Action)FitWindow);
                }
                break;

            case Native.WmDpiChanged:
            case 0x007E: // WM_DISPLAYCHANGE
            case 0x001A: // WM_SETTINGCHANGE, including work-area changes
            case 0x0232: // WM_EXITSIZEMOVE
                FitWindow();
                break;
        }
    }

    private void SendState()
    {
        if (!_ready || _closing)
            return;

        Post(new
        {
            type = "state",
            version = AppVersion,
            platform = "windows",
            accessOk = true,
            pinned = _pinned,
            rows = _engine.Snapshot(TriggerOk),
            hotkeys = _hotkeys,
            hotkeyOk = new { start = _startOk, stop = _stopOk },
            switchStop = _engine.SwitchStop
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
        var position = new SavedWindow(bounds.X, bounds.Y);
        new Settings(_engine.Configuration(), _pinned, position, _hotkeys, _engine.SwitchStop).Save();
    }

    protected override void OnFormClosing(FormClosingEventArgs e)
    {
        base.OnFormClosing(e);
        if (e.Cancel)
            return;

        _closing = true;
        CancelPick();
        UnregisterHotkeys();
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
            _pickTimer.Dispose();
            _countTimer.Dispose();
            _saveTimer.Dispose();
            _engine.Dispose();
        }
        base.Dispose(disposing);
    }
}
