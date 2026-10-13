# 真 mihomo 夹具（**不是手写的**）

这些文件是 **mihomo 真二进制的原始输出**，由主管在 M0 spike 里对着真内核采集
，原样拷入本目录，**一个字节都没改**。

## 为什么必须有这个目录

> **夹具应当是"断言的输入"，不是"断言的产物"。**

从断言反推写出来的夹具，只能证明"我们的代码与我们的断言自洽"——
它无法发现"我们以为的内核形态"本身就是错的。

`ta-kernel` 之前的 `parse_snapshot` 测试用的就是手写 JSON（"我以为的内核"），
结果 K7 实测出 **4 处偏差**会让它解析错误（`providers` 是对象不是数组、
`type` 是大写内部名、provider 节点不在 `/proxies` 顶层、内置伪节点混在列表里）。
**本目录就是为了让那种偏差无法再发生。** 夹具从这里读，不能改成手写。

## 采集环境

| 项 | 值 |
|---|---|
| 内核 | `Mihomo Meta v1.19.31 windows amd64 with go1.26.8` |
| REST | `127.0.0.1:39090` |
| 入站 | 只开显式 `listeners`（`mixed-port: 0`），**未启用 tun / 未改系统代理** |
| 日期 | 2026-09-21 ~ 2026-09-22 |
| 采集者 | 主管（M0 spike，`C:\Users\19404\workspace\.ta-tools\m0\`） |

来源目录与**用途**（15 个文件 = 7 + 6 + 2）：

| 来源目录 | 文件 | 用途 |
|---|---|---|
| `m0/fixtures/`（**7 个**） | `proxies.json`、`providers-proxies.json`、`k4.log`、`k8.log`、`version.json`、`configs.json`、`config-k4.yaml` | 前 4 个是**断言输入**（K7 往返测试 + childlog parser 单测直接 `include_str!` 它们）；后 3 个是**证据**（不在任何测试里读，用于人工核对/文档引用） |
| spike 根目录（**6 个**） | `emptya.log`、`emptyb.log`、`config-emptya.yaml`、`config-emptyb.yaml`、`k3c.log`、`config-k3c.yaml` | `emptya.log`/`emptyb.log` 是**断言输入**（childlog parser 单测）；其余 4 个是**证据**（B1/B2 的原始记录，未进测试） |
| `m0/providers/`（**2 个**） | `lane.yaml`、`EMPTY.yaml` | **证据**：provider 文件本体的形态（`lane.yaml` = 真内核接受过的 3 节点；`EMPTY.yaml` = `proxies: []`，即 B2 里"存在但为空"那一格的输入） |

> 「断言输入」= 测试会读它并**对着它**断言（：夹具是断言的输入）；
> 「证据」= 只作为事实记录与人工核对依据。**不要把证据文件删掉** —— 它们是这些结论的原始出处。

## 文件清单：每个文件证哪一条

| 文件 | 是什么 | 证什么 |
|---|---|---|
| `version.json` | `GET /version` 的原始响应 | 就绪探测的响应形态：`{"meta":true,"version":"v1.19.31"}`（[06 §1.6] 附带验证） |
| `configs.json` | `GET /configs` 的原始响应 | spike 运行配置的生效值（**注意：这是 spike 的配置，不是我们生成的** —— 它 `mixed-port: 39100`，而 K6 裁决我们**永远写 `mixed-port: 0`**） |
| `proxies.json` | `GET /proxies` 的原始响应 | **K7 的 7-b / 7-d**：`type` 是首字母大写内部名（`Selector`/`Shadowsocks`/`Compatible`…）；内置伪节点 `COMPATIBLE/DIRECT/GLOBAL/PASS/PASS-RULE/REJECT/REJECT-DROP` 混在里面；每个节点带 `alive` 与 `id` |
| `providers-proxies.json` | `GET /providers/proxies` 的原始响应 | **K7 的 7-a / 7-c**：`providers` 是**对象**（按 key 索引）不是数组；`ta-lane-provider` 的 `prov-node-a/b/c-vmess` **不在 `/proxies` 顶层**，却出现在 `TA-LANE-mixed.all` 里（悬空引用）；另 3 个 `vehicleType: Compatible` 的条目是**组的镜射**，不是用户订阅 |
| `lane.yaml` | K4/k3c 用的 provider 文件本体 | `ta-lane-provider` 的 3 个节点从哪来（`prov-node-a/b/c-vmess`） |
| `EMPTY.yaml` | 内容就是 `proxies: []` | **K2 细化里"provider 文件存在但为空"那一格**的输入 |
| `config-emptya.yaml` | provider **文件缺失**的配置 | B2 的输入：`missing-provider` 指向 `DOES-NOT-EXIST.yaml` |
| `config-emptyb.yaml` | provider **文件为空**的配置 | B2 的输入 |
| `emptya.log` | 上面那份配置的内核日志 | ★ **B2 的决定性证据**：`emptya.log:8` 是 `level=error initial proxy provider missing-provider error: open … The system cannot find the file specified.`，而 **`emptya.log:9` 紧接着是 `level=info RESTful API listening at: 127.0.0.1:39090`** —— **内核带着错误照常启动、REST 照常通**，所以"进程活着 + REST 有响应"式的就绪判定会对外报绿 |
| `emptyb.log` | 同上（空 provider） | ★ 同上，错误原文是 `file doesn't have any proxy`（`emptyb.log:8` → `:9` REST listening） |
| `config-k3c.yaml` | `filter: "(?i)^zzz-never-matches"` + `empty-fallback: REJECT` | B1 的输入 |
| `k3c.log` | 上面那份配置的内核日志 | ★ **B1 的决定性证据**：`k3c.log:10` 是 `[TCP] 127.0.0.1:8691 --> cp.cloudflare.com:80 using G-nomatch-reject[REJECT]` —— filter 匹配不到任何节点时**没有静默走 `COMPATIBLE`（直连）**，而是明确 REJECT |
| `config-k4.yaml` | 4 个 listener，第 4 个 `proxy: "这个节点不存在"` | B3 的输入（负例：指向不存在的名字） |
| `k4.log` | 上面那份配置的内核日志 | ★ **B3 的决定性证据**：`k4.log:10` `Mixed(http+socks)[ta-lane-bogus] proxy listening at: …39104` —— 指向不存在的名字**照样绑定成功、启动零报错**；直到 `k4.log:35`（41 秒后、请求到达时）才 `level=warning [Metadata] parse failed: proxy 这个节点不存在 not found` |
| `k8.log` | 强杀重启后的日志 | **K8**：选中项在强杀后仍保留；另可见 `Start initial compatible provider …` —— 解释了 `/providers/proxies` 里那些 Compatible 条目的来源 |

