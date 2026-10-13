; [docs/46 §302] **安装/卸载前：关掉运行中的 TokenAgency、给旧 exe 让路、确认目录可写**
;                        —— 全程 **详细日志** + **进度打印** + **绝不同步等待外部进程**。
;
; 这份钩子是被六个真实现场逼出来的（全部发生在真机）：
;
;   ① 旧实例在跑 ⇒ `tokenagency.exe` 被文件锁占着 ⇒ 复制失败 ⇒ 用户看到 NSIS 那句
;      `Error opening file for writing: …\TokenAgency\tokenagency.exe`；
;   ② 用户用火绒"粉碎文件夹"后重装仍报同一个错 —— 目录空着、我这边能写，真凶是**杀软拦安装程序写盘**；
;   ③ 第二版钩子在复制前 `Delete` 旧 exe ⇒ 复制失败就把用户的 app 删了（真发生；已改成
;      "改名让路 + 复制成功后才清理 + 用户取消则还原"）；
;   ④ `nsExec::Exec` / `ExecWait taskkill` **同步等待**外部进程 ⇒ 火绒 HIPS 一拦就永远停在
;      `Execute: "…taskkill.exe" …` 那一行 ⇒ 改用 `ExecShell`（立即返回，尽力而为）；
;   ⑤ 复制 58MB 内核 + 15MB 主程序 + 7MB GeoIP 时模板不打印任何东西 ⇒ 用户以为卡死 ⇒ 四阶段都打印；
;   ⑥ 用户问"是不是没提权？怎么回事？" ⇒ **把每个动作的结果 + Windows 错误码写进日志**：
;      `lasterr=5` = 拒绝访问（权限/杀软拦截；可试"以管理员身份运行"），`lasterr=32` = 文件被占用。
;
; 日志位置：`%TEMP%\ta-install-hook.log`（普通用户可写，不需要提权；每次安装重写一遍）。
;
; 注意：安装模式是 `currentUser`（装进 `%LOCALAPPDATA%`、注册表写 HKCU）—— **按设计就不提权**。
; 所以"写不进去"通常不是"忘了提权"，而是权限/杀软；但日志里的 `admin=` 与 `lasterr=` 能直接分清。
;
; 标签名各自加前缀：NSIS 宏是**文本展开**的，同名标签展开两次会编译失败。

; 写一行日志（不带错误码）
!macro TA_LOG TEXT
  FileOpen $R8 "$TEMP\ta-install-hook.log" a
  FileSeek $R8 0 END
  FileWrite $R8 "${TEXT}$\r$\n"
  FileClose $R8
!macroend

