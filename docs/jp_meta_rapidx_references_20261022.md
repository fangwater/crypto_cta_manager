# jp-meta RapidX 三分钟双参考发布

发布：`20261022T040321ZRapidxReferences`，功能代码 `b2f3139`。
运行二进制 SHA-256：`c104a265be5b6854e673afb8b823c25c5ebe6e755e96576d768e34af5bdf8cb1`。

RX02 / trade05、RX01 / trade06、RX03 / trade07 同时提供
`理论3min_vwap` 和 `理论3min_twap`。净值的费前、费后曲线及净值差滑点各自
计算；成本分析同时展示两个参考的价格与滑点，可切换参考查看明细。
算法、零成交处理及现行 API 契约见 [说明](rapidx_theoretical_vwap.md)。

只替换 `cta_web` 和 Manager 前端，未替换 systemd unit、主机 TOML 或 Nginx
配置。保留 CPU 24。发布前保存二进制、前端指向及数据库备份到
`/home/ubuntu/crypto_cta_manager/backups/rapidx_three_minute_20261022`；
`rollback.json` 记录可恢复的二进制和前端目录。部署脚本在重启失败时会自动恢复。

发布后的首轮保护检查确认 106 个交易、Viz、Config、Nginx、monitor 等相关
进程身份，27 份配置 SHA-256，以及六张快照/绑定/策略分配表内容均未变化。
只为核验创建短期管理员会话，核验结束删除。未运行 schema DDL，未写 Exec
RocksDB，未修改交易绑定。

重启后先重建本地事实订单缓存，再开放接口；该阶段 Nginx 暂时返回 502。
随后接口恢复 `ok`，理论目标索引继续在后台重建，页面展示 loading 并自动重试。
首次浏览器运行因索引未完成而超时，临时会话已清理；后续验证使用更长等待。

已有公开资金费历史 HTTP 403 问题在发布前基线中已存在，包括 ZRXUSDT、
ZECUSDT 和部分其他合约。这与本次执行参考无关，未更改资金费模型、出口或设置。

核验证据及复现脚本位于 `artifacts/rapidx_three_minute_20261022/`。
固定窗口为 1792619160000 至 1792640760000 Unix 毫秒，四个账户各查询策略及
账户 NAV，另取末一小时做成本对照。原始完整响应单独保存在本机
`/home/fanghaizhou/crypto_cta_manager-archives/20261022-rapidx-three-minute/`，不提交大文件。

第一轮严格公式核验中，8 份固定区间实际 NAV 报告与发布前完全一致，普通账户
trade03 的五分钟理论曲线和成本结果也完全一致。394 个三分钟参考价核算通过。
但零成交分钟导致 RX01 和 RX02 的部分理论曲线不可用。

操作员随后明确选择：TWAP 零成交分钟用收盘价；VWAP 整窗零成交时用三根
收盘价的算术平均，且不在页面标明。最终版本按此规则继续发布与验证。