## 解析这些日志时必须知道的两件事

1. 格式是 **logrus text**：`time="…" level=info msg="…"`。级别取值实测有
   `info` / `warning`（**不是 `warn`**）/ `error`。
2. **引号内的值会被转义**：`emptya.log:8` 原文里是 `C:\\Users\\19404\\…`（双反斜杠）。
   解析器必须做反转义，否则 Windows 路径会解析错。

## 校验

文件哈希（sha256）：

```
config-emptya.yaml       73E314359033F89C3863F87697C7EE78C65B74591E95AFFD7B1B2AF0D387D002
config-emptyb.yaml       46288FC18E88E678B434F48D7FEDF1DA0C9693818EF57D2AC9BB8CD4DD79C7AA
config-k3c.yaml          6D16D543E17CCF6CFD91D328428DF80FB96B5754AE1B3FB02CC45A4E42D78A6C
config-k4.yaml           3B03264D0121FCF178FD7DB4B3358F4D1F575942139397BA19DA25487467DDD6
configs.json             8D019A79373AAEF2A43B86B2E71CE54AE2F8BE0331ABCFA63ACF14B424731288
EMPTY.yaml               8875894E84075C7EAB82661951551932CDFE1DE11171EEB0B22A0EC83F65D25E
emptya.log               6A6B8C4B4697A98196783E767A5B3A3D7D0E6592D1E809C5D759148EC5B55D8A
emptyb.log               E9BBA7ED38245F940DB940FCA9892155828C45590BCEE84D365695CBF0C87A15
k3c.log                  C8D29A29734822015FE61D544E491C5D2F5352C330B87B5B998A5287664EEF67
k4.log                   C582DB6A673A2A3DF5C3896AAE76C4E08B19147F5E8D1EE6FBC16D39CCD5236F
k8.log                   AB31DACCD40D3C895FEF8507B7BDCB269FBD1C8E73EC091B4B8AA58950B7B004
lane.yaml                82B60DB8280CA27FD14835EF06F4ED20472D36A4F7FB9A6C01BA8F404D2D328D
providers-proxies.json   CD9C47583CE82DAAB9CDD0B2932EE6D4D5231225E31FA03C5DEE4056E234C2D4
proxies.json             D3E7ABFC3946444E49248EE4713F2A9BB9F58D31AEEC8F600B49C28CC763A422
version.json             695DD1E0B2CD97D7D5C34CC6C7683FE420F53310714563D29072EAA52BE3805C
```

生成日期里的时间戳/端口是当时 spike 的取值，**不要**把它们写进断言以外的逻辑。
