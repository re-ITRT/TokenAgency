# 不变量自动检查（docs/09-architecture-design.md §1.2）
#
#   I1 入口唯一      —— 只监听 127.0.0.1，没有任何 0.0.0.0
#   I2 系统网络零副作用 —— 启停前后系统代理 / 路由表 / DNS 逐字段一致
#
# 用法：
#   pwsh -File scripts/check-invariants.ps1 -Mode Snapshot -Out before.json
#   # …启动 TokenAgency，跑几个请求…
#   pwsh -File scripts/check-invariants.ps1 -Mode Compare -Baseline before.json
#
#   # 一键端到端（需要已构建出 exe）：
#   pwsh -File scripts/check-invariants.ps1 -Mode Run -Exe target\debug\tokenagency.exe
#
# 退出码：0 = 通过，1 = 违反不变量

[CmdletBinding()]
param(
    [ValidateSet("Snapshot", "Compare", "Run")]
    [string]$Mode = "Snapshot",
    [string]$Out = "invariants-snapshot.json",
    [string]$Baseline,
    [string]$Exe,
    [int]$AppWarmupSeconds = 12,
    [int]$ObserveSeconds = 20
)

$ErrorActionPreference = "Stop"

# 我们自己的进程名（出现这些名字的监听才算"我们的"）
$OurProcesses = @("tokenagency", "mihomo", "clash-meta", "clash")
# 允许监听的回环端口（内核 REST + lane 入站落在 39100-39299，见 docs/11 §2）
$AllowedLoopback = @(8787, 8788, 39090)
$LanePortRange = @(39100, 39299)

function Get-ListeningSockets {
    $rows = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue
    $result = foreach ($r in $rows) {
        $proc = Get-Process -Id $r.OwningProcess -ErrorAction SilentlyContinue
        [pscustomobject]@{
            Address   = $r.LocalAddress
            Port      = $r.LocalPort
            Process   = if ($proc) { $proc.ProcessName } else { "pid-$($r.OwningProcess)" }
            ProcessId = $r.OwningProcess
        }
    }
    return @($result | Sort-Object Address, Port)
}

function Get-ProxyRegistry {
    $k = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
    $p = Get-ItemProperty -Path $k -ErrorAction SilentlyContinue
    return [pscustomobject]@{
        ProxyEnable    = $p.ProxyEnable
        ProxyServer    = $p.ProxyServer
        ProxyOverride  = $p.ProxyOverride
        AutoConfigURL  = $p.AutoConfigURL
        AutoDetect     = $p.AutoDetect
    }
}

function Get-WinHttpProxy {
    $x = & netsh winhttp show proxy 2>&1 | Out-String
    return $x.Trim()
}

function Get-Routes {
    # 只取 IPv4 路由表的持久部分（去掉易变的 Active 计数与统计）
    $x = & route print -4 2>&1 | Out-String
    $lines = $x -split "`r?`n" | Where-Object { $_ -match '^\s*\d+\.\d+\.\d+\.\d+' }
    return @($lines | ForEach-Object { $_.Trim() -replace '\s+', ' ' } | Sort-Object)
}

function Get-DnsServers {
    $rows = Get-DnsClientServerAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue
    return @($rows | ForEach-Object { "$($_.InterfaceAlias)|$($_.ServerAddresses -join ',')" } | Sort-Object)
}

function Get-TunAdapters {
    # TUN 模式会新增虚拟网卡 —— 这是我们"不接管整机"承诺的直接证据
    $rows = Get-NetAdapter -ErrorAction SilentlyContinue
    return @($rows | Where-Object { $_.InterfaceDescription -match 'TAP|TUN|WireGuard|wintun|Clash|Mihomo' } |
        ForEach-Object { "$($_.Name)|$($_.InterfaceDescription)|$($_.Status)" } | Sort-Object)
}

function Get-OurProcesses {
    $rows = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $OurProcesses -contains $_.ProcessName.ToLower() }
    return @($rows | ForEach-Object { "$($_.ProcessName)#$($_.Id)" } | Sort-Object)
}

