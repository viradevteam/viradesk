using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

internal static class ViraDeskStart
{
    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int nIndex);

    private static int Main()
    {
        string dir = Path.GetDirectoryName(typeof(ViraDeskStart).Assembly.Location);
        string exe = Path.Combine(dir, "ViraDesk.exe");
        // A normal desktop's remote view is a GPU texture. Forcing SwiftShader
        // leaves that texture black. Only an RDP session needs software ANGLE
        // so the rest of the window is not blank.
        if (GetSystemMetrics(0x1000) != 0)
        {
            Environment.SetEnvironmentVariable("ANGLE_DEFAULT_PLATFORM", "swiftshader");
        }
        else
        {
            Environment.SetEnvironmentVariable("ANGLE_DEFAULT_PLATFORM", null);
        }
        Process.Start(new ProcessStartInfo
        {
            FileName = exe,
            WorkingDirectory = dir,
            UseShellExecute = false
        });
        return 0;
    }
}
