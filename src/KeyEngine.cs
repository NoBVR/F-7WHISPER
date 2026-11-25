using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading;

namespace F7Whisper;

internal sealed class KeyEngine : IDisposable
{
    private sealed class Row
    {
        internal RowSpec Spec;
        internal Native.Stroke Stroke;
        internal Native.Stroke HeldStroke;
        internal long Period;
        internal long Hold;
        internal long DownDue;
        internal long UpDue;
        internal long Count;
        internal long ReportedCount;
        internal bool Running;
        internal bool Down;
        // Keys and left / right / middle clicks run on Blur's engine; back / forward
        // clicks (no side-button output there) stay on this scheduler.
        internal bool UsesBlur;
        internal ulong Blur;
        internal bool BlurPaused;
        internal long BlurBase;

        internal Row(RowSpec spec)
        {
            Spec = spec;
            Configure(spec);
        }

        internal void Configure(RowSpec spec)
        {
            Spec = spec;
            Stroke = Native.MapStroke(spec);
            Period = Milliseconds(spec.IntervalMs);
            Hold = Math.Min(Milliseconds(30), Period / 2);
            // A key without a usable VK never presses, as before, on the old scheduler.
            UsesBlur = spec.Kind == "click"
                ? spec.Button is "left" or "right" or "middle"
                : Stroke.Vk != 0;
        }
    }

    private static readonly long WatchPeriod = Milliseconds(75);
    private static readonly long HoldPeriod = Milliseconds(10);
    private static readonly long BlurPollMin = Milliseconds(1);
    private static readonly long BlurPollMax = Milliseconds(10);
    private static readonly long ImageCachePeriod = Milliseconds(1000);
    private static readonly uint OwnPid = (uint)Environment.ProcessId;

    private readonly object _gate = new();
    private readonly AutoResetEvent _wake = new(false);
    private readonly List<Row> _rows;
    // A removed or reconfigured row can still owe its original release.
    private readonly List<Row> _held = new();
    private readonly List<Chord> _keyboardHolds = new();
    private readonly HashSet<Chord> _mouseHolds = new();
    private readonly Thread _thread;
    private nint _owner;
    private bool _switchStop;
    private bool _stateChanged;
    private uint _targetPid;
    private long _watchDue = long.MaxValue;
    private long _holdDue = long.MaxValue;
    private uint _imagePid;
    private string? _imageName;
    private long _imageDue;
    private bool _exiting;

    internal KeyEngine(RowSpec[] rows, bool switchStop)
    {
        _rows = rows.Select(spec => new Row(spec)).ToList();
        _switchStop = switchStop;
        _thread = new Thread(Run) { IsBackground = true, Name = "F-7WHISPER input scheduler" };
        _thread.Start();
    }

    private static long Milliseconds(int ms) =>
        (long)Math.Round(ms * (Stopwatch.Frequency / 1000.0));

    internal int RowCount
    {
        get { lock (_gate) return _rows.Count; }
    }

    internal bool SwitchStop
    {
        get { lock (_gate) return _switchStop; }
        set
        {
            lock (_gate)
            {
                _switchStop = value;
                UpdateWatch();
                _wake.Set();
            }
        }
    }

    internal void SetOwner(nint owner)
    {
        lock (_gate)
        {
            _owner = owner;
            _wake.Set();
        }
    }

    internal void Add(RowSpec spec)
    {
        lock (_gate)
        {
            string id;
            do
            {
                id = Guid.NewGuid().ToString("N")[..12];
            } while (Find(id) is not null);

            _rows.Add(new Row(spec with { Id = id, Disabled = false }));
            _wake.Set();
        }
    }

