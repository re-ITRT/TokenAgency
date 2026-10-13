# 拉取随包内核 mihomo 的**对象码**与**对应源码**。
#
# 为什么要有这个脚本（2026-10-13，许可决策当天写的）：
#
# 1. **GPL 合规**。本软件随安装包分发 mihomo（GPLv3），属于 GPLv3 §5 末段的 aggregate。
#    义务落在**我们**身上：要么随对象码给出对应源码，要么给三年有效的书面要约，
#    要么（§6(d)）在**提供对象码的同一个地方**同样免费地提供对应源码的等价访问。
#    这个脚本把"对应源码"这件事变成一条命令：`-Mode Source` 拉到的 tarball
#    就是发布 GitHub Release 时**必须一起挂上去**的那个附件。
#
# 2. **"改没改过内核"必须可查**。GPL 下"改过"与"没改过"是两种完全不同的义务：
#    没改 → 指向上游源码就够了；改过 → **必须公开我们自己的改动**。
#    二进制一旦入库，这件事就看不出来了。所以：**二进制不入库**（见 .gitignore），
#    由本脚本按**钉死的 tag + sha256** 拉取，校验不过就拒绝。
#    已核实（2026-10-13）：随包的 `mihomo.exe` 与上游
#    `mihomo-windows-amd64-compatible-v1.19.31` **逐字节相同** —— 即未修改上游。
#
# 3. **可复现**。干净 clone 拿不到 65MB 二进制，跑一次本脚本即可。
#
# ⚠️ `Country.mmdb` **不由本脚本拉取**，因为它**不在上游任何 mihomo 发布资产里**
#    （2026-10-13 核实：v1.19.31 的 windows 资产里只有 exe）。
#    它的来源与许可**还没查清**，必须先查清再决定怎么随包，并写进 THIRD-PARTY-NOTICES.md。
#
# 用法：
#   pwsh -File scripts/fetch-mihomo.ps1                 # 对象码（构建用）
#   pwsh -File scripts/fetch-mihomo.ps1 -Mode Source    # 对应源码（发布用，挂 Release）
#   pwsh -File scripts/fetch-mihomo.ps1 -Mode All
#   pwsh -File scripts/fetch-mihomo.ps1 -Force          # 已存在也重下

[CmdletBinding()]
param(
    [ValidateSet("Runtime", "Source", "All")]
    [string]$Mode = "Runtime",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$RuntimeDir = Join-Path $RepoRoot "src-tauri\resources\mihomo"
$AssetDir = Join-Path $RepoRoot ".ta-tmp\release-assets"
$WorkDir = Join-Path $AssetDir "_work"

# ---------------------------------------------------------------- 钉死的版本与校验和
# 这些值**不是猜的**，是 2026-10-13 从上游实测比对出来的：
# 随包 exe 的 sha256 == 上游该资产的 exe sha256（逐字节相同）。
$MihomoVersion = "v1.19.31"
$RuntimeAssetName = "mihomo-windows-amd64-compatible-$MihomoVersion.zip"
$RuntimeAssetSha256 = "93D14E9A13B49B2F2D256202D02CC8D14A7C4695EDF084CAE0F941986BC9C218"
$RuntimeExeSha256 = "1FA8055E03596FC35167F70E9ECD1890517D38D960A39177445746A1B0DEFC2B"
$RuntimeExeName = "mihomo.exe"

$SourceAssetName = "mihomo-$MihomoVersion-source.tar.gz"
$SourceAssetSha256 = "5A04AA9CF4520E06FA1C13D37B6ACA49209479690722D485C40034AB17F8581F"
$SourceBaseUrl = "https://github.com/MetaCubeX/mihomo/archive/refs/tags/$MihomoVersion.tar.gz"
$ReleaseBaseUrl = "https://github.com/MetaCubeX/mihomo/releases/download/$MihomoVersion"

function Get-VerifiedFile {
    <#
      下载并**校验 sha256**。校验不过就抛错 —— 不写半成品。
      这个仓库把"静默失效"视为最严重缺陷，所以这里宁可炸掉：
      一个校验和不对的内核二进制随包发出去，是把 GPL 与"我们发了什么"两件事同时搞坏。
    #>
    param([string]$Url, [string]$OutFile, [string]$ExpectSha256)

    if ((Test-Path $OutFile) -and -not $Force) {
        $have = (Get-FileHash $OutFile -Algorithm SHA256).Hash
        if ($have -eq $ExpectSha256) {
            Write-Host "  已存在且校验通过：$(Split-Path -Leaf $OutFile)"
            return $OutFile
        }
        Write-Host "  已存在但校验不符，重新下载：$(Split-Path -Leaf $OutFile)"
        Remove-Item $OutFile -Force
    }

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $OutFile) | Out-Null
    Write-Host "  下载 $Url"
    Invoke-WebRequest -Uri $Url -OutFile $OutFile -MaximumRedirection 5 -TimeoutSec 600

    $got = (Get-FileHash $OutFile -Algorithm SHA256).Hash
    if ($got -ne $ExpectSha256) {
        Remove-Item $OutFile -Force -ErrorAction SilentlyContinue
        throw "sha256 不符，已删除下载物。`n  期望 $ExpectSha256`n  实际 $got`n  URL  $Url"
    }
    Write-Host "  sha256 校验通过：$($got.Substring(0,16))…"
    return $OutFile
}

