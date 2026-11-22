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
        }
    }

    private static readonly long WatchPeriod = Milliseconds(75);
    private static readonly uint OwnPid = (uint)Environment.ProcessId;

    private readonly object _gate = new();
    private readonly AutoResetEvent _wake = new(false);
    private readonly List<Row> _rows;
    // A removed or reconfigured row can still owe its original release.
    private readonly List<Row> _held = new();
    private readonly Thread _thread;
    private nint _owner;
    private bool _switchStop;
    private bool _stoppedOnSwitch;
    private uint _targetPid;
    private long _watchDue = long.MaxValue;
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

            _rows.Add(new Row(spec with { Id = id }));
            _wake.Set();
        }
    }

    internal void Edit(RowSpec spec)
    {
        lock (_gate)
        {
            if (Find(spec.Id) is not { } row)
                return;

            Release(row);
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
            if (Find(id) is not { } row)
                return;

            SetRunning(row, !row.Running);
            UpdateWatch();
            _wake.Set();
        }
    }

    internal void SetAll(bool running)
    {
        lock (_gate)
        {
            if (running)
            {
                foreach (var row in _rows)
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

    private Row? Find(string id) => _rows.Find(row => row.Spec.Id == id);

    private void SetRunning(Row row, bool running)
    {
        if (running && !row.Running)
            row.DownDue = Stopwatch.GetTimestamp();
        row.Running = running;
        if (!running)
            Release(row);
    }

    private void StopAllLocked()
    {
        foreach (var row in _rows)
            row.Running = false;
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
        _stoppedOnSwitch = true;
        return true;
    }

    internal bool TakeSwitchStopNotification()
    {
        lock (_gate)
        {
            bool pending = _stoppedOnSwitch;
            _stoppedOnSwitch = false;
            return pending;
        }
    }

    internal RowSpec[] Configuration()
    {
        lock (_gate)
            return _rows.Select(row => row.Spec).ToArray();
    }

    internal object[] Snapshot()
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
                    spec.Button, spec.At, spec.App, spec.IntervalMs, row.Running, row.Count
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

        long next = _watchDue;
        uint imagePid = 0;
        string? imageName = null;
        foreach (var row in _rows)
        {
            if (!row.Running)
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
                next = long.MaxValue;
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