    internal void Edit(RowSpec spec)
    {
        lock (_gate)
        {
            if (Find(spec.Id) is not { } row)
                return;

            spec = spec with { Disabled = row.Spec.Disabled };   // editing keeps a row disabled
            if (row.Running && row.Spec.Trigger is { Hold: true } old &&
                old.Hotkey.Chord != spec.Trigger?.Hotkey.Chord)
            {
                SetRunning(row, false);
                UpdateWatch();
            }
            else
            {
                Release(row);
                StopBlur(row);
            }
            row.Configure(spec);
            row.DownDue = Stopwatch.GetTimestamp();
            _wake.Set();
        }
    }

    internal void Remove(string id)
    {
        lock (_gate)
        {
            if (Find(id) is not { } row)
                return;

            SetRunning(row, false);
            _rows.Remove(row);
            UpdateWatch();
            _wake.Set();
        }
    }

    internal void Toggle(string id)
    {
        lock (_gate)
        {
            if (Find(id) is not { } row || row.Spec.Disabled)
                return;

            SetRunning(row, !row.Running);
            UpdateWatch();
            _wake.Set();
        }
    }

    // A disabled row never runs: toggle, Start all and its activate key all skip it, and
    // TriggerChords leaves its key out so the key reaches other apps. Enabling doesn't start it.
    internal bool SetDisabled(string id, bool disabled)
    {
        lock (_gate)
        {
            if (Find(id) is not { } row || row.Spec.Disabled == disabled)
                return false;

            if (disabled)
            {
                SetRunning(row, false);
                UpdateWatch();
            }
            row.Spec = row.Spec with { Disabled = disabled };
            _wake.Set();
            return true;
        }
    }

    internal void SetAll(bool running)
    {
        lock (_gate)
        {
            if (running)
            {
                foreach (var row in _rows)
                    if (!row.Spec.Disabled)
                        SetRunning(row, true);
                UpdateWatch();
            }
            else
            {
                StopAllLocked();
            }
            _wake.Set();
        }
    }

    internal Chord[] TriggerChords()
    {
        lock (_gate)
        {
            return _rows.Where(row => !row.Spec.Disabled && row.Spec.Trigger is not null)
                        .Select(row => row.Spec.Trigger!.Hotkey.Chord).Distinct().ToArray();
        }
    }

    // A physical press reaches this method once per chord, including shared triggers.
    internal bool Activate(Chord chord)
    {
        lock (_gate)
        {
            bool changed = false;
            bool hold = false;
            bool anyToggle = false;
            bool start = false;
            foreach (var row in _rows)
            {
                // An enabled row can share the chord, so it stays registered: skip the disabled one.
                if (row.Spec.Disabled || row.Spec.Trigger is not { } trigger || trigger.Hotkey.Chord != chord)
                    continue;

                if (trigger.Hold)
                {
                    hold = true;
                    changed |= !row.Running;
                    SetRunning(row, true);
                }
                else
                {
                    anyToggle = true;
                    start |= !row.Running;
                }
            }

            if (anyToggle)
            {
                foreach (var row in _rows)
                {
                    if (!row.Spec.Disabled && row.Spec.Trigger is { Hold: false } trigger &&
                        trigger.Hotkey.Chord == chord && row.Running != start)
                    {
                        changed = true;
                        SetRunning(row, start);
                    }
                }
            }

            if (hold && !chord.IsCombo)
            {
                if (chord.IsMouse)
                {
                    _mouseHolds.Add(chord);
                }
                else if (!_keyboardHolds.Contains(chord))
                {
                    _keyboardHolds.Add(chord);
                    if (_holdDue == long.MaxValue)
                        _holdDue = Stopwatch.GetTimestamp() + HoldPeriod;
                }
            }

            if (changed)
                UpdateWatch();
            _wake.Set();
            return changed;
        }
    }

    private bool StopHoldRows(Chord chord)
    {
        bool changed = false;
        foreach (var row in _rows)
        {
            if (row.Running && row.Spec.Trigger is { Hold: true } trigger &&
                trigger.Hotkey.Chord == chord)
            {
                SetRunning(row, false);
                changed = true;
            }
        }
        return changed;
    }

