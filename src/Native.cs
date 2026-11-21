using System;
using System.Diagnostics;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Forms;
using Microsoft.Win32.SafeHandles;

namespace F7Whisper;

internal static class Native
{
    internal const int WmMove = 0x0003;
    internal const int WmSize = 0x0005;
    internal const int WmGetMinMaxInfo = 0x0024;
    internal const int WmNcCalcSize = 0x0083;
    internal const int WmHotkey = 0x0312;
    internal const int SizeMinimized = 1;
    internal const int SizeMaximized = 2;
    internal const int HtCaption = 2;
    internal const uint ModNoRepeat = 0x4000;
    internal const uint VkF7 = 0x76;

    internal static int Scale(int dips, uint dpi) =>
        (int)Math.Round(dips * ((dpi == 0 ? 96u : dpi) / 96.0));

    internal static void ApplyFrame(nint hwnd)
    {
        SetDwmValue(hwnd, 20, 1);   // DWMWA_USE_IMMERSIVE_DARK_MODE
        SetDwmValue(hwnd, 33, 2);   // DWMWCP_ROUND
        SetDwmValue(hwnd, 34, -2);  // DWMWA_COLOR_NONE

        // Keep a DWM-managed frame for the shadow under the full-client WebView.
        var margins = new Margins { Top = 1 };
        DwmExtendFrameIntoClientArea(hwnd, ref margins);
    }

    private static void SetDwmValue(nint hwnd, int attribute, int value) =>
        DwmSetWindowAttribute(hwnd, attribute, ref value, sizeof(int));

    internal static void AdjustTracking(nint hwnd, nint address)
    {
        if (address == 0)
            return;

        uint dpi = GetDpiForWindow(hwnd);
        if (dpi == 0)
            dpi = 96;

        var info = Marshal.PtrToStructure<MinMaxInfo>(address);
        var screen = Screen.FromHandle(hwnd);
        var work = screen.WorkingArea;
        var monitor = screen.Bounds;
        int padding = GetSystemMetricsForDpi(92, dpi);           // SM_CXPADDEDBORDER
        int frameX = GetSystemMetricsForDpi(32, dpi) + padding; // SM_CXSIZEFRAME
        int frameY = GetSystemMetricsForDpi(33, dpi) + padding; // SM_CYSIZEFRAME

        info.MinTrack = new WinPoint { X = Scale(560, dpi), Y = Scale(260, dpi) };
        info.MaxPosition = new WinPoint
        {
            X = work.Left - monitor.Left - frameX,
            Y = work.Top - monitor.Top - frameY
        };
        info.MaxSize = new WinPoint
        {
            X = work.Width + 2 * frameX,
            Y = work.Height + 2 * frameY
        };
        Marshal.StructureToPtr(info, address, false);
    }

    internal static void AdjustClient(nint hwnd, nint address)
    {
        if (address == 0 || !IsZoomed(hwnd))
            return;

        // Both WM_NCCALCSIZE forms begin with the proposed RECT. The invisible
        // maximized frame can extend outside the work area on all four sides.
        var rect = Marshal.PtrToStructure<WinRect>(address);
        var proposed = Rectangle.FromLTRB(rect.Left, rect.Top, rect.Right, rect.Bottom);
        var work = Screen.FromRectangle(proposed).WorkingArea;
        rect.Left = Math.Max(rect.Left, work.Left);
        rect.Top = Math.Max(rect.Top, work.Top);
        rect.Right = Math.Min(rect.Right, work.Right);
        rect.Bottom = Math.Min(rect.Bottom, work.Bottom);
        Marshal.StructureToPtr(rect, address, false);
    }

    internal static void RestoreWindow(nint hwnd) => ShowWindow(hwnd, 9); // SW_RESTORE

    internal static int EdgeHit(string? edge) => edge switch
    {
        "w" => 10, "e" => 11, "n" => 12, "nw" => 13, "ne" => 14,
        "s" => 15, "sw" => 16, "se" => 17, _ => 0
    };

    internal static void BeginMouseOperation(nint hwnd, int hit)
    {
        if (GetAsyncKeyState(1) >= 0 || !GetCursorPos(out var point))
            return;

        // WM_NCLBUTTONDOWN expects signed screen coordinates packed into LPARAM.
        int coords = unchecked((point.X & 0xFFFF) | ((point.Y & 0xFFFF) << 16));
        ReleaseCapture();
        SendMessageW(hwnd, 0x00A1, hit, coords);
    }

    internal static bool CanPressIntoForeground(nint owner)
    {
        nint foreground = GetForegroundWindow();
        if (owner == 0 || foreground == 0 || foreground == owner)
            return false;

        nint rootOwner = GetAncestor(foreground, 3); // GA_ROOTOWNER
        return rootOwner != 0 && rootOwner != owner;
    }

    internal readonly record struct Stroke(ushort Vk, ushort Scan, bool Extended);

