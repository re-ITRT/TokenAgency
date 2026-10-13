# 第三方组件声明（THIRD-PARTY-NOTICES）

本项目以 **GPL-3.0** 分发（见 [LICENSE](LICENSE)）。本文件列出**随包分发**或**编译进二进制**的
第三方组件、它们的许可、以及我们为履行这些许可所做的事。

> 这份文件**必须先于公开发布完成**。它不是形式主义：下面每一条都对应一个具体的义务
> （署名 / 提供源码 / 保留声明），漏掉一条就是把义务留给了下载的人去猜。
>
> 完整的依赖清单（925 条，机器生成）见 [THIRD-PARTY-DEPS.md](THIRD-PARTY-DEPS.md)；
> 重新生成：`node scripts/gen-third-party.mjs`。

---

## 1. mihomo —— 随安装包分发的内核（GPL-3.0）

| 项 | 值 |
| --- | --- |
| 组件 | mihomo（Clash 系代理内核） |
| 版本 | **v1.19.31** |
| 上游 | <https://github.com/MetaCubeX/mihomo> |
| 许可 | **GPL-3.0**（上游 LICENSE 全文） |
| 我们分发的形式 | `src-tauri/resources/mihomo/mihomo.exe`（打包进安装包） |
| sha256 | `1FA8055E03596FC35167F70E9ECD1890517D38D960A39177445746A1B0DEFC2B` |

**是否修改：没有。** 2026-10-13 实测：随包 exe 与上游
`mihomo-windows-amd64-compatible-v1.19.31.zip` 里的 exe **逐字节相同**
（同为 61105152 字节，sha256 一致）。因此附上游对应源码即已完整履行义务；
**若将来改动了内核，必须改为公开我们自己的源码**，不能再指向上游。

**与本项目的关系：聚合（aggregate），不是衍生。** 本程序通过命令行拉起 `mihomo.exe`，
再用本地 HTTP 控制口（`127.0.0.1:39090`）通信 —— 两个独立进程，不构成 GPLv3 §5
意义上的"更大程序"，因此 GPL 不传染到本仓代码。

**对应源码如何提供（GPLv3 §6(d)）**：与安装包**挂在同一个 GitHub Release**，作为附件
`mihomo-v1.19.31-source.tar.gz`（sha256 `5A04AA9CF4520E06FA1C13D37B6ACA49209479690722D485C40034AB17F8581F`）。
获取方式：`pwsh -File scripts/fetch-mihomo.ps1 -Mode Source`。

> ⚠️ **不要只贴上游链接来充当"提供源码"**。§6 的义务落在**分发者**身上，而那个 URL 不在我们
> 控制之下 —— 上游重打 tag、归档或删 release，我们的合规就断了。挂在自家 Release 上才算数。

---

## 2. Country.mmdb —— GeoIP 数据库（⚠️ 定制方待确认）

| 项 | 值 |
| --- | --- |
| 文件 | `src-tauri/resources/mihomo/Country.mmdb`（7,705,141 字节） |
| sha256 | `F35228DD74B70AE0AC72DCD2B86D03E0510264E20A0EC21EBB6AA32FA4F85C38` |
| 内嵌元数据（实测读出） | `database_type = GeoLite2-Country`；`description = Customized GeoLite2 Country database` |

**它不在上游 mihomo 的任何发布资产里**（已核实：v1.19.31 的 Windows 资产里只有 exe），
所以它是**本仓自己随包的第三方数据**，义务由我们承担。

它是 **MaxMind GeoLite2-Country 的第三方定制衍生库**。GeoIP 定位数据属于数据库，
受数据许可而非代码许可约束：

* **署名（必须）**：GeoLite2 数据由 MaxMind 创建。需要保留的署名文案：
  *This product includes GeoLite2 data created by MaxMind, available from
  <https://www.maxmind.com>.*
