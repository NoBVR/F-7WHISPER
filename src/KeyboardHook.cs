using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Threading;

namespace F7Whisper;

internal sealed class KeyboardHook : IDisposable
{
    internal readonly record struct Edge(Chord Chord, int RegistrationId, bool Down);

    private readonly nint _window;
    private readonly Native.HookProc _callback;
    private readonly Thread _thread;
    private readonly ManualResetEventSlim _started = new(false);
    private readonly ManualResetEvent _stop = new(false);
    private readonly ConcurrentQueue<Edge> _events = new();
    private volatile Dictionary<Chord, int> _bindings;
    private volatile bool _running;

    // Only the hook thread changes physical state. A swallowed down owes an up
    // even if the partner is released or the binding changes in the meantime.
    private readonly bool[] _held = new bool[256];
    private readonly bool[] _swallowed = new bool[256];
    private readonly List<Edge> _active = new();

    private KeyboardHook(nint window, Dictionary<Chord, int> bindings)
    {
        _window = window;
        _bindings = bindings;
        _callback = OnKey;
        _thread = new Thread(Run) { IsBackground = true, Name = "F-7WHISPER combo hotkeys" };
    }

    internal bool Running => _running;

    internal static KeyboardHook? Start(nint window, Dictionary<Chord, int> bindings)
    {
        var hook = new KeyboardHook(window, bindings);
        hook._thread.Start();
        hook._started.Wait();
        if (hook.Running)
            return hook;

        hook.Dispose();
        return null;
    }

    internal void SetBindings(Dictionary<Chord, int> bindings) => _bindings = bindings;

    internal bool TryRead(out Edge edge) => _events.TryDequeue(out edge);

    private void Post(Edge edge)
    {
        _events.Enqueue(edge);
        Native.PostMessageW(_window, Native.WmKeyboardEdge, 0, 0);
    }

    private uint Modifiers() =>
        (_held[0xA4] || _held[0xA5] ? 1u : 0u) |
        (_held[0xA2] || _held[0xA3] ? 2u : 0u) |
        (_held[0xA0] || _held[0xA1] ? 4u : 0u) |
        (_held[0x5B] || _held[0x5C] ? 8u : 0u);

    private bool KeyUp(int vk)
    {
        _held[vk] = false;
        for (int i = _active.Count - 1; i >= 0; i--)
        {
            var edge = _active[i];
            if (edge.Chord.Vk == vk || edge.Chord.With == vk)
            {
                _active.RemoveAt(i);
                Post(edge with { Down = false });
            }
        }

        bool swallow = _swallowed[vk];
        _swallowed[vk] = false;
        return swallow;
    }

    private bool KeyDown(int vk)
    {
        if (_held[vk])
            return _swallowed[vk];

        _held[vk] = true;
        if (IsModifier(vk))
            return false;

        uint mods = Modifiers();
        var bindings = _bindings;
        for (int other = 1; other < _held.Length; other++)
        {
            if (!_held[other] || other == vk)
                continue;

            var chord = new Chord(vk, mods, other);
            if (!bindings.TryGetValue(chord, out int id))
                continue;

            // A key-up this hook never saw must not leave a key "held". A swallowed
            // key never reaches GetAsyncKeyState, but its up always comes through
            // this hook, so it is trusted as held.
            if (!_swallowed[other] && !Native.KeyDown(other))
            {
                KeyUp(other);
                continue;
            }

            var edge = new Edge(chord, id, true);
            _active.Add(edge);
            _swallowed[vk] = true;
            Post(edge);
        }

        return _swallowed[vk];
    }

    private static bool IsModifier(int vk) => vk is 0x5B or 0x5C or >= 0xA0 and <= 0xA5;

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput
    {
        public uint Vk, Scan, Flags, Time;
        public nuint ExtraInfo;
    }

    private nint OnKey(int code, nint wParam, nint lParam)
    {
        const int keyDown = 0x100, keyUp = 0x101, sysDown = 0x104, sysUp = 0x105;
        const uint injected = 0x10;
        int message = (int)wParam;
        if (code == 0 && message is keyDown or keyUp or sysDown or sysUp)
        {
            var input = Marshal.PtrToStructure<KeyboardInput>(lParam);
            if ((input.Flags & injected) == 0 && input.ExtraInfo != 0x800DA5A5u && input.Vk < 256)
            {
                int vk = input.Vk switch
                {
                    0x10 => input.Scan == 0x36 ? 0xA1 : 0xA0,
                    0x11 => (input.Flags & 1) != 0 ? 0xA3 : 0xA2,
                    0x12 => (input.Flags & 1) != 0 ? 0xA5 : 0xA4,
                    _ => (int)input.Vk
                };
                if (message is keyDown or sysDown ? KeyDown(vk) : KeyUp(vk))
                    return 1;
            }
        }
        return Native.CallNextHookEx(0, code, wParam, lParam);
    }

    private void Run()
    {
        const int whKeyboardLl = 13;
        const uint qsAllInput = 0x04FF, inputAvailable = 4, remove = 1, quit = 0x12;
        const int checkInterval = 1000;
        nint module = Native.GetModuleHandleW(null);
        nint hook = Native.SetWindowsHookExW(whKeyboardLl, _callback, module, 0);
        try
        {
            if (hook == 0)
                return;

            // Account for modifiers already down when capture ends or the switch turns on.
            for (int vk = 0; vk < _held.Length; vk++)
                if (IsModifier(vk))
                    _held[vk] = Native.KeyDown(vk);

            _running = true;
            _started.Set();
            nint stop = _stop.SafeWaitHandle.DangerousGetHandle();
            long checkAt = Environment.TickCount64 + checkInterval;
            while (true)
            {
                uint wait = (uint)Math.Max(0L, checkAt - Environment.TickCount64);
                uint result = Native.MsgWaitForMultipleObjectsEx(1, ref stop, wait, qsAllInput, inputAvailable);
                if (result != 1 && result != 258) // input or WAIT_TIMEOUT
                    break;

                if (result == 1 && Native.PeekMessageW(out var msg, 0, 0, 0, remove))
                {
                    if (msg.Message == quit)
                        break;
                    Native.DispatchMessageW(ref msg);
                }

                if (Environment.TickCount64 < checkAt)
                    continue;

                // Windows can silently retire a timed-out LL hook while its thread
                // is alive. Replace it without a gap and check the retired handle.
                nint next = Native.SetWindowsHookExW(whKeyboardLl, _callback, module, 0);
                if (next == 0)
                    break;
                bool intact = Native.UnhookWindowsHookEx(hook);
                hook = next;
                if (!intact)
                    break;
                checkAt = Environment.TickCount64 + checkInterval;
            }
        }
        finally
        {
            _running = false;
            if (hook != 0)
                Native.UnhookWindowsHookEx(hook);
            foreach (var edge in _active)
                Post(edge with { Down = false });
            _started.Set();
            Native.PostMessageW(_window, Native.WmKeyboardEdge, 0, 0);
        }
    }

    public void Dispose()
    {
        _stop.Set();
        _thread.Join();
        _stop.Dispose();
        _started.Dispose();
    }
}
