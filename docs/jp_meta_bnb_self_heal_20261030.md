# jp-meta BNB 待确认自动恢复（2026-10-30）

## 原因与交易所核对

trade03 的 `transfer BNB MAIN_UMFUTURE` 从 2026-10-27 19:10:08.622 UTC
（上海时间 10 月 28 日 03:10）一直保留 pending。请求为 0.00000657 BNB，
交易所编号 `425435178631`。从当前交易配置重新确认固定查询 IP
`172.31.35.228`，只读查询交易所划转历史，得到相同编号、币种、金额及方向的
`CONFIRMED` 记录，时间为 `1793128208000`。没有重提这笔划转。

旧确认逻辑要求收款钱包余额高于划转前余额加到账量。持续手续费在观察前消耗了
这笔微量到账，余额门槛此后无法满足。检查时 trade03 总储备约 25.0613 BNB、
手续费钱包约 0.8616 BNB，均低于原有触发线；trade04 总量约 25.6315 BNB、
手续费钱包约 1.2339 BNB，pending 为空。两账户 USDT/BFUSD 自动理财开启、
未暂停，最新轮次因没有可申购的闲置 USDT 正常跳过。

## 自动恢复规则

保留到账余额确认的正常路径。等待超过 30 秒仍无法观察到账时，钱包划转和
BNB 活期赎回/申购查询交易所流水，以已持久化的 `tranId`、`redeemId` 或
`purchaseId` 精确匹配。还必须匹配十进制金额、币种、原提交时间窗口、划转方向，
或理财产品及来源/接收钱包；查询校验完整分页、总数和流水 ID 唯一性。

仅在匹配流水最终为 `CONFIRMED` / `PAID` / `SUCCESS` 时归档证据并自动解除
pending。最终 `FAILED` 归档后在下一轮重新评估。处理中、缺失、矛盾、分页不全或
查询失败均保留 pending；无交易所编号的未知请求仍需核对，不能按超时清除或
盲目重提。重启不改变这些判断。超过一分钟的 pending 按原配置周期查询，
最短 60 秒，避免每十秒查询历史。

划转历史 GET 使用独立的 1 IP weight，POST 保留原有保守的 900 UID weight；
理财历史查询使用 150 IP weight。全部请求使用原绑定客户端和共享限流器。
交易所依据：[划转历史](https://developers.binance.com/en/docs/catalog/core-trading-wallet/api/rest-api/asset#query-user-universal-transfer-history-user_data)、
[活期理财历史](https://developers.binance.com/en/docs/catalog/investment-and-services-simple-earn/api/rest-api/flexible-locked)。

## 发布与验证

发布为 `bnb-self-heal-20261030`，源码基于 checkout `c23cc62` 加本次补丁，
程序 SHA-256 为 `2b23dabb01b244990cf914d12ca17cd64b2341e94108d57178eb0904581ef751`。
仅替换并重启 jp-meta 的 `cta_web`，新 PID `2905678`，继续绑定 CPU 24。
主机 TOML、前端、BNB 与 USDT/BFUSD 设置及交易服务均保持原值。

`cargo fmt --check`、`cargo check`、`cargo test`、`cargo build --release` 通过；
需要独立 PostgreSQL 测试集群的集成测试按默认设置跳过。BNB 的 25 项测试覆盖
微量到账被手续费消耗、重启恢复、错误金额/币种/方向/产品/钱包/时间、非最终状态、
未知响应、查询失败、重复和不完整分页；另有 GET/POST 独立限流池回归。
新二进制上线前已通过两个账户的只读预览。

03:53:52 UTC，新程序自动查询并归档旧划转的 `CONFIRMED` 流水，解除旧 pending；
未调用人工 acknowledge 接口。自动流程随后消耗 678.56006445 BFUSD，购得
0.93933953 BNB，Convert 最终为 `SUCCESS`，再补足手续费钱包并申购余量
0.30031901 BNB。启动后的 USDT/BFUSD 常规轮次曾将现货 BFUSD 扫回合约，
BNB 控制器随后按实际余额重新准备所需 BFUSD，未重复兑换。

交易所固定 IP 只读复核及随后 Manager 自动轮次结果：

| 账户 | BNB 总量 | 手续费钱包 | 活期理财 | BNBUSDC 实际仓位 |
| --- | ---: | ---: | ---: | ---: |
| trade03 | 25.99961217 | 1.49961207 | 24.50000010 | -26.00 |
| trade04 | 25.63020329 | 1.23259040 | 24.39760961 | -26.00 |

余额继续随手续费和利息变化。两个账户 pending、refill_target、hedge_error 均为空；
trade03 后续自动轮次返回 `BNB reserve is within thresholds`。Exec Viz 就绪，
两个系统对冲均执行完成，无活动订单；trade03 目标尾差约 0.00000003 BNB。
共享 IP 预算仍偶尔让一轮检查延后，未产生未知资金操作，trade04 已观察到限流后
返回正常阈值状态。两个 USDT/BFUSD 控制器均保持开启、未暂停，Manager 健康为 `ok`。

85 个受保护进程、30 份配置、5 张不可变快照表、BNB 设置、USDT/BFUSD 设置及
前端指向在发布前后核对一致。短时诊断登录会话已全部在 finally 中删除。

04:02:42 UTC 追加只读核对：固定交易 IP `172.31.35.228` 的 USD-M 响应返回
`x-mbx-used-weight-1m=1143`，接近 Manager 的保守 1200/min 预算；同一时刻的
SAPI 划转历史返回 IP weight 1。补币完成、没有新资金划转时仍有本地预算不足
记录，因此不能全部归因为单笔划转太重。普通账户查询也会受共享交易出口的
USD-M 已用额度影响。现有错误日志未记录被拦截的具体接口或限流池，不能据此
精确归属每一轮失败；本次未提高预算或变更交易/理财出口。

本地证据及无密钥操作脚本：`artifacts/jp_meta_bnb_recovery_20261030/`。
主机备份：`/home/ubuntu/crypto_cta_manager/backups/bnb-self-heal-20261030/`；
原程序和资金日志用于审计，资金日志不能直接覆盖回运行状态或撤销已发生的操作。
