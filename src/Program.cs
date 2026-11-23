using System;
using Microsoft.VisualBasic.ApplicationServices;

namespace F7Whisper;

internal static class Program
{
    [STAThread]
    private static void Main(string[] args)
    {
        if (Updater.RunHelper(args)) return;

        // Chromium reads this before the WebView2 controller has a background color.
        Environment.SetEnvironmentVariable("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", "FF0A0A0A");
        new DesktopApplication().Run(args);
    }

    private sealed class DesktopApplication : WindowsFormsApplicationBase
    {
        internal DesktopApplication()
        {
            IsSingleInstance = true;
            EnableVisualStyles = true;
            HighDpiMode = System.Windows.Forms.HighDpiMode.PerMonitorV2;
        }

        protected override void OnCreateMainForm() => MainForm = new F7Whisper.MainForm();

        protected override void OnStartupNextInstance(StartupNextInstanceEventArgs eventArgs)
        {
            if (MainForm is { WindowState: System.Windows.Forms.FormWindowState.Minimized } form)
                Native.RestoreWindow(form.Handle);

            eventArgs.BringToForeground = true;
            base.OnStartupNextInstance(eventArgs);
        }
    }
}
