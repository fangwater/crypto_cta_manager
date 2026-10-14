# jp-meta trade03 VIP2 BNB 储备调整

2026-10-13 UTC，按操作员要求调整 jp-meta 的
`binance_exec_trade03`，通过运行中的 Manager API 保存账户配置并完成首次补仓。

| 配置 | 原值 | 生效值 |
| --- | ---: | ---: |
| `required_bnb` | 5 | 25 |
| `refill_trigger_bnb` | 5.2 | 25.2 |
| `refill_target_bnb` | 6 | 26 |

管理总量为现货 + USD-M 钱包 + BNB 活期理财，不包括永续合约数量。
总量小于等于 25.2 BNB 时，沿用 BFUSD → BNB 直接 Convert 补到 26 BNB。
自动管理保持开启，`dry_run=false`。BNBUSDC 永续仍由
`SYSTEM_BNB_RESERVE` 独占对冲。

首次补仓前，总量约 5.1613 BNB。原先有一笔 2026-10-09 22:47 UTC 的
`redeem BNB fee reserve` 待确认，金额为 0.00001505 BNB、响应为空。
交易所两小时完整赎回历史只有此前已支付的 0.30001074 BNB，未找到该小额请求。
核对历史和余额后，通过带原始待处理时间戳的 Manager acknowledge 接口解除阻塞。

首次购买需要超过原 10000 USDT 等值兑换上限，执行期间临时提高到 20000。
临时将对冲调整阈值设为 1 BNB，避免购买前的一次小额修正占用正常的一小时
对冲发布间隔；购买后仍发布完整的 -26 BNB USDC 对冲目标。
完成后，兑换上限恢复为 10000、调整阈值恢复为 0.5 BNB。
正常对冲最短间隔仍为 3600 秒，余额检查间隔仍为 60 秒。

Convert 确认成功：消耗 15557.3124121 BFUSD，购得 20.83870911 BNB。
手续费钱包补足至 1.5 BNB，余量申购 BNB 活期理财，并关闭该产品的
`autoSubscribe`，避免交易所自动归集与 Manager 备用金流程竞争。

2026-10-13 13:54:23 UTC，以重新核对过的交易绑定
`172.31.35.228` 固定查询交易所账户和活期理财，结果如下：

| 项目 | 数量（BNB） |
| --- | ---: |
| 现货 | 0 |
| USD-M 手续费钱包 | 1.49995163 |
| 活期理财 | 24.50000006 |
| 储备合计 | 25.99995169 |
| BNBUSDC 实际空头 | -25.99 |
| 储备与对冲净敞口 | 0.00995169 |

Exec Viz 确认对冲执行完成、无活动订单，未成交尾量约 0.01 BNB，处于原有
0.02 BNB 执行容差内。Manager 状态为 `BNB reserve is within thresholds`，
无待处理资金操作、无对冲错误，健康接口为 `ok`。
交易所此时仍报告 `feeTier=1`；本次调整不会代替交易所的 VIP 等级认定。

本次未替换二进制、未重启服务。15 项 Manager/交易配置及二进制哈希不变，
USDT/BFUSD 配置文件不变。所记录的非 Nginx 进程运行标识不变；观测期间
6 个旧 Nginx worker 退出，Nginx master 运行标识不变，本次未调用 Nginx
reload/restart。临时管理员会话均已删除。

远端恢复备份和验证记录保留于：

`/home/ubuntu/crypto_cta_manager/backups/trade03-vip2-20261013T135036Z`

其中 `bnb-auto.before.json` 是原配置与资金日志，`verification.json` 和
`exchange-verification.json` 是最终验证。恢复旧文件不能撤销已成交的资金兑换
或对冲，也不能直接覆盖运行中 Manager 的当前资金日志；后续调整仍通过 API。