# 开机自启动条目（docs/09 §1.2 E5：唯一会写 OS 状态的功能，列为观察项）
#
# 语义：**允许它存在**（那是用户开的），但任何变化都必须能在应用日志里
# 找到对应的用户操作。所以 Compare 模式只报告，Run 模式视为违规
# —— 自动化跑测期间不可能有用户去点开关。
function Get-AutostartEntries {
    $out = @()
    if ($IsWindows -or $env:OS -eq 'Windows_NT') {
        # HKCU Run 键：当前用户级，免提权（与我们的免提权承诺一致）
        foreach ($key in @(
                "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run",
                "HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce")) {
            $p = Get-ItemProperty -Path $key -ErrorAction SilentlyContinue
            if ($null -eq $p) { continue }
            $p.PSObject.Properties |
                Where-Object { $_.Name -notmatch '^PS' } |
                ForEach-Object { $out += "$key :: $($_.Name)=$($_.Value)" }
        }
    }
    elseif ($IsMacOS) {
        $dir = Join-Path $HOME "Library/LaunchAgents"
        if (Test-Path $dir) {
            Get-ChildItem $dir -Filter *.plist -ErrorAction SilentlyContinue |
                ForEach-Object { $out += "~/Library/LaunchAgents/$($_.Name)" }
        }
    }
    else {
        $dir = Join-Path $HOME ".config/autostart"
        if (Test-Path $dir) {
            Get-ChildItem $dir -Filter *.desktop -ErrorAction SilentlyContinue |
                ForEach-Object { $out += "~/.config/autostart/$($_.Name)" }
        }
    }
    return @($out | Sort-Object)
}

# 我们自己写的自启动条目长什么样（用于在报告里高亮，不是判据）
function Test-IsOurAutostartEntry($line) {
    return ($line -match 'TokenAgency' -or $line -match 'tokenagency')
}

function Take-Snapshot {
    return [pscustomobject]@{
        TakenAt   = (Get-Date).ToString("o")
        Host      = $env:COMPUTERNAME
        Proxy     = Get-ProxyRegistry
        WinHttp   = Get-WinHttpProxy
        Routes    = Get-Routes
        Dns       = Get-DnsServers
        TunIfaces = Get-TunAdapters
        Processes = Get-OurProcesses
        Autostart = Get-AutostartEntries
        Listeners = Get-ListeningSockets
    }
}

# 自启动条目对比：返回变化描述（观察项，不是 I2 违规）
function Compare-Autostart($before, $after) {
    $a = @($before.Autostart | Sort-Object)
    $b = @($after.Autostart | Sort-Object)
    $added = @($b | Where-Object { $a -notcontains $_ })
    $removed = @($a | Where-Object { $b -notcontains $_ })
    if ($added.Count -eq 0 -and $removed.Count -eq 0) { return @() }
    $out = @()
    foreach ($x in $added) {
        $mark = if (Test-IsOurAutostartEntry $x) { "【我们的】" } else { "" }
        $out += "新增 ${mark}$x"
    }
    foreach ($x in $removed) {
        $mark = if (Test-IsOurAutostartEntry $x) { "【我们的】" } else { "" }
        $out += "移除 ${mark}$x"
    }
    return $out
}