    // The keyboard hook emits an up only for an active combo. Retiring a combo
    // also stops any running Hold rows before their release path disappears.
    internal bool ReleaseHookHold(Chord chord)
    {
        lock (_gate)
        {
            if ((!chord.IsCombo && !_mouseHolds.Remove(chord)) || !StopHoldRows(chord))
                return false;

            UpdateWatch();
            _wake.Set();
            return true;
        }
    }

    // RegisterHotKey delivers the keyboard press; poll only these main keys for release.
    private void PollHolds()
    {
        bool changed = false;
        for (int i = _keyboardHolds.Count - 1; i >= 0; i--)
        {
            var chord = _keyboardHolds[i];
            if (Native.KeyDown(chord.Vk))
                continue;

            _keyboardHolds.RemoveAt(i);
            changed |= StopHoldRows(chord);
        }

        _holdDue = _keyboardHolds.Count == 0 ? long.MaxValue : Stopwatch.GetTimestamp() + HoldPeriod;
        if (changed)
        {
            UpdateWatch();
            NotifyChanged();
        }
    }

    private void NotifyChanged()
    {
        _stateChanged = true;
        if (_owner != 0)
            Native.PostStateChanged(_owner);
    }

    private Row? Find(string id) => _rows.Find(row => row.Spec.Id == id);

    private void SetRunning(Row row, bool running)
    {
        if (running && !row.Running)
            row.DownDue = Stopwatch.GetTimestamp();
        row.Running = running;
        if (!running)
        {
            StopBlur(row);
            Release(row);
        }
    }

    private void StopAllLocked()
    {
        foreach (var row in _rows)
        {
            row.Running = false;
            StopBlur(row);
        }
        ReleaseAllHeld();
        _targetPid = 0;
        _watchDue = long.MaxValue;
    }

    private void UpdateWatch()
    {
        if (!_switchStop || !_rows.Any(row => row.Running))
        {
            _targetPid = 0;
            _watchDue = long.MaxValue;
        }
        else if (_watchDue == long.MaxValue)
        {
            // Capture synchronously so a hotkey start binds the app already in front.
            _watchDue = Stopwatch.GetTimestamp() + WatchPeriod;
            ObserveForeground(Native.ForegroundPid(_owner));
        }
    }

    private bool ObserveForeground(uint pid)
    {
        // Windows can briefly report no foreground HWND during activation.
        if (_watchDue == long.MaxValue || pid == 0)
            return false;

        if (_targetPid == 0)
        {
            if (pid != OwnPid)
                _targetPid = pid;
            return false;
        }

        if (pid == _targetPid)
            return false;

        StopAllLocked();
        NotifyChanged();
        return true;
    }

    internal bool TakeStateNotification()
    {
        lock (_gate)
        {
            bool pending = _stateChanged;
            _stateChanged = false;
            return pending;
        }
    }

    internal RowSpec[] Configuration()
    {
        lock (_gate)
            return _rows.Select(row => row.Spec).ToArray();
    }

    internal object[] Snapshot(Func<Chord, bool> triggerOk)
    {
        lock (_gate)
        {
            return _rows.Select(row =>
            {
                var spec = row.Spec;
                row.ReportedCount = row.Count;
                return (object)new
                {
                    spec.Id, spec.Kind, spec.Label, spec.Code, spec.Vk,
                    spec.Button, spec.At, spec.App, spec.IntervalMs, spec.Trigger,
                    TriggerOk = spec.Trigger is { } trigger && triggerOk(trigger.Hotkey.Chord),
                    row.Running, row.Count,
                    spec.Disabled   // on every row: the page's gate for the Disable / Enable item
                };
            }).ToArray();
        }
    }

