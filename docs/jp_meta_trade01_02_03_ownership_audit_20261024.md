# trade01 / trade02 / trade03 归属只读核查（2026-10-24）

后续已按操作者指定的 10 月 18 日完成新的交易所逐单核对：01 仍缺 16 笔、
02 仍缺 1 笔，03 之前的 24 笔已在库内。见
[同日缺口与时间窗口报告](jp_meta_oct18_missing_fill_windows_20261024.md)。
下文保留前次仅核对本地记录时的观察，不将其当作交易所完整性结论。

操作者追问 trade03、01 和 02 是否存在与 prc 相同的问题。本次对这三个
source 只读检查 Exec 历史、运行日志及 Nginx 下的 Manager/Viz，不写入归属，
不补写成交，不修改交易进程或配置。以下时间均为上海时间。

## 固定窗口观察

订单盈亏窗口固定为 **2026-10-24 00:00–19:40**，不含资金费。
“系统持仓名义”是持仓绝对市值之和，不是亏损。

| source / 账户 | Manager 系统费后订单盈亏 U | Manager 系统持仓名义 U | 同轮 Viz 系统持仓名义 U |
|---|---:|---:|---:|
| trade01 / prc_cta_01 | -582.47043695 | 34116.49536 | 3.30624 |
| trade02 / prc_cta_02 | -30.90044062 | 1271.52124 | 1.72901 |
| trade03 / trade03 | -797.51471861 | 28777.71885 | 13.37413 |

三者均有显著的历史归属偏差。历史累计订单报告与当前执行账本不是同一
时间锚点，不能把差额直接视为真实待平仓量或今天的误交易损失。
三者目前均没有 `cta_strategy_allocation_snapshots` 记录；不能声称 trade03
此前已经做了不可变归属校准。

trade02 的现存日志明确记录 10 月 18 日重启时恢复了 10 月 13 日更新的
Batch 账本。trade03 的此前归档取证也确认了 10 月 18 日恢复旧 Redis RDB，
参见 `artifacts/jp_meta_trade03_missing_trades_20261023/REPORT.md`。
trade01 当前日志仅保留到 10 月 21 日启动，暂不足以证明其 10 月 18 日
恢复点，不能仅凭类似页面表现断言完全相同的原因。

## 本地成交链核对

只读扫描截止 19:40 的全部正数量 `uniform_orders`，按账户、市场、合约、
client order ID 汇总；对 10 月 15 日起收到的 `trade_updates` 与
`trade_updates_unmatched` 正累计成交数量逐单比较。三者均未发现本地
原始回报存在而 uniform 累计数量不足的订单。

| source | 全历史正数量 uniform 事件 | 本次比较的正数量原始回报 | 原始回报到 uniform 的数量缺口 |
|---|---:|---:|---:|
| trade01 | 1275646 | 205124 | 0 |
| trade02 | 283959 | 29741 | 0 |
| trade03 | 790266 | 147475 | 0 |

三者今天真实交易所 `SYSTEM_POSITION_CLOSE` 成交均为 **0**；系统事件分别
1504 / 559 / 1178 条，均为内部交叉。10 月 18 日确有历史真实系统成交：
trade01 354 条、名义 36942.7779668 U；trade02 23 条、1249.5294636 U；
trade03 144 条、30072.799755 U（USDT/USDC 名义相加）。这些名义额不是损失，
也不能为了清理页面系统项而改成 CTA 策略成交。

这不是新的交易所 REST 全量对账；交易所成交若同时缺失于本地两种记录，
此检查无法发现。trade03 此前发现过的 24 笔缺失成交属于旧取证结果，不能
在没有当前交易所复核的情况下声称这些成交现在仍缺失。

## 证据

`artifacts/jp_meta_prc_history_ownership_20261024/` 中：
`accounts_before.jsonl` / `accounts_fixed_pre.jsonl` 保存接口摘要；
`trade0{1,2,3}_allocation_logs.jsonl.gz` 保存选定的安全日志；
`binance_exec_trade0{1,2,3}_events.jsonl.gz` 保存流式扫描结果；
`source_scanner_manifest.json` 保存扫描器 SHA256 及全部成功退出状态；
`check_source_events.rs`、`check_accounts.py` 可复核扫描规则。
扫描器通过匿名 memfd 低优先级运行，没有安装到 Exec，也没有写入 Exec 存储。
诊断认证会话均在 finally 删除并复核。
