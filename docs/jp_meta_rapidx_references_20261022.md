# jp-meta RapidX 三分钟双参考发布

最终发布：`20261022T042311ZRapidxReferences`，功能代码 `a4d19ee`。
运行二进制 SHA-256：`f07399edda74eee9ce3540073032e4ed57acd60acd2d650ea93c4d84e801159f`。

RX02 / trade05、RX01 / trade06、RX03 / trade07 同时提供
`理论3min_vwap` 和 `理论3min_twap`。净值的费前、费后曲线及净值差滑点各自
计算；成本分析同时展示两个参考的价格与滑点，可切换参考查看明细。
算法、零成交处理及现行 API 契约见 [说明](rapidx_theoretical_vwap.md)。

只替换 `cta_web` 和 Manager 前端，未替换 systemd unit、主机 TOML 或 Nginx
配置。保留 CPU 24。发布前保存二进制、前端指向及数据库备份到
`/home/ubuntu/crypto_cta_manager/backups/rapidx_three_minute_close_20261022`；
`rollback.json` 记录可恢复的二进制和前端目录。部署脚本在重启失败时会自动恢复。

发布后的首轮及最终保护检查确认 106 个交易、Viz、Config、Nginx、monitor 等相关
进程身份，27 份配置 SHA-256，以及六张快照/绑定/策略分配表内容均未变化。
只为核验创建短期管理员会话，核验结束删除。未运行 schema DDL，未写 Exec
RocksDB，未修改交易绑定。

重启后先重建本地事实订单缓存，再开放接口；该阶段 Nginx 暂时返回 502。
随后接口恢复 `ok`，理论目标索引继续在后台重建，页面展示 loading 并自动重试。
首次浏览器运行因索引未完成而超时，临时会话已清理；后续验证使用更长等待。

已有公开资金费历史 HTTP 403 问题在发布前基线中已存在，包括 ZRXUSDT、
ZECUSDT 和部分其他合约。这与本次执行参考无关，未更改资金费模型、出口或设置。

第一轮证据位于 `artifacts/rapidx_three_minute_20261022/`，最终版本证据位于
`artifacts/rapidx_three_minute_close_20261022/`。
固定窗口为 1792619160000 至 1792640760000 Unix 毫秒，四个账户各查询策略及
账户 NAV，另取末一小时做成本对照。原始完整响应单独保存在本机
`/home/fanghaizhou/crypto_cta_manager-archives/20261022-rapidx-three-minute/`，不提交大文件。

第一轮版本 `20261022T040321ZRapidxReferences`（`b2f3139`）的严格公式核验中，8 份固定区间实际 NAV 报告与发布前完全一致，普通账户
trade03 的五分钟理论曲线和成本结果也完全一致。394 个三分钟参考价核算通过。
但零成交分钟导致 RX01 和 RX02 的部分理论曲线不可用。

操作员随后明确选择：TWAP 零成交分钟用收盘价；VWAP 整窗零成交时用三根
收盘价的算术平均，且不在页面标明。最终版本已按此规则发布并完成验证。

最终固定区间的 8 份实际 NAV 报告仍与最初发布前完全一致。普通账户 trade03
的五分钟理论曲线、净值差滑点及原有成本指标也保持一致。三个 RapidX 账户的
两种参考均已完成计算，没有缺失价格或不可用原因：

| 账户 | 3min_vwap 曲线点 | 3min_twap 曲线点 | 核对参考价数 |
| --- | ---: | ---: | ---: |
| RX02 / trade05 | 212 | 213 | 104 |
| RX01 / trade06 | 147 | 150 | 200 |
| RX03 / trade07 | 149 | 149 | 90 |

共独立复算 394 个三分钟参考价，验证样本窗口、数量权重、两种参考价及明细的一致性。
两种参考的每分钟成交权重不同，因此稀疏曲线点数可以不同。
四个账户重复查询结果不变，Kline 请求计数保持 1497 → 1497。目标索引已 ready，
最终检查累计处理 5,754,973 条归档消息，Manager 健康状态为 `ok`，无刷新错误。
trade05、trade06、trade07 的 Viz snapshot 和 Config 网关均返回 HTTP 200。

Chromium 完成 8 个场景：RX01 与 RX03 的桌面/手机净值、RX01 的桌面/手机成本，
以及桌面 RX02 和普通 trade03 净值。两条参考的费前、费后曲线均已选中并绘出；
成本页可以切换参考并同时显示两种参考价。无页面运行错误、无横向溢出，RapidX
页面没有回退标注。人工复核 RX01 桌面净值和手机成本截图，显示正常。
临时管理员会话已删除，两个独立 Chromium 实例及其临时目录已清理，截图保存在
上述本机归档目录。`archive_manifest.json` 记录原始响应及截图的大小和 SHA-256。

最终版本的全量 Rust 验证仍为 402 passed、20 ignored；前端构建、8 项成本
分析测试及 lint 通过。保留既有两项 lint warning 和 bundle 大小提示。
