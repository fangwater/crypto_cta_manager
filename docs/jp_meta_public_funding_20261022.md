# jp-meta 公共 Funding HTTP 403 修复 — 2026-10-22

## 现象与定位

`public funding REST returned HTTP 403` 来自公共 Binance USD-M
`/fapi/v1/fundingRate`，影响策略资金费估算。账户实际账单使用独立的 `[funding]`
配置；修复前三个 RapidX 账户 trade05/06/07 的实际采集均为 `ok`。

08:22 UTC 在 jp-meta 对比同一 BTCUSDT 请求：默认本地 `172.31.35.228` /
公网 `13.115.227.29` 返回 403，已有数据本地 `172.31.46.93` /
公网 `18.181.48.65` 返回 200。后者对六个默认 USDT 合约、BTCUSDC 和
`龙虾USDT` 的 30 天历史请求均返回完整有效数据。

只绑定数据出口仍不足以解决批量预热：08:31:08 UTC 再次出现 403。原缓存虽然
限制每五分钟 450 次，但能集中发出全部额度，而且只对 418/429 设置共享冷却，
403 会继续按合约重试。[Binance 文档](https://developers.binance.com/en/docs/products/derivatives-trading-usds-futures/general-info)
将 403 定义为 WAF 限制；本次 HTML 响应没有披露具体触发规则。

独立出口使用每秒一次仍于 08:59:15 UTC 返回 403（开始预热约 136 秒后），
说明分离出口和一秒节流不足以稳定恢复。因此最终降低为每三秒一次、滚动五分钟
最多 90 次。日志中原 Manager 涉及 266 个不同合约，首次完整预热需要约十五分钟。

## 最终变更

- jp-meta 主机配置增加 `[funding_rates].local_ip = "172.31.46.92"`
  （公网 `52.192.54.88`），与 Kline 的 `172.31.46.93` 分开。该地址已绑定网卡，
  25 份交易配置均未使用；检查时其已有 TCP 连接属于访问 Google 的 rclone。
  不改网卡、路由、rclone 或交易绑定。
- 公共费率全局请求最小间隔三秒，滚动五分钟额度 90，并发上限八。
- 403 对全部合约设置至少五分钟冷却；连续拒绝指数退避至一小时，尊重更长的
  `Retry-After`。同一冷却期间的在途失败不重复加倍，在途成功不解除冷却。
- 成功恢复后重置退避；冷却后的排队请求仍间隔三秒发送。保留现有缓存、分页校验、
  加载/不可用响应、30 天范围和每半小时增量刷新。

不修改资金费计算、FIFO、成交归因、账户实际账单出口或开关。Kline 保持原来的
滚动分钟预算和并发模型，新的三秒节流仅作用于公共 Funding 客户端。

## 发布与验证

配置先于 08:28:48 UTC 生效；确认批量请求仍失败后，于 08:40:06 UTC 发布
节流与退避修复。公共 Funding 与 Kline 共用出口仍出现少量 403，改为独立绑定
`172.31.46.92`。始终仅重启 `crypto-cta-manager-web.service`，CPU affinity
保持 24。前端和其他二进制未替换。

最终保守限流二进制于 09:07:47 UTC 生效，Manager PID `3874116`。这次仍保留
`172.31.46.92` 固定出口。首次一秒节流版本的独立出口验证已有十组账户/策略查询
完整恢复，订单报表与实际资金费记录散列均与原固定窗口一致；最终版本另做全量验收。

运行代码基于 `ac4ffcce172284abdd49cf7a5d5999bfa9abdea1` 加本次
`src/funding_rates.rs` 修改；精确补丁保存在下述证据目录。二进制 SHA-256：

```text
8ff48b6a8193a723bac39ebe5037a5c86c1c2edb80a69188eb87d8c36ebe0515
```

本地 `cargo fmt --check`、`cargo check`、七项 `cargo test --lib funding_rates`
以及 `cargo build --release --bin cta_web` 通过。新增测试覆盖并发请求共享节流、
跨合约 403 冷却、连续拒绝退避、Retry-After 和恢复；既有测试继续验证缓存保留、
完整分页/结算数据和资金费边界。

线上 14 组固定窗口账户/策略查询全部恢复，公共 Funding 均完成加载；累计曲线
终点、策略合计与总额一致，订单报告及实际资金费记录的 SHA-256 全部与修复前相同。
trade05/06/07 实际账单采集均为 `ok`。最终修复进程在验收时没有 Funding 错误。

首次保护核对中，26 份配置和 5 张快照表、前端、Manager CPU 24、账单开关均未改；
106 个受保护进程中 96 个保持身份与 affinity，10 个 RX02/trade05 进程已停止，
操作员随后明确确认这是其主动维护。09:31:16 UTC 另项主机维护重启 Manager
为 PID `3922355`，加入 trade08 并更新 sources/kline/funding 配置及 Nginx 映射。
这些并发维护变化单独记录，未用旧备份覆盖。后续多 Virtual 发布以新状态为基线。

本次 Funding 修复的临时备份在验收归档后清理，永久发布清单位于
`/home/ubuntu/crypto_cta_manager/releases/public_funding_20261022/manifest.json`。

证据目录：`artifacts/jp_meta_public_funding_20261022/`。脚本始终通过 4191 Nginx
网关查询；诊断会话仅存在内存并在 `finally` 删除。配置备份只保留在远端，未复制
数据库凭据、交易凭据或会话 token。