    internal static Stroke MapStroke(string code, int vk)
    {
        // DOM keyCode collapses left/right modifiers and changes with Num Lock.
        int mappedVk = code switch
        {
            "ShiftLeft" => 0xA0,   "ShiftRight" => 0xA1,
            "ControlLeft" => 0xA2, "ControlRight" => 0xA3,
            "AltLeft" => 0xA4,     "AltRight" => 0xA5,
            "MetaLeft" => 0x5B,    "MetaRight" => 0x5C,
            "NumpadEnter" => 0x0D, "NumpadDivide" => 0x6F,
            "NumpadDecimal" => 0x6E,
            _ when code.Length == 7 &&
                   code.StartsWith("Numpad", StringComparison.Ordinal) &&
                   code[6] is >= '0' and <= '9' => 0x60 + code[6] - '0',
            _ => vk
        };

        uint scan = MapVirtualKeyW((uint)mappedVk, 4); // MAPVK_VK_TO_VSC_EX
        bool extended = (scan >> 8) is 0xE0 or 0xE1 ||
            code is "ArrowUp" or "ArrowDown" or "ArrowLeft" or "ArrowRight"
                or "Insert" or "Delete" or "Home" or "End" or "PageUp" or "PageDown"
                or "NumpadEnter" or "NumpadDivide" or "ControlRight" or "AltRight"
                or "MetaLeft" or "MetaRight" or "ContextMenu";

        return new Stroke((ushort)mappedVk, (ushort)(scan & 0xFF), extended);
    }

    private static readonly int InputSize = Marshal.SizeOf<Input>();

    internal static bool SendStroke(Stroke stroke, bool up)
    {
        var input = new Input
        {
            Type = 1, // INPUT_KEYBOARD
            Data = new InputUnion
            {
                Keyboard = new KeyboardInput
                {
                    Vk = stroke.Vk,
                    Scan = stroke.Scan,
                    Flags = 0x0008u | (stroke.Extended ? 0x0001u : 0u) | (up ? 0x0002u : 0u)
                }
            }
        };

        return SendInput(1, ref input, InputSize) == 1;
    }

    internal sealed class SchedulerTimer : WaitHandle
    {
        internal SchedulerTimer()
        {
            const uint highResolution = 0x00000002;
            const uint modifyAndWait = 0x00100002;
            SafeWaitHandle = CreateWaitableTimerExW(0, null, highResolution, modifyAndWait);
        }

        internal bool Arm(long stopwatchTicks)
        {
            if (SafeWaitHandle.IsInvalid)
                return false;

            // Negative due times are relative and measured in 100 ns units.
            long due = -Math.Max(1L, (long)Math.Ceiling(
                stopwatchTicks * (10_000_000.0 / Stopwatch.Frequency)));
            return SetWaitableTimer(SafeWaitHandle, ref due, 0, 0, 0, false);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WinPoint { public int X, Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct WinRect { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct Margins { public int Left, Right, Top, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct MinMaxInfo
    {
        public WinPoint Reserved, MaxSize, MaxPosition, MinTrack, MaxTrack;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput
    {
        public ushort Vk, Scan;
        public uint Flags, Time;
        public nuint ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput
    {
        public int Dx, Dy;
        public uint MouseData, Flags, Time;
        public nuint ExtraInfo;
    }

    // INPUT's union is sized by MOUSEINPUT, including on x64.
    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion
    {
        [FieldOffset(0)] public KeyboardInput Keyboard;
        [FieldOffset(0)] public MouseInput Mouse;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Input
    {
        public uint Type;
        public InputUnion Data;
    }

    [DllImport("user32.dll")]
    internal static extern uint GetDpiForSystem();

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hwnd);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetricsForDpi(int index, uint dpi);

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(nint hwnd, int attribute, ref int value, int size);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(nint hwnd, ref Margins margins);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsZoomed(nint hwnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(nint hwnd, int command);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int vk);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out WinPoint point);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ReleaseCapture();

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint SendMessageW(nint hwnd, uint message, nint wParam, nint lParam);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool RegisterHotKey(nint hwnd, int id, uint modifiers, uint vk);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool UnregisterHotKey(nint hwnd, int id);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint hwnd, uint flags);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern uint MapVirtualKeyW(uint code, uint mapType);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, ref Input inputs, int size);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    private static extern SafeWaitHandle CreateWaitableTimerExW(
        nint attributes, string? name, uint flags, uint desiredAccess);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWaitableTimer(
        SafeWaitHandle timer, ref long due, int period, nint callback, nint argument,
        [MarshalAs(UnmanagedType.Bool)] bool resume);

    [DllImport("winmm.dll", EntryPoint = "timeBeginPeriod")]
    internal static extern uint TimeBeginPeriod(uint period);

    [DllImport("winmm.dll", EntryPoint = "timeEndPeriod")]
    internal static extern uint TimeEndPeriod(uint period);
}