    internal Dictionary<string, long>? TakeChangedCounts()
    {
        lock (_gate)
        {
            Dictionary<string, long>? changes = null;
            foreach (var row in _rows)
            {
                if (row.Count == row.ReportedCount)
                    continue;

                changes ??= new Dictionary<string, long>(StringComparer.Ordinal);
                changes.Add(row.Spec.Id, row.Count);
                row.ReportedCount = row.Count;
            }
            return changes;
        }
    }

    private void Release(Row row)
    {
        if (!row.Down)
            return;

        if (Native.SendStroke(row.HeldStroke, up: true))
        {
            row.Down = false;
            _held.Remove(row);
        }
        else
        {
            row.UpDue = Stopwatch.GetTimestamp() + Milliseconds(20);
        }
    }

    private void ReleaseAllHeld()
    {
        for (int i = _held.Count - 1; i >= 0; i--)
            Release(_held[i]);
    }

    private static bool StartBlur(Row row)
    {
        var spec = row.Spec;
        int kind = spec.Kind == "click" ? BlurEngine.KindClick : BlurEngine.KindKey;
        int button = spec.Button switch
        {
            "left" => BlurEngine.ButtonLeft,
            "right" => BlurEngine.ButtonRight,
            "middle" => BlurEngine.ButtonMiddle,
            _ => 0
        };
        ushort vk = kind == BlurEngine.KindKey ? row.Stroke.Vk : (ushort)0;
        var at = kind == BlurEngine.KindClick ? spec.At : null;

        ulong handle;
        try
        {
            handle = BlurEngine.Start(kind, vk, button, at is null ? 0 : 1, at?.X ?? 0, at?.Y ?? 0,
                                      spec.IntervalMs);
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException
                                     or BadImageFormatException)
        {
            Debug.WriteLine(ex.Message);
            handle = 0;
        }

        if (handle == 0)
            return false;

        row.Blur = handle;
        row.BlurPaused = false;
        row.BlurBase = row.Count;
        return true;
    }

    // Blur releases anything it holds before its thread ends.
    private static void StopBlur(Row row)
    {
        if (row.Blur == 0)
            return;

        long final = BlurEngine.Stop(row.Blur);
        row.Blur = 0;
        row.BlurPaused = false;
        if (final >= 0)
            row.Count = Math.Max(row.Count, row.BlurBase + final);
    }

    private string? ForegroundImage(uint pid, long now)
    {
        if (pid != _imagePid || now >= _imageDue)
        {
            _imagePid = pid;
            _imageName = Native.ExecutableName(pid);
            _imageDue = now + ImageCachePeriod;
        }
        return _imageName;
    }

    // Blur rows: start a run once the row may press, pause it exactly when the old
    // scheduler would skip a press, collect counts and notice runs Blur ended itself.
    private long ProcessBlur(long now)
    {
        long poll = long.MaxValue;
        foreach (var row in _rows)
        {
            if (row.Running && row.UsesBlur)
                poll = Math.Min(poll, row.Period);
        }
        if (poll == long.MaxValue)
            return long.MaxValue;

        uint pid = Native.ForegroundPid(_owner);
        if (ObserveForeground(pid))
            return long.MaxValue;

        bool changed = false;
        bool active = false;
        foreach (var row in _rows)
        {
            if (!row.Running || !row.UsesBlur)
                continue;

            bool blocked = pid == 0 || pid == OwnPid ||
                (row.Spec.App is { } app &&
                 !string.Equals(app, ForegroundImage(pid, now), StringComparison.OrdinalIgnoreCase));

            if (row.Blur == 0)
            {
                if (!blocked && !StartBlur(row))
                {
                    row.Running = false;
                    changed = true;
                    continue;
                }
                active = true;
                continue;
            }

            if (BlurEngine.Running(row.Blur) == 0)
            {
                StopBlur(row);
                row.Running = false;
                changed = true;
                continue;
            }

            if (blocked != row.BlurPaused)
            {
                BlurEngine.Pause(row.Blur, blocked ? 1 : 0);
                row.BlurPaused = blocked;
            }

            long clicks = BlurEngine.Clicks(row.Blur);
            if (clicks >= 0)
                row.Count = Math.Max(row.Count, row.BlurBase + clicks);
            active = true;
        }

        if (changed)
        {
            UpdateWatch();
            NotifyChanged();
        }

        return active ? now + Math.Clamp(poll, BlurPollMin, BlurPollMax) : long.MaxValue;
    }