function Test-Invariants($snap) {
    # 返回 @{ Violations = @(); Notes = @() }
    #
    # 重要设计修正：**I2 是"启停前后一致"，不是"必须是关的"。**
    # 用户机器上本来就可能开着系统代理 / 有 TUN 网卡（那是他自己的其它软件）。
    # TokenAgency 既不能打开它们，**也不能替他关掉** —— 擅自关闭同样是副作用。
    # 所以这里只做 I1 的绝对检查，I2 一律交给 Compare-Snapshots 做前后对比。
    $violations = @()
    $notes = @()

    # ---- I1：我们的进程只允许回环监听 ----
    foreach ($l in $snap.Listeners) {
        $isOurs = $OurProcesses -contains $l.Process.ToLower()
        if (-not $isOurs) { continue }

        if ($l.Address -notin @("127.0.0.1", "::1")) {
            $violations += "I1 违反：$($l.Process) 监听在 $($l.Address):$($l.Port)（非回环，违反'入口唯一'）"
            continue
        }
        $okPort = ($AllowedLoopback -contains $l.Port) -or
        ($l.Port -ge $LanePortRange[0] -and $l.Port -le $LanePortRange[1])
        if (-not $okPort) {
            $notes += "I1 提醒：$($l.Process) 监听回环端口 $($l.Port)，不在预期清单内"
        }
    }

    # ---- 环境说明（不是违规，只是让报告可读）----
    if ($snap.Proxy.ProxyEnable -eq 1) {
        $notes += "环境：系统代理本来就开着（$($snap.Proxy.ProxyServer)）。不变量要求的是【前后一致】，即我们既不打开也不关闭它。"
    }
    if ($snap.TunIfaces.Count -gt 0) {
        $notes += "环境：已有 $($snap.TunIfaces.Count) 个 TUN/TAP 网卡（其它软件装的）。只有【新增】才算我们违规。"
    }

    return [pscustomobject]@{ Violations = @($violations); Notes = @($notes) }
}

function Compare-Snapshots($before, $after) {
    $diffs = @()

    function Cmp($label, $a, $b) {
        $sa = ($a | ConvertTo-Json -Compress -Depth 5)
        $sb = ($b | ConvertTo-Json -Compress -Depth 5)
        if ($sa -ne $sb) {
            $script:diffs += "I2 违反：$label 发生变化"
            $script:diffs += "    before: $sa"
            $script:diffs += "    after : $sb"
        }
    }

    Cmp "系统代理注册表" $before.Proxy $after.Proxy
    Cmp "WinHTTP 代理" $before.WinHttp $after.WinHttp
    Cmp "IPv4 路由表" $before.Routes $after.Routes
    Cmp "DNS 服务器" $before.Dns $after.Dns
    Cmp "TUN 虚拟网卡" $before.TunIfaces $after.TunIfaces

    return $diffs
}

# ================================================================= 主流程