* **CC BY-SA 4.0（必须核对）**：MaxMind 的 GeoLite2 数据库以 **CC BY-SA 4.0** 分发，
  该许可带**相同方式共享**要求。以 MaxMind 官方页面为准。
* ⚠️ **定制方待确认**：内嵌描述里的 "Customized" 说明它不是原版 GeoLite2，而是某个
  社区项目的再加工产物（候选：`Loyalsoldier/geoip`、`MetaCubeX/meta-rules-dat`）。
  **定制方自己的许可会叠加在 MaxMind 之上**，而不同候选的许可不同
  （有的是 GPL-3.0，有的更宽松）。**在确认是哪一家之前，这一条不算完成。**

**待办**：确认该文件的下载来源 → 补齐定制方署名与其许可 → 更新本节。

---

## 3. Rust 依赖中**非宽松**的部分

全部 693 个 Rust 依赖里，绝大多数是 `MIT OR Apache-2.0`。**没有 GPL 不兼容的许可。**
以下几类需要单独声明：

### 3.1 MPL-2.0（文件级弱 copyleft，共 5 个）

| 组件 | 版本 |
| --- | --- |
| `cssparser` | 0.36.0 |
| `cssparser-macros` | 0.6.1 |
| `dtoa-short` | 0.3.5 |
| `option-ext` | 0.2.0 |
| `selectors` | 0.36.1 |

MPL-2.0 与 GPLv3 **兼容**（MPL-2.0 §3.3 明确允许把 MPL 文件并入 GPL 作品）。
**我们未修改这些文件**，因此不触发"改动部分必须保持 MPL"的义务；仅需保留声明。
若将来打补丁，被改的那**些文件**（不是整个项目）仍须以 MPL-2.0 提供。

### 3.2 双/多许可（我们选用其中宽松的一项，故无额外义务）

| 组件 | 声明 | 我们选 |
| --- | --- | --- |
| `r-efi` 5.3.0 / 6.0.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | MIT |
| `ryu` 1.0.23 | `Apache-2.0 OR BSL-1.0` | Apache-2.0 |
| `wasite` 0.1.0 / `whoami` 1.6.1 | `Apache-2.0 OR BSL-1.0 OR MIT` | MIT |

### 3.3 其它已核对为宽松的

`aws-lc-rs` / `aws-lc-sys`（ISC + Apache-2.0 + MIT + BSD-3-Clause）、
`webpki-roots`（CDLA-Permissive-2.0）、`unicode-ident`（`(MIT OR Apache-2.0) AND Unicode-3.0`）。

---

## 4. npm 依赖中需要署名的部分

| 组件 | 版本 | 许可 | 义务 |
| --- | --- | --- | --- |
| `caniuse-lite` | 1.0.30001810 | **CC-BY-4.0** | 仅需**署名**（非 copyleft） |

其余 231 个 npm 依赖均为 MIT / ISC / Apache-2.0 / BSD / 0BSD / MIT-0。

---

## 5. 其余全部依赖

除以上列出的，本项目的 Rust 与 npm 依赖均为宽松许可：
MIT、Apache-2.0、BSD-2/3-Clause、ISC、Zlib、Unlicense、0BSD、MIT-0、Unicode-3.0。
逐条清单见 [THIRD-PARTY-DEPS.md](THIRD-PARTY-DEPS.md)。

---

## 6. 本文件怎么维护

* 依赖增减后：`node scripts/gen-third-party.mjs` 重新生成清单，并**人工复查**
  第 3、4 节那类"非宽松"的条目有没有新增（生成器会把它们单独标出来）；
* 内核升级后：`scripts/fetch-mihomo.ps1` 里钉死的 tag/sha256 要更新，
  本文件第 1 节的版本、sha256、以及 Release 附件的源码 tarball **三者必须同步**；
* **判断标准**：`gen-third-party.ps1` 只做机械汇总，它**不能**替我们判断
  "这个许可要不要额外做什么"。凡是非 MIT/Apache/BSD/ISC 一类的，都必须人工过一遍。
