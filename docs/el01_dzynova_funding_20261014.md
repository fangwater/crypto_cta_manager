# el01 dzynova funding 与 Manager 开关（2026-10-14）

仅 `dzynova` / `binance_exec_trade09` 开启资金费记录。主机 `[funding]` 固定绑定本地及预期公网地址 `154.197.32.9`，禁止代理、默认路由及交易 IP 回退；13 个启用账户的交易 TOML 均已检查，交易公网出口 `154.197.32.6` 显式排除。Kline 继续使用 `154.197.32.10`。

账户和各 symbol 从本机最早订单创建时间拉取历史，不以功能开启时间或净值快照为起点。dzynova 的账户起点为 `1790240116645637`，即 **2026-09-24 08:55:16.645637 UTC**（北京时间 16:55:16）。发现更早历史时只向前扩展起点，重开受影响的 cursor；原有账单保留，以账户、venue 和 transaction ID 去重。

Manager 的账户配置页及策略启用页都有「Funding 记录」面板。开关和拉取间隔保存在 PostgreSQL，默认关闭，保存后即时生效；当前 dzynova 为 300 秒，可配置 60–86400 秒。账户卡片和净值页显示开启、关闭、补齐、正常或异常标志。每次开启先校验主机私有环境中的启用 token，然后实测固定公网出口及该账户的 Binance 签名 funding income API，全部通过才保存开关和测试记录。暂停保留历史账单，并继续计入同一窗口总体盈亏。明文 token 不保存在仓库、数据库、日志或浏览器存储。

既有数据库已显式执行两个维护脚本：`apply_funding_schema.sql` 新增账单与游标表，`apply_funding_settings_schema.sql` 新增账户开关、频率和测试结果字段。正常启动不执行 DDL。

运行时发布为 `20261014T220053Z`，`cta_web` SHA-256 为 `3e3a015b716736f8416d2ff0ceed756531d0e148525551bf405877c084676b73`；开关版前端发布为 `20261014T220725Z`，Funding 曲线展示版为 `20261014T223225Z`。两次前端切换均没有重启 Manager。构建使用既有工作区，保留此前并发修改。

## 验证

- dzynova 的 `.6`、`.9`、`.10` 绑定分别通过公网出口及签名 funding income 连通测试（初次部署证据）。实际采集使用 `.9`；Manager 中其他 HTTPS 客户端仍可能使用 `.10` Kline 和 `.6` 账户查询。
- 线上匿名开启被拒绝为 401，缺失或错误 token 被拒绝为 403；59 秒和 86401 秒被拒绝为 400，账户开关仍关闭。正确 token 通过固定 `.9` 公网出口及 Binance 认证测试后开启，并记录两个已验证 IP。
- 无重启将拉取间隔改为 600 秒，随后恢复 300 秒。暂停后原有 12 笔账单保留且仍计入账户净值；重新开启再次要求 token 和网络测试。
- 修正旧的启用时间起点时保留原有 12 笔账单（合计 `2.17707360`），按真实订单起点补齐 15 个币种。请求受滚动每分钟 120 weight / 4 次 income 请求约束。历史补齐完成后共 **662 笔**，资金费合计 **`-385.39771612 USDT`**，状态 `ok`，其他账户仍关闭。
- 净值查询使用账户快照允许的完整区间（资金费观测起点独立、比快照更早），662 笔全部包含在区间内；费前/费后总体盈亏分别严格等于订单 NAV 加区间 funding，API 返回 154 个 funding/总体曲线点。
- 再从固定 `.9` 独立查询 BTCUSDT 首周的 21 笔原始账单，transaction ID、十进制金额和入账时间与数据库逐项一致，首周合计 `-44.32452860 USDT`。
- 82 个受保护进程（含 PostgreSQL 主进程）及 13 份交易 TOML 哈希未变，Exec Viz/Config 和前端资产返回 200。监控仅原子替换二进制以兼容新 TOML，运行中的监控没有重启。临时验证会话及私有回退备份已清理。
- Rust 全量 311 个库测试和 9 个二进制测试通过；7 个 PostgreSQL schema 测试及 funding 的 PostgreSQL/绑定 IP/签名 REST/429 重试集成测试通过。前端生产构建及 lint 通过（lint 仅保留既有的两个无关警告）。

启用验证证据位于 [verification.jsonl](../artifacts/funding_settings_el01_20261014/verification.jsonl)，最终财务及运行验证见 [final_verification.jsonl](../artifacts/funding_settings_el01_20261014/final_verification.jsonl)，独立账单核对见 [exchange_verification.jsonl](../artifacts/funding_settings_el01_20261014/exchange_verification.jsonl)；前端发布证据位于 [frontend_deployment.json](../artifacts/funding_settings_el01_20261014/frontend_deployment.json)。初次部署的三个 IP 连通测试见 [原始部署证据](../artifacts/funding_el01_20261014T150804Z/deployment.json)。通用配置与财务口径见 [funding_and_slippage.md](funding_and_slippage.md)。

## Funding 收益曲线直接展示

已开启或保存过 Funding 的账户默认进入「账户综合 PnL」，收益曲线默认勾选橙色 `Funding（累计资金费）` 和费后总体净值。Funding 使用阶梯线，顶部增加区间 Funding 金额；仍可手动取消勾选或查看策略归因。未开启且没有历史记录的账户保持策略归因默认值。暂停后历史 Funding 继续可见，区间与币种筛选沿用账户 NAV 的范围。

2026-10-14 22:32 UTC 通过 Nginx 核对 dzynova：1D 的 38 笔合计 `0.28435201`，完整区间 662 笔合计 `-385.39771612`，1D BTCUSDT 的 3 笔合计 `1.75893418`。逐个核对三个范围内所有 Funding 曲线点与 SQL 账单累加值、起点零值、终点总额及含 Funding 的费前/费后净值加法关系。前端构建通过；页面和新 JS/CSS 返回 200，已包含新增 Funding 曲线与金额标签。本次不启动浏览器开发服务，验证方式为生产构建、线上资源校验和账单核对。

只原子切换前端 symlink，保留旧 release 供回滚。Manager、Nginx、PostgreSQL 主进程和 82 个受保护进程以及 13 份交易配置均保持不变；临时认证会话已删除。发布及核对证据见 [deployment.json](../artifacts/funding_curve_el01_20261014/deployment.json) 和 [verification.jsonl](../artifacts/funding_curve_el01_20261014/verification.jsonl)。

按操作员要求，前端 `20261014T223814Z` 移除「资金费明细（最近 100 笔）」和「按币种查看观测进度」两个展开项，保留资金费曲线、汇总金额、总体盈亏和状态。生产构建及线上新资源核验通过，82 个受保护进程与 13 份交易配置未变。见 [简化界面发布](../artifacts/funding_compact_el01_20261014/deployment.json) 与 [验证记录](../artifacts/funding_compact_el01_20261014/verification.json)。