switch ($Mode) {
    "Snapshot" {
        $snap = Take-Snapshot
        $snap | ConvertTo-Json -Depth 6 | Set-Content -Path $Out -Encoding utf8
        Write-Host "已保存快照：$Out" -ForegroundColor Green
        $r = Test-Invariants $snap
        if ($r.Violations.Count -gt 0) {
            $r.Violations | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        }
        else {
            Write-Host "  ✔ I1：我们的进程没有非回环监听" -ForegroundColor Green
        }
        $r.Notes | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
        exit 0
    }

    "Compare" {
        if (-not $Baseline -or -not (Test-Path $Baseline)) {
            Write-Host "缺少 -Baseline 文件" -ForegroundColor Red; exit 1
        }
        $before = Get-Content $Baseline -Raw | ConvertFrom-Json
        $after = Take-Snapshot

        $diffs = Compare-Snapshots $before $after
        $r = Test-Invariants $after

        Write-Host ""
        Write-Host "=== I2 系统网络零副作用（前后一致性）===" -ForegroundColor Cyan
        if ($diffs.Count -eq 0) {
            Write-Host "  ✔ 系统代理 / 路由表 / DNS / 虚拟网卡 逐字段一致" -ForegroundColor Green
        }
        else {
            $diffs | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        }

        Write-Host ""
        Write-Host "=== I1 入口唯一（仅回环监听）===" -ForegroundColor Cyan
        if ($r.Violations.Count -eq 0) {
            Write-Host "  ✔ 无 0.0.0.0 监听，端口均在预期内" -ForegroundColor Green
        }
        else {
            $r.Violations | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        }
        $r.Notes | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }

        $autoChanges = Compare-Autostart $before $after
        Write-Host ""
        Write-Host "=== 开机自启动条目（观察项，非 I2 违规）===" -ForegroundColor Cyan
        if ($autoChanges.Count -eq 0) {
            Write-Host "  ✔ 与启动前一致" -ForegroundColor Green
            $ours = @($after.Autostart | Where-Object { Test-IsOurAutostartEntry $_ })
            if ($ours.Count -gt 0) {
                Write-Host "  当前存在我们的启动项（应是用户在设置里开的）：" -ForegroundColor DarkGray
                $ours | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
            }
            else {
                Write-Host "  当前没有我们的启动项（默认状态，符合 E1）" -ForegroundColor DarkGray
            }
        }
        else {
            Write-Host "  ⚠ 自启动条目发生变化 —— 请确认这是你在设置里操作的结果：" -ForegroundColor Yellow
            $autoChanges | ForEach-Object { Write-Host "    $_" -ForegroundColor Yellow }
            Write-Host "  （按 E5：允许变化，但必须能在应用日志里找到对应的用户操作）" -ForegroundColor DarkGray
        }

        Write-Host ""
        Write-Host "=== 当前我们的进程 ===" -ForegroundColor DarkGray
        if ($after.Processes.Count -eq 0) { Write-Host "  （无）" -ForegroundColor DarkGray }
        $after.Processes | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }

        if ($diffs.Count -gt 0 -or $r.Violations.Count -gt 0) { exit 1 }
        Write-Host ""
        Write-Host "✔ 不变量全部满足" -ForegroundColor Green
        exit 0
    }

    "Run" {
        if (-not $Exe -or -not (Test-Path $Exe)) {
            Write-Host "缺少 -Exe（未构建？先 cargo build）" -ForegroundColor Red; exit 1
        }

        $tmp = Join-Path $env:TEMP "ta-invariants-before.json"
        Write-Host "[1/4] 启动前快照 ..." -ForegroundColor Cyan
        (Take-Snapshot) | ConvertTo-Json -Depth 6 | Set-Content -Path $tmp -Encoding utf8

        Write-Host "[2/4] 启动 $Exe ..." -ForegroundColor Cyan
        $proc = Start-Process -FilePath $Exe -PassThru
        try {
            Start-Sleep -Seconds $AppWarmupSeconds
            Write-Host "[3/4] 观测 $ObserveSeconds 秒（此时可手动打几个请求）..." -ForegroundColor Cyan
            Start-Sleep -Seconds $ObserveSeconds
        }
        finally {
            Write-Host "      结束进程 ..." -ForegroundColor DarkGray
            Get-Process -Name "tokenagency", "mihomo" -ErrorAction SilentlyContinue |
                Stop-Process -Force -ErrorAction SilentlyContinue
            Start-Sleep -Seconds 3
        }

        Write-Host "[4/4] 对比 ..." -ForegroundColor Cyan
        $before = Get-Content $tmp -Raw | ConvertFrom-Json
        $after = Take-Snapshot

        $leftover = $after.Processes
        $diffs = Compare-Snapshots $before $after
        $r = Test-Invariants $after
        # 自动化跑测期间不可能有人去点"开机自启动"开关 —— 所以这里出现变化就是违规（E5）
        $autoChanges = @(Compare-Autostart $before $after | Where-Object { Test-IsOurAutostartEntry $_ })

        Write-Host ""
        if ($leftover.Count -gt 0) {
            Write-Host "  I2 违反：退出后仍有残留进程 —— $($leftover -join ', ')" -ForegroundColor Red
        }
        if ($autoChanges.Count -gt 0) {
            Write-Host "  I2/E5 违反：自动化跑测期间自启动条目被改动（无人操作却变化）：" -ForegroundColor Red
            $autoChanges | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
        }
        if ($diffs.Count -eq 0 -and $r.Violations.Count -eq 0 -and $leftover.Count -eq 0 -and $autoChanges.Count -eq 0) {
            Write-Host "✔ 端到端不变量检查通过（I1 + I2 + 自启动 E5）" -ForegroundColor Green
            exit 0
        }
        $diffs | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        $r.Violations | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        exit 1
    }
}
