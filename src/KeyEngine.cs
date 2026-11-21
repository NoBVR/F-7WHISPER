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
            Stroke = Native.MapStroke(spec.Code, spec.Vk);
            Period = Milliseconds(spec.IntervalMs);
            Hold = Math.Min(Milliseconds(30), Period / 2);
        }
    }

    private readonly object _gate = new();
    private readonly AutoResetEvent _wake = new(false);
    private readonly List<Row> _rows;
    // A removed or edited row can still owe a key-up if Windows rejected the first one.
    private readonly List<Row> _held = new();
    private readonly Thread _thread;
    private nint _owner;
    private bool _exiting;

    internal KeyEngine(RowSpec[] rows)
    {
        _rows = rows.Select(spec => new Row(spec)).ToList();
        _thread = new Thread(Run) { IsBackground = true, Name = "F-7WHISPER key scheduler" };
        _thread.Start();
    }

    private static long Milliseconds(int ms) =>
        (long)Math.Round(ms * (Stopwatch.Frequency / 1000.0));

    internal void SetOwner(nint owner)
    {
        lock (_gate)
        {
            _owner = owner;
            _wake.Set();
        }
    }

    internal void Add(KeyChoice key, int interval)
    {
        lock (_gate)
        {
            string id;
            do
            {
                id = Guid.NewGuid().ToString("N")[..12];
            } while (Find(id) is not null);

            _rows.Add(new Row(new RowSpec(id, key.Label, key.Code, key.Vk,
                                          Settings.ClampInterval(interval))));
            _wake.Set();
        }
    }

    internal void Edit(string id, KeyChoice? key, int? interval)
    {
        lock (_gate)
        {
            if (Find(id) is not { } row)
                return;

            var spec = row.Spec;
            if (key is not null)
                spec = spec with { Label = key.Label, Code = key.Code, Vk = key.Vk };
            if (interval.HasValue)
                spec = spec with { IntervalMs = Settings.ClampInterval(interval.Value) };

            if (spec == row.Spec)
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
            _wake.Set();
        }
    }

    internal void SetAll(bool running)
    {
        lock (_gate)
        {
            foreach (var row in _rows)
                SetRunning(row, running);
            if (!running)
                ReleaseAllHeld();
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
                    spec.Id, spec.Label, spec.Code, spec.Vk, spec.IntervalMs,
                    row.Running, row.Count
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
            var row = _held[i];
            if (row.UpDue <= now)
                Release(row);
        }

        long next = long.MaxValue;
        foreach (var row in _rows)
        {
            if (!row.Running)
                continue;

            now = Stopwatch.GetTimestamp();
            if (row.DownDue <= now)
            {
                // Keep the original phase and advance over missed periods in one step.
                long periods = (now - row.DownDue) / row.Period + 1;
                row.DownDue += periods * row.Period;

                if (!row.Down && row.Stroke.Scan != 0 &&
                    Native.CanPressIntoForeground(_owner) &&
                    Native.SendStroke(row.Stroke, up: false))
                {
                    row.Down = true;
                    row.HeldStroke = row.Stroke;
                    row.UpDue = Stopwatch.GetTimestamp() + row.Hold;
                    row.Count++;
                    _held.Add(row);
                }
            }
            next = Math.Min(next, row.DownDue);
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
                // Normal key-ups succeed on the first call. Allow transient failures to clear.
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
