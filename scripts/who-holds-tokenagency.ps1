# **谁占着 tokenagency.exe？** —— 安装器报「Error opening file for writing」时先跑这个。
#
# 用法：
#   pwsh -File scripts/who-holds-tokenagency.ps1
#
# 它做三件事（都是只读，不改任何东西）：
#   1. 现在能不能对安装目录里的 exe 取独占写句柄（= 有没有被占）；
#   2. 用 Windows **Restart Manager**（`rstrtmgr.dll`）列出**到底哪个进程/服务**占着它 ——
#      这是系统自己的 API，比"看任务管理器猜"准得多；
#   3. 顺手列出：路径里带 TokenAgency 的进程、计划任务、自启项（这三处是"托盘里看不见但它在跑"的常见来源）。
#
# 结论怎么用：
#   · 第 ② 步列出进程 → 那就是凶手，`taskkill /F /IM <名字>` 或从托盘退出；
#   · 第 ② 步是 `MsMpEng.exe`（Windows Defender）/ 杀软 → 那是**实时扫描**，等几秒重试即可；
#   · 全都没有、第 ① 步也"可写" → 那一刻的占用是**短时的**（装的时候正好被扫），直接重跑安装器。

$ErrorActionPreference = 'Continue'
$exe = Join-Path $env:LOCALAPPDATA 'TokenAgency\tokenagency.exe'

Write-Host "== 目标：$exe" -ForegroundColor Cyan
if (-not (Test-Path $exe)) {
  Write-Host "   文件不存在（没装过，或已经卸载）—— 那就直接装。" -ForegroundColor Yellow
  return
}

# ① 独占写句柄
try {
  $fs = [IO.File]::Open($exe, 'Open', 'ReadWrite', 'None')
  $fs.Close()
  Write-Host "① 独占写句柄：成功 ⇒ 此刻**没有**被占" -ForegroundColor Green
} catch {
  Write-Host "① 独占写句柄：失败 ⇒ **此刻被占用**（$($_.Exception.Message)）" -ForegroundColor Red
}

# ② Restart Manager：列占用者
$src = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class TaLock {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct RM_UNIQUE_PROCESS {
        public int dwProcessId;
        public System.Runtime.InteropServices.ComTypes.FILETIME ProcessStartTime;
    }
    const int CCH_RM_MAX_APP_NAME = 255;
    const int CCH_RM_MAX_SVC_NAME = 63;
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct RM_PROCESS_INFO {
        public RM_UNIQUE_PROCESS Process;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = CCH_RM_MAX_APP_NAME + 1)] public string strAppName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = CCH_RM_MAX_SVC_NAME + 1)] public string strServiceShortName;
        public int ApplicationType;
        public uint AppStatus;
        public uint TSSessionId;
        [MarshalAs(UnmanagedType.Bool)] public bool bRestartable;
    }
    [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
    static extern int RmStartSession(out uint pSessionHandle, int dwSessionFlags, StringBuilder strSessionKey);
    [DllImport("rstrtmgr.dll")]
    static extern int RmEndSession(uint pSessionHandle);
    [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
    static extern int RmRegisterResources(uint pSessionHandle, uint nFiles, string[] rgsFilenames,
        uint nApplications, RM_UNIQUE_PROCESS[] rgApplications, uint nServices, string[] rgsServiceNames);
    [DllImport("rstrtmgr.dll")]
    static extern int RmGetList(uint dwSessionHandle, out uint pnProcInfoNeeded, ref uint pnProcInfo,
        [In, Out] RM_PROCESS_INFO[] rgAffectedApps, ref uint lpdwRebootReasons);

    public static List<string> Who(string path) {
        var res = new List<string>();
        uint session = 0;
        var key = new StringBuilder(255);
        if (RmStartSession(out session, 0, key) != 0) { res.Add("RmStartSession 失败（拿不到会话）"); return res; }
        try {
            if (RmRegisterResources(session, 1, new[] { path }, 0, null, 0, null) != 0) {
                res.Add("RmRegisterResources 失败"); return res;
            }
            uint needed = 0, count = 0, reasons = 0;
            int rc = RmGetList(session, out needed, ref count, null, ref reasons);
            if (rc == 234) {
                var arr = new RM_PROCESS_INFO[needed];
                count = needed;
                rc = RmGetList(session, out needed, ref count, arr, ref reasons);
                if (rc == 0) {
                    for (int i = 0; i < count; i++) {
                        res.Add("pid=" + arr[i].Process.dwProcessId + " 应用=" + arr[i].strAppName
                                + (string.IsNullOrEmpty(arr[i].strServiceShortName) ? "" : (" 服务=" + arr[i].strServiceShortName))
                                + " 会话=" + arr[i].TSSessionId);
                    }
                }
            } else if (rc != 0) { res.Add("RmGetList rc=" + rc); }
            if (res.Count == 0) res.Add("（Restart Manager：没有任何进程/服务占用它）");
        } finally { RmEndSession(session); }
        return res;
    }
}
'@
try {
  if (-not ('TaLock' -as [type])) { Add-Type -TypeDefinition $src -Language CSharp | Out-Null }
  Write-Host "② Restart Manager 报告的占用者：" -ForegroundColor Cyan
  [TaLock]::Who($exe) | ForEach-Object { Write-Host "   $_" }
} catch {
  Write-Host "② Restart Manager 查不了（$($_.Exception.Message)）—— 那就看下面第 ③ 步的清单" -ForegroundColor Yellow
}

# ③ 三处"托盘看不见但在跑"的常见来源
Write-Host "③ 相关进程 / 计划任务 / 自启项：" -ForegroundColor Cyan
$procs = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -match 'tokenagency' -or $_.Path -like '*TokenAgency*' }
if ($procs) { $procs | Select-Object Id, ProcessName, Path, SessionId | Format-Table -AutoSize | Out-String | Write-Host }
else { Write-Host "   进程：没有" }

$tasks = Get-ScheduledTask -ErrorAction SilentlyContinue | Where-Object {
  $_.TaskName -match 'TokenAgency' -or (($_.Actions | ForEach-Object { $_.Execute }) -join ' ') -match 'TokenAgency'
}
if ($tasks) { $tasks | Select-Object TaskName, State | Format-Table -AutoSize | Out-String | Write-Host }
else { Write-Host "   计划任务：没有" }

$runs = @()
foreach ($k in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Run', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run')) {
  $item = Get-ItemProperty $k -ErrorAction SilentlyContinue
  if ($item) {
    $item.PSObject.Properties | Where-Object { $_.Name -notmatch '^PS' -and "$($_.Value)" -match 'TokenAgency' } | ForEach-Object {
      $runs += "$k :: $($_.Name) = $($_.Value)"
    }
  }
}
if ($runs) { $runs | ForEach-Object { Write-Host "   自启：$_" } } else { Write-Host "   自启项：没有" }
