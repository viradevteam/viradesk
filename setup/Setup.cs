using System;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Windows.Forms;

[assembly: AssemblyTitle("ViraDesk Setup")]
[assembly: AssemblyProduct("ViraDesk")]

namespace ViraDesk.Setup
{
    internal static class Program
    {
        [System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool AttachConsole(int dwProcessId);

        [STAThread]
        private static int Main(string[] args)
        {
            if (Has(args, "--install"))
            {
                AttachConsole(-1);
                try
                {
                    Console.WriteLine(Installer.Install());
                    return 0;
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine(ex.Message);
                    return 1;
                }
            }

            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new SetupForm());
            return 0;
        }

        private static bool Has(string[] args, string flag)
        {
            int i;
            for (i = 0; i < args.Length; i++)
            {
                if (string.Equals(args[i], flag, StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
            return false;
        }
    }

    internal static class Installer
    {
        [System.Runtime.InteropServices.DllImport("user32.dll")]
        private static extern int GetSystemMetrics(int nIndex);

        internal static string Install()
        {
            string dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "ViraDesk");
            string staging = Path.Combine(Path.GetTempPath(), "viradesk-setup-" + Guid.NewGuid().ToString("N"));
            string zipPath = Path.Combine(staging, "payload.zip");
            Directory.CreateDirectory(staging);
            try
            {
                using (Stream input = Assembly.GetExecutingAssembly().GetManifestResourceStream("Payload.app.zip"))
                {
                    if (input == null)
                    {
                        throw new InvalidOperationException("بستهٔ برنامه داخل نصب‌کننده پیدا نشد.");
                    }
                    using (FileStream output = File.Create(zipPath))
                    {
                        input.CopyTo(output);
                    }
                }
                string extracted = Path.Combine(staging, "app");
                ZipFile.ExtractToDirectory(zipPath, extracted);
                Directory.CreateDirectory(dir);
                CopyTree(extracted, dir);
            }
            finally
            {
                try
                {
                    Directory.Delete(staging, true);
                }
                catch (IOException)
                {
                }
            }
            string exe = Path.Combine(dir, "ViraDesk.exe");
            if (!File.Exists(exe))
            {
                throw new InvalidOperationException("ViraDesk.exe بعد از نصب پیدا نشد.");
            }
            // SwiftShader is only for an RDP session, where Flutter's DirectX
            // path leaves the window blank. On a normal desktop it blacks out
            // the remote-view texture, so a previous install must clear it.
            if (GetSystemMetrics(0x1000) != 0)
            {
                Environment.SetEnvironmentVariable("ANGLE_DEFAULT_PLATFORM", "swiftshader", EnvironmentVariableTarget.User);
            }
            else if (string.Equals(
                Environment.GetEnvironmentVariable("ANGLE_DEFAULT_PLATFORM", EnvironmentVariableTarget.User),
                "swiftshader",
                StringComparison.OrdinalIgnoreCase))
            {
                Environment.SetEnvironmentVariable("ANGLE_DEFAULT_PLATFORM", null, EnvironmentVariableTarget.User);
            }
            CreateStartShortcut(Path.Combine(dir, "ViraDesk-Start.exe"));
            return exe;
        }

        private static void CopyTree(string source, string dest)
        {
            foreach (string directory in Directory.GetDirectories(source, "*", SearchOption.AllDirectories))
            {
                string relative = directory.Substring(source.Length).TrimStart(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
                Directory.CreateDirectory(Path.Combine(dest, relative));
            }
            foreach (string file in Directory.GetFiles(source, "*", SearchOption.AllDirectories))
            {
                string relative = file.Substring(source.Length).TrimStart(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
                string target = Path.Combine(dest, relative);
                Directory.CreateDirectory(Path.GetDirectoryName(target));
                File.Copy(file, target, true);
            }
        }

        private static void CreateStartShortcut(string target)
        {
            string programs = Environment.GetFolderPath(Environment.SpecialFolder.Programs);
            Directory.CreateDirectory(programs);
            string link = Path.Combine(programs, "ViraDesk.lnk");
            Type shellType = Type.GetTypeFromProgID("WScript.Shell");
            object shell = Activator.CreateInstance(shellType);
            object shortcut = shellType.InvokeMember(
                "CreateShortcut",
                System.Reflection.BindingFlags.InvokeMethod,
                null,
                shell,
                new object[] { link });
            Type shortcutType = shortcut.GetType();
            shortcutType.InvokeMember("TargetPath", System.Reflection.BindingFlags.SetProperty, null, shortcut, new object[] { target });
            shortcutType.InvokeMember("WorkingDirectory", System.Reflection.BindingFlags.SetProperty, null, shortcut, new object[] { Path.GetDirectoryName(target) });
            shortcutType.InvokeMember("Description", System.Reflection.BindingFlags.SetProperty, null, shortcut, new object[] { "ViraDesk" });
            shortcutType.InvokeMember("Save", System.Reflection.BindingFlags.InvokeMethod, null, shortcut, new object[0]);
        }
    }

    internal sealed class SetupForm : Form
    {
        private readonly Label statusLabel;
        private readonly Button openButton;

        internal SetupForm()
        {
            Text = "ViraDesk Setup";
            FormBorderStyle = FormBorderStyle.FixedDialog;
            MaximizeBox = false;
            MinimizeBox = false;
            StartPosition = FormStartPosition.CenterScreen;
            ClientSize = new System.Drawing.Size(480, 230);
            RightToLeft = RightToLeft.Yes;
            RightToLeftLayout = true;
            Font = new System.Drawing.Font("Tahoma", 10f);

            Label intro = new Label();
            intro.AutoSize = false;
            intro.Location = new System.Drawing.Point(16, 16);
            intro.Size = new System.Drawing.Size(448, 78);
            intro.Text = "ViraDesk را برای همین کاربر نصب می‌کند و میانبر منوی شروع می‌سازد."
                + "\r\nبرنامهٔ نصب‌شده: ViraDesk.exe";

            statusLabel = new Label();
            statusLabel.AutoSize = false;
            statusLabel.Location = new System.Drawing.Point(16, 100);
            statusLabel.Size = new System.Drawing.Size(448, 56);
            statusLabel.Text = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "ViraDesk",
                "ViraDesk.exe");

            Button installButton = new Button();
            installButton.Text = "نصب";
            installButton.Location = new System.Drawing.Point(16, 176);
            installButton.Size = new System.Drawing.Size(120, 34);
            installButton.Click += OnInstall;

            openButton = new Button();
            openButton.Text = "اجرای ViraDesk";
            openButton.Location = new System.Drawing.Point(148, 176);
            openButton.Size = new System.Drawing.Size(150, 34);
            openButton.Enabled = false;
            openButton.Click += OnOpen;

            Controls.Add(intro);
            Controls.Add(statusLabel);
            Controls.Add(installButton);
            Controls.Add(openButton);
        }

        private void OnInstall(object sender, EventArgs e)
        {
            try
            {
                string dest = Installer.Install();
                statusLabel.Text = "نصب شد:\r\n" + dest;
                openButton.Enabled = true;
            }
            catch (Exception ex)
            {
                statusLabel.Text = ex.Message;
            }
        }

        private void OnOpen(object sender, EventArgs e)
        {
            string dest = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "ViraDesk",
                "ViraDesk.exe");
            System.Diagnostics.ProcessStartInfo start = new System.Diagnostics.ProcessStartInfo();
            start.FileName = dest;
            start.WorkingDirectory = Path.GetDirectoryName(dest);
            start.UseShellExecute = false;
            start.EnvironmentVariables["ANGLE_DEFAULT_PLATFORM"] = "swiftshader";
            System.Diagnostics.Process.Start(start);
        }
    }
}
