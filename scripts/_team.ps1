# TokenAgency 团队编制：身份 + 独占文件范围的**唯一真相源**。
#
# 被两个入口共同 dot-source（各自不能各抄一份）：
#   scripts/commit.ps1 —— **主管**直提 master（契约/文档/集成/合并）
#   scripts/pr.ps1     —— **员工**在自己分支上提 PR，主管评审后合并
#
# 为什么抽出来：范围表原来只活在 commit.ps1 里，docs/13 §1.2 又抄了一份表格，
# 两处不一致（前端路径挂在 E5 名下、E3 没有 src-tauri）→ E1 每提交一次前端都要
# 走 `-Override`。**同一事实有两个真相源，就会漂移。**
#
# 改这里 = 改制度。改完必须同步 docs/13 §1.2 的表格（那是可读版本，不是权威）。
#
# ⚠️ 本文件被 dot-source，**不得有任何副作用**：不写文件、不调 git、不 exit。

# ---------------------------------------------------------------- 身份
$Identities = @{
    'lead' = @{ Name = '技术主管'; Email = 'lead@tokenagency.local'; Desc = '契约/集成/打包/评审合并' }
    'E1'   = @{ Name = 'E1 前端与领域官'; Email = 'e1@tokenagency.local'; Desc = '前端 + 领域与配置' }
    'E2'   = @{ Name = 'E2 决策与可观测官'; Email = 'e2@tokenagency.local'; Desc = '决策/账本 + 可观测' }
    'E3'   = @{ Name = 'E3 数据与桌面官'; Email = 'e3@tokenagency.local'; Desc = '数据/密钥 + 桌面层' }
    'E4'   = @{ Name = 'E4 网络官'; Email = 'e4@tokenagency.local'; Desc = '网络接入（内核 + 订阅）' }
    'E5'   = @{ Name = 'E5 网关官'; Email = 'e5@tokenagency.local'; Desc = '网关（关键路径）' }
    'mech' = @{ Name = '机动岗'; Email = 'mech@tokenagency.local'; Desc = 'spike/补强/支援' }
}

# ---------------------------------------------------------------- 独占文件范围
$Scopes = @{
    'lead' = @('*')
    'E1'   = @('src', 'crates/ta-core/src/domain', 'crates/ta-core/src/config', 'config.example.yaml')
    'E2'   = @(
        'crates/ta-core/src/routing', 'crates/ta-core/src/sticky',
        'crates/ta-core/src/quota', 'crates/ta-core/src/pricing',
        'crates/ta-core/src/metering', 'crates/ta-core/src/diag',
        # 第 11 轮补的名册漏洞：`crates/ta-core/src/` 下的**散文件**（types.rs / error.rs /
        # trace.rs）此前不属于任何人。E2 拥有 ta-core 的契约层（diag/fields 与判定层），
        # 这三个文件是同一层的共享类型与错误定义，归它最自然。
        # 发现经过：E2 报"`MeteredUsage` 字段是 pub，别处仍可手拼冒充估算"，而要收紧构造
        # 就得改 types.rs —— 一查才发现它没有 Owner（docs/13 §1.3：没有 Owner 的归主管）。
        # 补进名册好过每次由我代笔：**无主文件是协作摩擦的源头**。
        'crates/ta-core/src/types.rs', 'crates/ta-core/src/error.rs',
        'crates/ta-core/src/trace.rs'
    )
    'E3'   = @('crates/ta-store', 'crates/ta-secrets', 'src-tauri')
    'E4'   = @('crates/ta-kernel', 'crates/ta-subscription')
    'E5'   = @('crates/ta-gateway')
    'mech' = @('*')
}

# 每个员工都隐式拥有的根文件。
#
# `Cargo.lock` 是 Cargo 生成的：改任何 crate 的 `Cargo.toml` 都会连带改它。
# 不给这条，员工的 PR 就会并入一个新依赖却留下不一致的锁文件 —— 那会在
# 合并后变成"别人的树突然变脏"，而且没人觉得是自己的责任。
# 根 `Cargo.toml` **不在此列**：它冻结，缺依赖要报给主管统一加（docs/13 §7）。
$ImplicitScopes = @('Cargo.lock')

# 集成分支。main 上只应该有**过审并合并**的东西。
$BaseBranch = 'master'

# ---------------------------------------------------------------- 工具函数

function Test-InScope {
    param([string]$Path, [string[]]$Scopes)
    if ($Scopes -contains '*') { return $true }
    $p = $Path -replace '\\', '/'
    foreach ($s in $Scopes) {
        if ($p -eq $s -or $p.StartsWith("$s/")) { return $true }
    }
    return $false
}

function Get-ScopeFor {
    param([string]$As)
    $list = @()
    if ($Scopes.ContainsKey($As)) { $list += $Scopes[$As] }
    if ($As -ne 'lead' -and $As -ne 'mech') { $list += $ImplicitScopes }
    return $list
}

# 员工的 PR 分支名。一个员工一个分支 —— 分支名就是身份，便于 `git log` 溯源。
function Get-PrBranch {
    param([string]$As)
    return "wp/$($As.ToLower())"
}
