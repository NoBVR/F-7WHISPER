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
    private enum HotkeyAction { Start, Stop, Rows }
    private readonly record struct Registration(int Id, HotkeyAction Action, bool ViaKeyboardHook = false);

    private const string AppHost = "app.f7whisper.local";
    private const string PageUrl = "https://app.f7whisper.local/index.html";
    private const int LastHotkeyId = 0xBFFF;
    private const int WidthCss = 800;
    private static readonly Color Background = Color.FromArgb(10, 10, 10);
    private static readonly string AppVersion =
        typeof(MainForm).Assembly.GetName().Version!.ToString(3);

    private readonly WebView2 _web = new();
    private readonly Timer _countTimer = new() { Interval = 200 };
    private readonly Timer _saveTimer = new() { Interval = 500 };
    private readonly Timer _pickTimer = new() { Interval = 15 };
    private readonly Updater _updater;
    private readonly KeyEngine _engine;
    private readonly Screen _initialScreen;
    private readonly Point? _initialPosition;
    private readonly Dictionary<Chord, Registration> _registered = new();

    private Dictionary<Chord, bool> _triggerOk = new();
    private MouseHotkeys? _mouse;
    private KeyboardHook? _keyboard;
    private int _keyboardId;
    private bool _twoKey;
    private nint _hotkeyWindow;
    private int _lastId;
    private HotkeyPair _hotkeys;
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
        _updater = new Updater(AppVersion, message =>
        {
            if (_ready && !_closing) Post(message);
        }, Close);
        var settings = Settings.Load();
        _engine = new KeyEngine(settings.Rows, settings.SwitchStop);
        _hotkeys = settings.Hotkeys;
        _twoKey = settings.TwoKey;
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
            if (_closing)
                return;

            // Posted edges normally run first. Recover any edge whose wake-up
            // message could not be posted, including a hold's physical release.
            while (DispatchMouseEdge()) { }
            while (DispatchKeyboardEdge()) { }

            if (!_ready)
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
        long heightCss = _fitHeight ?? (32L + 52 + 36 + (rows == 0 ? 96 : 48L * Math.Min(rows, 8)) + 16);
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
        _hotkeyWindow = Handle;
        Native.ApplyFrame(Handle);
        _engine.SetOwner(Handle);
        RegisterHotkeys();
        SendState();
    }

    protected override void OnHandleDestroyed(EventArgs e)
    {
        UnregisterHotkeys();
        _hotkeyWindow = 0;
        if (!_closing)
            _engine.SetOwner(0);
        base.OnHandleDestroyed(e);
    }

    protected override void OnLoad(EventArgs e)
    {
        PlaceWindow(_initialScreen.WorkingArea, _initialPosition, DeviceDpi);
        _trackWindow = true;
        base.OnLoad(e);
    }

    private void UnregisterHotkeys() => ApplyBindings(new(), [], refreshKeyboard: false);

    private void RegisterHotkeys(bool refreshKeyboard = true)
    {
        var wanted = new Dictionary<Chord, HotkeyAction>();
        var triggers = _engine.TriggerChords(out var pressedKeys);
        var keyboardChords = triggers.Where(chord => chord.IsCombo ||
            (!_hotkeys.Uses(chord) && KeyChoice.IsOrdinaryKey(chord.Vk) && pressedKeys.Contains(chord.Vk)))
            .ToHashSet();
        if (!_paused && !_closing && _hotkeyWindow != 0)
        {
            if (_hotkeys.Stop is { } stop)
                wanted.Add(stop.Chord, HotkeyAction.Stop);
            if (_hotkeys.Start is { } start)
                wanted.TryAdd(start.Chord, HotkeyAction.Start);

            foreach (var chord in triggers)
                if (!chord.IsCombo)
                    wanted.TryAdd(chord, HotkeyAction.Rows);

            if (_twoKey)
            {
                foreach (var chord in triggers)
                    if (chord.IsCombo && Native.HasScanCode(chord.Vk) && Native.HasScanCode(chord.With) &&
                        !wanted.ContainsKey(new Chord(chord.Vk, chord.Modifiers)) &&
                        !wanted.ContainsKey(new Chord(chord.With, chord.Modifiers)))
                        wanted.Add(chord, HotkeyAction.Rows);
            }
        }

        ApplyBindings(wanted, keyboardChords, refreshKeyboard);

        // Preserve the last known trigger result while the page captures a key.
        var results = new Dictionary<Chord, bool>();
        foreach (var chord in triggers)
        {
            if (keyboardChords.Contains(chord))
                results[chord] = _paused
                    ? _triggerOk.GetValueOrDefault(chord)
                    : _keyboard is { Running: true } && _registered.ContainsKey(chord);
            else
                results[chord] = !_hotkeys.Uses(chord) && (_paused
                    ? _triggerOk.GetValueOrDefault(chord)
                    : _registered.ContainsKey(chord));
        }
        _triggerOk = results;
    }

    private void ApplyBindings(Dictionary<Chord, HotkeyAction> wanted, HashSet<Chord> keyboardChords,
                               bool refreshKeyboard)
    {
        if (_keyboard is { Running: false })
            DropKeyboardHook();

        foreach (var (chord, registration) in _registered.ToArray())
        {
            if (wanted.TryGetValue(chord, out var action) && action == registration.Action &&
                registration.ViaKeyboardHook == keyboardChords.Contains(chord) &&
                (registration.ViaKeyboardHook || chord.IsMouse || !refreshKeyboard))
                continue;

            _registered.Remove(chord);
            if (!registration.ViaKeyboardHook && !chord.IsMouse)
            {
                Native.UnregisterHotKey(_hotkeyWindow, registration.Id);
                if (!keyboardChords.Contains(chord))
                    continue;
            }
            if (registration.Action == HotkeyAction.Rows)
                _engine.ReleaseHookHold(chord);
        }

        var usedIds = _registered.Values.Select(registration => registration.Id).ToHashSet();
        foreach (var (chord, action) in wanted)
        {
            if (_registered.ContainsKey(chord))
                continue;

            // Hook tokens have their own ID space and never reach RegisterHotKey.
            if (keyboardChords.Contains(chord))
            {
                // An earlier native binding can still have a pending poll after capture.
                if (!chord.IsCombo)
                    _engine.ReleaseHookHold(chord, onlyIfPolled: true);
                _registered.Add(chord, new Registration(unchecked(--_keyboardId), action, ViaKeyboardHook: true));
                continue;
            }

            // There are fewer possible VK/modifier pairs than application hotkey IDs.
            // Rotate IDs so queued input for a retired binding cannot start its replacement.
            do
            {
                _lastId = _lastId == LastHotkeyId ? 1 : _lastId + 1;
            } while (usedIds.Contains(_lastId));

            if (!chord.IsMouse && !Native.RegisterHotKey(_hotkeyWindow, _lastId,
                    Native.ModNoRepeat | chord.Modifiers, (uint)chord.Vk))
                continue;

            usedIds.Add(_lastId);
            _registered.Add(chord, new Registration(_lastId, action));
        }

        var keyboardBindings = _registered.Where(pair => pair.Value.ViaKeyboardHook)
                                          .ToDictionary(pair => pair.Key, pair => pair.Value.Id);
        if (keyboardBindings.Count == 0)
        {
            _keyboard?.Dispose();
            _keyboard = null;
        }
        else if (_keyboard is not null)
            _keyboard.SetBindings(keyboardBindings);
        else
        {
            _keyboard = KeyboardHook.Start(_hotkeyWindow, keyboardBindings);
            if (_keyboard is null)
                DropKeyboardHook();
        }

        var mouseBindings = _registered.Where(pair => pair.Key.IsMouse)
                                       .ToDictionary(pair => pair.Key, pair => pair.Value.Id);

        if (mouseBindings.Count == 0)
        {
            var oldMouse = _mouse;
            _mouse = null;
            oldMouse?.Dispose();
        }
        else if (_mouse is not null)
        {
            _mouse.SetBindings(mouseBindings);
        }
        else
        {
            _mouse = MouseHotkeys.Start(_hotkeyWindow, mouseBindings);
            if (_mouse is null)
            {
                foreach (var chord in mouseBindings.Keys)
                    _registered.Remove(chord);
            }
        }
    }

    private bool HotkeyOk(Hotkey? hotkey, HotkeyAction action) =>
        hotkey is not null && _registered.TryGetValue(hotkey.Chord, out var registration) &&
        registration.Action == action;

    private void DispatchHotkey(Chord chord, int id, bool down)
    {
        if (_closing || _paused || !_registered.TryGetValue(chord, out var registration) ||
            registration.Id != id)
            return;

        if (registration.Action == HotkeyAction.Rows)
        {
            bool changed = down ? _engine.Activate(chord, registration.ViaKeyboardHook)
                                : _engine.ReleaseHookHold(chord);
            if (changed)
                SendState();
        }
        else if (down)
        {
            _engine.SetAll(registration.Action == HotkeyAction.Start);
            SendState();
        }
    }

    private bool DispatchMouseEdge()
    {
        if (_mouse is null || !_mouse.TryRead(out var edge))
            return false;

        DispatchHotkey(edge.Chord, edge.RegistrationId, edge.Down);
        return true;
    }

    private void DropKeyboardHook()
    {
        _keyboard?.Dispose();
        _keyboard = null;
        foreach (var (chord, registration) in _registered.ToArray())
        {
            if (!registration.ViaKeyboardHook)
                continue;

            _registered.Remove(chord);
            _triggerOk[chord] = false;
            _engine.ReleaseHookHold(chord);
        }
    }

    private bool DispatchKeyboardEdge()
    {
        if (_keyboard is null)
            return false;

        if (!_keyboard.Running)
        {
            DropKeyboardHook();
            SendState();
            return false;
        }

        if (!_keyboard.TryRead(out var edge))
            return false;

        DispatchHotkey(edge.Chord, edge.RegistrationId, edge.Down);
        return true;
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
                    _updater.Ready();
                    return;

                case "installUpdate":
                    _updater.Install();
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
                    RegisterHotkeys(refreshKeyboard: false);
                    if (_fitHeight is null) FitWindow();
                    break;

                case "edit" when id is not null:
                    if (RowSpec.Read(message.Field("input"), id, fromFile: false) is not { } edited)
                        return;
                    _engine.Edit(edited);
                    RegisterHotkeys(refreshKeyboard: false);
                    break;

                case "setDisabled" when id is not null:
                    if (message.Field("disabled").Boolean() is not bool disabled ||
                        !_engine.SetDisabled(id, disabled))
                        return;
                    RegisterHotkeys(refreshKeyboard: false);   // drops or restores its activate key
                    break;

                case "remove" when id is not null:
                    _engine.Remove(id);
                    RegisterHotkeys(refreshKeyboard: false);
                    if (_fitHeight is null) FitWindow();
                    break;

                case "settings":
                    if (message.Field("switchStop").Boolean() is not bool switchStop ||
                        HotkeyPair.Read(message.Field("hotkeys"), fromFile: false) is not { } keys)
                        return;
                    _hotkeys = keys;
                    _twoKey = message.Field("twoKey").Boolean() ?? _twoKey;
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

            case Native.WmHotkey:
                uint packed = unchecked((uint)(long)m.LParam);
                var chord = new Chord((int)(packed >> 16), packed & 0x0f);
                if (!chord.IsMouse)
                    DispatchHotkey(chord, (int)m.WParam, down: true);
                m.Result = 0;
                return;

            case Native.WmKeyboardEdge:
                DispatchKeyboardEdge();
                m.Result = 0;
                return;

            case Native.WmMouseEdge:
                DispatchMouseEdge();
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
            rows = _engine.Snapshot(chord => _triggerOk.GetValueOrDefault(chord)),
            hotkeys = _hotkeys,
            hotkeyOk = new
            {
                start = HotkeyOk(_hotkeys.Start, HotkeyAction.Start),
                stop = HotkeyOk(_hotkeys.Stop, HotkeyAction.Stop)
            },
            twoKey = _twoKey,
            selfKey = true,
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
        new Settings(_engine.Configuration(), _pinned, position, _hotkeys, _engine.SwitchStop, _twoKey).Save();
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
            UnregisterHotkeys();
            _updater.Stop();
            _pickTimer.Dispose();
            _countTimer.Dispose();
            _saveTimer.Dispose();
            _engine.Dispose();
        }
        base.Dispose(disposing);
    }
}
