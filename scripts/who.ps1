# TokenAgency 改动溯源 —— 回答"这是谁改的"
#
# 为什么要这个脚本：主管曾据文件时间戳**推断**是某员工写了 ta-subscription，
# 结果推错了人。不同提交身份 + 这个脚本，能把"推断"换成"查询"。
#
# ⚠️ 重要局限：本脚本只能看到**已提交**的内容。
#    未提交的在途文件无法归属 —— 那正是 L17 踩的坑。
#    所以配套约定是：**员工每完成一个可编译的里程碑就提交一次**，
#    让"在途窗口"尽量短。
#
# 用法：
#   pwsh -File scripts/who.ps1                          # 各身份改了多少文件
#   pwsh -File scripts/who.ps1 -Path crates/ta-subscription   # 这个目录谁在改
#   pwsh -File scripts/who.ps1 -File crates/ta-core/src/types.rs  # 单个文件的历史
#   pwsh -File scripts/who.ps1 -Author E4               # 某个身份改了什么
#   pwsh -File scripts/who.ps1 -Uncommitted             # 在途未提交的文件（按范围粗归因）

[CmdletBinding()]
param(
    [string]$Path,
    [string]$File,
    [string]$Author,
    [switch]$Uncommitted,
    [int]$Limit = 40
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Get-AuthorColor($name) {
    switch -Regex ($name) {
        '技术主管' { 'Magenta' }
        'E1' { 'Cyan' }
        'E2' { 'Green' }
        'E3' { 'Yellow' }
        'E4' { 'Blue' }
        'E5' { 'Red' }
        default { 'Gray' }
    }
}

# ---------------------------------------------------------------- 在途（未提交）
if ($Uncommitted) {
    Write-Host ""
    Write-Host "=== 在途未提交的改动 ===" -ForegroundColor Cyan
    Write-Host "注意：git 无法归属未提交的改动，下列分组是按 docs/12 §1.2 的范围**粗归因**。" -ForegroundColor DarkGray
    Write-Host "     如果某文件出现在这里而不是某次提交里，说明它的作者还没交付。" -ForegroundColor DarkGray
    Write-Host ""

    $scopes = [ordered]@{
        'E1 领域官' = @('crates/ta-core/src/domain', 'crates/ta-core/src/config', 'config.example.yaml')
        'E2 决策官' = @('crates/ta-core/src/routing', 'crates/ta-core/src/sticky',
            'crates/ta-core/src/quota', 'crates/ta-core/src/pricing', 'crates/ta-core/src/metering')
        'E3 数据官' = @('crates/ta-store', 'crates/ta-secrets')
        'E4 网络官' = @('crates/ta-kernel', 'crates/ta-subscription')
        'E5 网关官' = @('crates/ta-gateway', 'src-tauri', 'src/pages', 'src/app', 'src/stores', 'src/shared')
        '技术主管' = @('Cargo.toml', 'docs', 'scripts', '.githooks', 'crates/ta-core/src/lib.rs',
            'crates/ta-core/src/types.rs', 'crates/ta-core/src/error.rs', 'crates/ta-core/src/clock.rs',
            'src/main.tsx', 'src/App.tsx', 'src/theme.ts')
    }

    $changed = @(git status --porcelain | ForEach-Object { $_.Substring(3).Trim('"') } |
        Where-Object { $_ -and $_ -notmatch ' -> ' })
    $changed += @(git status --porcelain | Where-Object { $_ -match ' -> ' } |
        ForEach-Object { ($_ -split ' -> ')[1].Trim('"') })

    $hit = @{}
    $orphan = @()
    foreach ($f in ($changed | Sort-Object -Unique)) {
        $matched = $false
        foreach ($k in $scopes.Keys) {
            foreach ($s in $scopes[$k]) {
                if ($f -eq $s -or $f.StartsWith("$s/")) {
                    if (-not $hit.ContainsKey($k)) { $hit[$k] = @() }
                    $hit[$k] += $f
                    $matched = $true
                    break
                }
            }
            if ($matched) { break }
        }
        if (-not $matched) { $orphan += $f }
    }

    foreach ($k in $scopes.Keys) {
        if (-not $hit.ContainsKey($k)) { continue }
        Write-Host "  $k （$(($hit[$k]).Count) 个文件）" -ForegroundColor (Get-AuthorColor $k)
        $hit[$k] | Sort-Object | Select-Object -First 12 | ForEach-Object { Write-Host "      $_" -ForegroundColor DarkGray }
        if (($hit[$k]).Count -gt 12) { Write-Host "      …还有 $(($hit[$k]).Count - 12) 个" -ForegroundColor DarkGray }
    }
    if ($orphan.Count -gt 0) {
        Write-Host "  未命中任何范围（需人工确认）" -ForegroundColor Yellow
        $orphan | Sort-Object | Select-Object -First 12 | ForEach-Object { Write-Host "      $_" -ForegroundColor Yellow }
    }
    Write-Host ""
    exit 0
}

# ---------------------------------------------------------------- 单文件历史
if ($File) {
    if (-not (Test-Path $File)) { Write-Host "文件不存在：$File" -ForegroundColor Red; exit 2 }
    Write-Host ""
    Write-Host "=== $File 的提交历史 ===" -ForegroundColor Cyan
    Write-Host ""
    git --no-pager log --format='  %h  %ad  %an  %s' --date=format:'%m-%d %H:%M' -n $Limit -- $File
    Write-Host ""
    Write-Host "  按身份汇总：" -ForegroundColor DarkGray
    git --no-pager log --format='%an' -- $File | Group-Object | Sort-Object Count -Descending |
        ForEach-Object { Write-Host ("    {0,-14} {1} 次" -f $_.Name, $_.Count) -ForegroundColor (Get-AuthorColor $_.Name) }
    Write-Host ""
    exit 0
}

# ---------------------------------------------------------------- 某目录谁在改
if ($Path) {
    Write-Host ""
    Write-Host "=== $Path 的改动者 ===" -ForegroundColor Cyan
    Write-Host ""
    $authors = git --no-pager log --format='%an' -- $Path | Group-Object | Sort-Object Count -Descending
    if (-not $authors) { Write-Host "  没有提交记录（可能是新目录，或改动还没提交）" -ForegroundColor Yellow; exit 0 }
    foreach ($a in $authors) {
        Write-Host ("  {0,-14} {1} 次提交" -f $a.Name, $a.Count) -ForegroundColor (Get-AuthorColor $a.Name)
    }
    Write-Host ""
    Write-Host "  最近改动：" -ForegroundColor DarkGray
    git --no-pager log --format='    %h  %ad  %an  %s' --date=format:'%m-%d %H:%M' -n 10 -- $Path
    Write-Host ""
    $unc = @(git status --porcelain -- $Path)
    if ($unc.Count -gt 0) {
        Write-Host "  ⚠ 该路径还有 $($unc.Count) 个**未提交**的改动 —— git 无法归属，需人工确认" -ForegroundColor Yellow
        $unc | Select-Object -First 8 | ForEach-Object { Write-Host "      $_" -ForegroundColor Yellow }
    }
    Write-Host ""
    exit 0
}

# ---------------------------------------------------------------- 某身份改了什么
if ($Author) {
    Write-Host ""
    Write-Host "=== 身份 $Author 的改动 ===" -ForegroundColor Cyan
    Write-Host ""
    $files = @(git --no-pager log --pretty=format: --name-only "--author=$Author" | Where-Object { $_ } | Sort-Object -Unique)
    if ($files.Count -eq 0) { Write-Host "  没有它的提交（或用 -Author 传的是中文名，试试完整名）" -ForegroundColor Yellow; exit 0 }
    Write-Host "  提交数：$((git --no-pager log --pretty=format:'%h' "--author=$Author" | Measure-Object).Count)" -ForegroundColor DarkGray
    Write-Host "  涉及文件（$($files.Count)）：" -ForegroundColor DarkGray
    $files | ForEach-Object {
        $committed = git --no-pager log --pretty=format:'%an' -- $_ | Select-Object -Unique
        Write-Host ("    {0,-56} {1}" -f $_, ($committed -join ', ')) -ForegroundColor DarkGray
    }
    Write-Host ""
    exit 0
}

# ---------------------------------------------------------------- 默认：总览
Write-Host ""
Write-Host "=== 各身份的提交总览 ===" -ForegroundColor Cyan
Write-Host ""
$stat = git --no-pager log --pretty=format:'%an' | Group-Object | Sort-Object Count -Descending
if (-not $stat) { Write-Host "  还没有提交" -ForegroundColor Yellow; exit 0 }
foreach ($a in $stat) {
    $name = $a.Name
    # 注意：PowerShell 传原生参数时，`--author=$a.Name` 这种写法不可靠（属性访问可能不被展开）。
    # 必须先把名字取到变量、再用 "$(...)" 显式插值，并给整个参数加引号。
    $files = @(git --no-pager log --pretty=format: --name-only "--author=$name" |
        Where-Object { $_ } | Sort-Object -Unique)
    Write-Host ("  {0,-16} {1,3} 次提交   {2,4} 个文件" -f $name, $a.Count, $files.Count) -ForegroundColor (Get-AuthorColor $name)
}
Write-Host ""
Write-Host "  提示：早期提交全部署名为 'TokenAgency'（身份约定启用前）。" -ForegroundColor DarkGray
Write-Host "  从启用 scripts/commit.ps1 起，每次提交都带 Work-Package trailer。" -ForegroundColor DarkGray
Write-Host ""
Write-Host "  查某个目录谁在改：  pwsh -File scripts/who.ps1 -Path crates/ta-subscription" -ForegroundColor DarkGray
Write-Host "  查在途未提交的：    pwsh -File scripts/who.ps1 -Uncommitted" -ForegroundColor DarkGray
Write-Host ""