function Get-Runtime {
    Write-Host "== 拉取内核对象码 $MihomoVersion =="
    New-Item -ItemType Directory -Force -Path $WorkDir, $RuntimeDir | Out-Null

    $zip = Join-Path $WorkDir $RuntimeAssetName
    Get-VerifiedFile -Url "$ReleaseBaseUrl/$RuntimeAssetName" -OutFile $zip -ExpectSha256 $RuntimeAssetSha256 | Out-Null

    $x = Join-Path $WorkDir "extract"
    Remove-Item $x -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive -Path $zip -DestinationPath $x -Force

    $exe = Get-ChildItem $x -Recurse -Filter *.exe | Select-Object -First 1
    if (-not $exe) { throw "压缩包里没有 exe（资产名或结构变了，去上游发布页确认）" }

    $got = (Get-FileHash $exe.FullName -Algorithm SHA256).Hash
    if ($got -ne $RuntimeExeSha256) {
        throw "解出来的 exe sha256 与钉死值不符 —— 上游可能重打了该 tag 的资产。`n  期望 $RuntimeExeSha256`n  实际 $got"
    }

    Copy-Item $exe.FullName (Join-Path $RuntimeDir $RuntimeExeName) -Force
    Write-Host "  ✅ 已就位 $RuntimeDir\$RuntimeExeName（$($exe.Length) 字节）"

    if (-not (Test-Path (Join-Path $RuntimeDir "Country.mmdb"))) {
        Write-Warning "  ⚠️ 缺 Country.mmdb：它**不在上游 mihomo 资产里**，来源与许可尚未查清（见本脚本头部注释）。内核需要它才能做地域分流。"
    }
}

function Get-Source {
    Write-Host "== 拉取对应源码 $MihomoVersion（发布时必须挂到同一个 GitHub Release）=="
    $out = Join-Path $AssetDir $SourceAssetName
    Get-VerifiedFile -Url $SourceBaseUrl -OutFile $out -ExpectSha256 $SourceAssetSha256 | Out-Null
    Write-Host "  ✅ $out"
    Write-Host "     发布时把它作为附件挂到本项目的 Release —— 这就是 GPLv3 §6(d) 的"
    Write-Host "     '在同一个地方提供对应源码的等价访问'。别只贴上游链接："
    Write-Host "     那个 URL 不在我们控制之下，上游重打 tag/归档就断了。"
}

switch ($Mode) {
    "Runtime" { Get-Runtime }
    "Source"  { Get-Source }
    "All"     { Get-Runtime; Get-Source }
}