    private long ProcessDue()
    {
        long now = Stopwatch.GetTimestamp();
        for (int i = _held.Count - 1; i >= 0; i--)
        {
            if (_held[i].UpDue <= now)
                Release(_held[i]);
        }

        if (_watchDue <= now)
        {
            _watchDue = now + WatchPeriod;
            ObserveForeground(Native.ForegroundPid(_owner));
        }

        if (_holdDue <= now)
            PollHolds();

        long blurDue = ProcessBlur(now);
        long next = Math.Min(Math.Min(_watchDue, _holdDue), blurDue);
        uint imagePid = 0;
        string? imageName = null;
        foreach (var row in _rows)
        {
            if (!row.Running || row.UsesBlur)
                continue;

            now = Stopwatch.GetTimestamp();
            if (row.DownDue > now)
            {
                next = Math.Min(next, row.DownDue);
                continue;
            }

            // Keep the original phase, including periods skipped by an app filter.
            long periods = (now - row.DownDue) / row.Period + 1;
            row.DownDue += periods * row.Period;
            next = Math.Min(next, row.DownDue);

            uint pid = Native.ForegroundPid(_owner);
            if (ObserveForeground(pid))
            {
                next = _holdDue;
                break;
            }
            if (pid == 0 || pid == OwnPid || row.Down)
                continue;

            if (row.Spec.App is { } app)
            {
                if (pid != imagePid)
                {
                    imagePid = pid;
                    imageName = Native.ExecutableName(pid);
                }
                if (!string.Equals(app, imageName, StringComparison.OrdinalIgnoreCase))
                    continue;
            }

            if (Native.SendStroke(row.Stroke, up: false, at: row.Spec.At))
            {
                row.Down = true;
                row.HeldStroke = row.Stroke;
                row.UpDue = Stopwatch.GetTimestamp() + row.Hold;
                row.Count++;
                _held.Add(row);
            }
        }

        foreach (var row in _held)
            next = Math.Min(next, row.UpDue);

        return next;
    }

    private void Run()
    {
        bool periodSet = Native.TimeBeginPeriod(1) == 0;
        try
        {
            using var timer = new Native.SchedulerTimer();
            WaitHandle[] waits = [_wake, timer];

            while (true)
            {
                long next;
                lock (_gate)
                {
                    if (_exiting)
                        break;
                    next = ProcessDue();
                }

                if (next == long.MaxValue)
                {
                    _wake.WaitOne();
                    continue;
                }

                long remaining = next - Stopwatch.GetTimestamp();
                if (remaining <= 0)
                    continue;

                if (timer.Arm(remaining))
                    WaitHandle.WaitAny(waits);
                else
                    _wake.WaitOne((int)Math.Ceiling(remaining * (1000.0 / Stopwatch.Frequency)));
            }
        }
        finally
        {
            try
            {
                lock (_gate)
                {
                    foreach (var row in _rows)
                        StopBlur(row);
                }

                for (int attempt = 0; attempt < 6; attempt++)
                {
                    lock (_gate)
                    {
                        ReleaseAllHeld();
                        if (_held.Count == 0)
                            break;
                    }
                    if (attempt < 5)
                        Thread.Sleep(10);
                }
            }
            finally
            {
                if (periodSet)
                    Native.TimeEndPeriod(1);
            }
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_exiting)
                return;
            _exiting = true;
            _wake.Set();
        }
        _thread.Join();
        _wake.Dispose();
    }
}