; 写一行日志 + Windows 最后一次错误码（必须紧跟在失败的那次调用之后用）
!macro TA_LOG_ERR TEXT
  System::Call 'kernel32::GetLastError() i.R1'
  FileOpen $R8 "$TEMP\ta-install-hook.log" a
  FileSeek $R8 0 END
  FileWrite $R8 "${TEXT} -> lasterr=$R1$\r$\n"
  FileClose $R8
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; ---------- 环境快照（每次安装重写；定位问题全靠这几行）
  System::Call 'shell32::IsUserAnAdmin() i.R3'
  FileOpen $R8 "$TEMP\ta-install-hook.log" w
  FileWrite $R8 "===== TokenAgency 安装器钩子 =====$\r$\n"
  FileWrite $R8 "phase   = preinstall$\r$\n"
  FileWrite $R8 "instDir = $INSTDIR$\r$\n"
  FileWrite $R8 "temp    = $TEMP$\r$\n"
  FileWrite $R8 "admin   = $R3   (1=以管理员身份运行, 0=普通用户)$\r$\n"
  IfFileExists "$INSTDIR\tokenagency.exe" 0 ta_pre_log_absent
  FileWrite $R8 "exe     = 已存在$\r$\n"
  Goto ta_pre_log_env_done
  ta_pre_log_absent:
  FileWrite $R8 "exe     = 不存在$\r$\n"
  ta_pre_log_env_done:
  FileClose $R8

  ; ---------- [1/4] 尽力关掉正在运行的实例（异步，绝不阻塞；火绒拦 taskkill 也不会卡住安装器）
  DetailPrint "TokenAgency: [1/4] 关闭正在运行的实例（尽力而为，不等待）…"
  ExecShell "open" "taskkill.exe" "/F /IM tokenagency.exe /T" SW_HIDE
  Sleep 1500
  !insertmacro TA_LOG "  step1 taskkill 已异步发起（不等待）"

  ; ---------- [2/4] 让路：旧 exe 改名成 .old（复制变成"新建"，杀软拦得少，也不怕句柄没释放）
  DetailPrint "TokenAgency: [2/4] 给旧文件让路（改名而不是删除）…"
  StrCpy $R5 0
  StrCpy $R6 "clean"
  IfFileExists "$INSTDIR\tokenagency.exe" 0 ta_pre_probe
  ta_pre_rename:
    Delete "$INSTDIR\tokenagency.exe.old"
    ClearErrors
    Rename "$INSTDIR\tokenagency.exe" "$INSTDIR\tokenagency.exe.old"
    IfErrors 0 ta_pre_moved
    !insertmacro TA_LOG_ERR "  step2 改名失败（第 $R5 次）"
    Sleep 2000
    IntOp $R5 $R5 + 1
    IntCmp $R5 8 ta_pre_rename_busy ta_pre_rename ta_pre_rename_busy
  ta_pre_moved:
    StrCpy $R6 "moved"
    !insertmacro TA_LOG "  step2 旧 exe 已改名为 tokenagency.exe.old（安装成功后清理）"
    DetailPrint "TokenAgency: 旧文件已改名让路（安装成功后自动清理）"
    Goto ta_pre_probe
  ta_pre_rename_busy:
    StrCpy $R6 "locked"
    !insertmacro TA_LOG "  step2 放弃：旧 exe 一直动不了（被占用或被拦）"
    DetailPrint "TokenAgency: 旧文件动不了（被占用 —— 可能还在运行，或被杀软扫描/拦截）"
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "安装程序动不了 tokenagency.exe —— 它正被占用。$\r$\n$\r$\n常见原因：TokenAgency 还在运行（托盘图标可能看不见），$\r$\n或者杀毒软件正在扫描/拦截它。$\r$\n$\r$\n先运行这个会更省事：$\r$\n    taskkill /F /IM tokenagency.exe /T$\r$\n然后点「重试」。$\r$\n$\r$\n详细诊断：%TEMP%\ta-install-hook.log" /SD IDCANCEL IDRETRY ta_pre_rename
    Goto ta_pre_abort

  ; ---------- [3/4] 可写探针：目录能不能真的写进去（写不进 ⇒ 点名杀软/权限，而不是让用户猜）
  ta_pre_probe:
    DetailPrint "TokenAgency: [3/4] 检查安装目录是否可写…"
    StrCpy $R7 0
    StrCpy $R9 "fail"
  ta_pre_probe_loop:
    ClearErrors
    FileOpen $R8 "$INSTDIR\ta-write-probe.tmp" w
    IfErrors 0 ta_pre_probe_ok
    !insertmacro TA_LOG_ERR "  step3 目录写探针失败（第 $R7 次）"
    IntOp $R7 $R7 + 1
    IntCmp $R7 3 ta_pre_probe_bad ta_pre_probe_loop ta_pre_probe_bad
  ta_pre_probe_ok:
    FileWrite $R8 "probe"
    FileClose $R8
    Delete "$INSTDIR\ta-write-probe.tmp"
    StrCpy $R9 "ok"
    !insertmacro TA_LOG "  step3 目录可写 ✓"
    DetailPrint "TokenAgency: 目录可写 ✓"
    Goto ta_pre_probe_done
  ta_pre_probe_bad:
    !insertmacro TA_LOG "  step3 放弃：目录写不进去（看上面的 lasterr：5=拒绝访问/被拦，32=被占用）"
    DetailPrint "TokenAgency: 目录写不进去 ✗（杀软拦截，或完整性/权限不够）"
    ; ---- 真机证据（2026-09-27）：日志落在 `%LOCALAPPDATA%\Temp\Low\` 里 —— 说明这台机器上的安装器
    ;      跑在**低完整性（Low IL）**里（火绒的「安装拦截」沙箱、或从受限上下文启动都会这样）。
    ;      低完整性进程**写不进** `%LOCALAPPDATA%\TokenAgency`（那里的强制标签是 Medium），
    ;      表现就是用户看到的那句 `Error opening file for writing`，而且连**改名让路**都会失败。
    ;      ⇒ 这时如果自己不是管理员，就用 UAC 重新拉起自己（runas ⇒ 高完整性），一遍就好。
    System::Call 'shell32::IsUserAnAdmin() i.R3'
    StrCmp $R3 "1" ta_pre_probe_noelevate
    !insertmacro TA_LOG "  step3 用 runas 重新以管理员身份启动安装器（低完整性写不进 %LOCALAPPDATA%）"
    DetailPrint "TokenAgency: 当前进程权限不足（低完整性），正在申请管理员权限重新启动…"
    MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "安装目录写不进去：$\r$\n    $INSTDIR$\r$\n$\r$\n这个进程权限不足（看起来被杀软沙箱/受限上下文降权了）。$\r$\n点「确定」会**申请管理员权限重新启动**，然后在弹出的窗口里继续安装（UAC 选「是」）。" /SD IDOK IDOK ta_pre_probe_elevate
    Goto ta_pre_abort
  ta_pre_probe_elevate:
    ExecShell "runas" "$EXEPATH" "$CMDLINE" SW_SHOWNORMAL
    Quit
  ta_pre_probe_noelevate:
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "安装目录写不进去：$\r$\n    $INSTDIR$\r$\n$\r$\n已经是管理员了，但还是写不进去 ⇒ 几乎可以确定是【杀毒软件】在拦：$\r$\n  火绒的「安装拦截 / 文件实时监控」最常见（加信任区有时还不够，要临时关掉实时监控）；$\r$\n  360 / Defender 同理。$\r$\n$\r$\n详细诊断（含 Windows 错误码 lasterr）：%TEMP%\ta-install-hook.log$\r$\n$\r$\n处理完点「重试」。" /SD IDCANCEL IDRETRY ta_pre_probe_loop
    Goto ta_pre_abort
  ta_pre_probe_done:

  !insertmacro TA_LOG "  step4 交给安装程序复制文件（内核 58MB + 主程序 15MB + GeoIP 7MB）"
  DetailPrint "TokenAgency: [4/4] 开始复制文件 —— 内核 58MB + 主程序 15MB + GeoIP 7MB，$\r$\n被杀软逐个扫描时可能要 1~3 分钟。进度条在走就没事，请耐心等（不要点「取消」）。"
  Goto ta_pre_end

  ; ---------- 用户点了「取消」：把让过路的 .old 还原回去（绝不能让他没有 app）
  ta_pre_abort:
    IfFileExists "$INSTDIR\tokenagency.exe.old" 0 ta_pre_abort_done
    IfFileExists "$INSTDIR\tokenagency.exe" 0 ta_pre_restore
    Goto ta_pre_abort_done
  ta_pre_restore:
    Rename "$INSTDIR\tokenagency.exe.old" "$INSTDIR\tokenagency.exe"
    !insertmacro TA_LOG "  取消安装：已把 .old 还原回 tokenagency.exe"
  ta_pre_abort_done:
    !insertmacro TA_LOG "  preinstall 中止（move=$R6 tries=$R5 probe=$R9）"
    Abort
  ta_pre_end:
!macroend

; 复制/安装成功之后才清掉让路的 .old（中途失败时它还在，用户的 app 可救）
!macro NSIS_HOOK_POSTINSTALL
  Delete "$INSTDIR\tokenagency.exe.old"
  Delete /REBOOTOK "$INSTDIR\tokenagency.exe.old"
  !insertmacro TA_LOG "postinstall: 复制已完成，.old 已清理 ✓"
  DetailPrint "TokenAgency: 完成（旧文件已清理）"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  System::Call 'shell32::IsUserAnAdmin() i.R3'
  FileOpen $R8 "$TEMP\ta-install-hook.log" a
  FileSeek $R8 0 END
  FileWrite $R8 "===== TokenAgency 卸载器钩子 =====$\r$\n"
  FileWrite $R8 "phase   = preuninstall$\r$\n"
  FileWrite $R8 "instDir = $INSTDIR$\r$\n"
  FileWrite $R8 "admin   = $R3$\r$\n"
  FileClose $R8
  DetailPrint "TokenAgency: 关闭正在运行的实例（尽力而为，不等待）…"
  ExecShell "open" "taskkill.exe" "/F /IM tokenagency.exe /T" SW_HIDE
  Sleep 1500
  !insertmacro TA_LOG "preuninstall: taskkill 已异步发起"
!macroend
