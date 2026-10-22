# jp-meta 多 Virtual 叠加 — 2026-10-22

账户现在可跟随多个 Virtual，每项有独立的非负倍率。同一发布绑定的有效份数为
各 Virtual 份数 × 对应倍率之和；同名绑定的仓位策略和下单模板必须一致。
暂停或移除其中一项只撤掉它的贡献。修改 Virtual 时重新计算所有跟随项，
冲突会回滚整个事务；只对最终有效绑定排队发布一次。原有发布名、历史归因、
零目标停止及持久重试机制保留。

账户配置接口统一使用 `{"mode":"follow","follows":[{"virtual_id":"virtual03","multiplier":1}]}`。
前端支持增删跟随行和分别设置倍率，定时状态刷新保留未保存的编辑。
切回独立配置继续保留当前有效份数；空跟随列表须显式选择独立模式，停止所有
贡献则将全部倍率设为零后保存。

## 验证

- `cargo fmt --check`、`cargo check`、`cargo test` 通过；库单元测试 403 项。
- PostgreSQL 16 隔离集群的 16 项集成测试全部通过，包括旧主键维护、组合叠加、
  暂停、删除单项贡献、模板变更、冲突回滚和队列去重；测试集群已停止并删除。
- `npm run build`、`npm run lint` 通过；lint 只有两个既有未使用变量警告。
- `cargo build --release --bin cta_web` 通过。

## 发布

按操作员选择仅发布 jp-meta。发布前读取最新运行配置作为基线，保留另一项维护
新增的 trade08 及停用 RX02 的状态；不覆盖主机 TOML、Nginx 或 systemd 配置。
完整 PostgreSQL 备份为 278,254,256 字节，仅保留在主机临时备份目录。

暂停 Manager 配置写入后，显式执行
`scripts/apply_multiple_virtual_follows_schema.sql`，仅把 `cta_account_follows`
主键从 `source_id` 改为 `(source_id, virtual_id)`，未重写跟随关系、倍率或
有效绑定。原两条跟随为 trade06 → virtual03、trade07 → virtual04，倍率均为 1。
随后原子替换 `cta_web` 和前端链接，只启动 Manager，新 PID `3932323`。

发布目录：`/home/ubuntu/crypto_cta_manager/web-releases/multiple_virtuals_20261022`。
二进制 SHA-256：

```text
ccb2fb93b137e9363d4e235ce764dd37d39c97b67b861987a1355beacdce1269
```

此二进制包含已验证的公共 Funding 节流与 403 退避修复。运行源码基于
`ac4ffcce172284abdd49cf7a5d5999bfa9abdea1` 加本次未提交修改；发布目录保存源码
补丁、各文件散列、维护 SQL 和验收结果。

4191 网关验收：7 个启用账户配置均与 PostgreSQL 一致，原有单跟随返回一项
`follows`，发布队列全部为空。4 个 Virtual 的目录及两条跟随关系正常。前端
HTML、JS、CSS 等 4 个资源逐一通过网关 SHA-256 核对。

发布前后 100 个受保护进程、27 份交易/Nginx 配置、5 张快照表、5 张 Virtual/
有效绑定表、主机 TOML、账单采集设置和 CPU 24 均保持不变。新进程公共 Funding
错误数为零；trade07 固定窗口估算再次得到 0.045864087842642384、42 次结算，
与修复验收一致。后台历史索引按正常重启流程重建，不阻碍账户配置操作。

验证完成后删除本次临时完整数据库备份、旧二进制硬链接和上传暂存文件；
永久发布目录及验证证据保留。没有替实际账户新增跟随、改倍率或发出测试目标。

证据目录：`artifacts/jp_meta_multiple_virtuals_20261022/`。
