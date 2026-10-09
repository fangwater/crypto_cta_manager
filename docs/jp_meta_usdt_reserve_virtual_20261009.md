# jp-meta USDT 保留金额与 Virtual 页面发布验证

2026-10-09 UTC 发布 `20261009T073542Z`，运行代码提交
`34d571053fb48556bb10278201818df4d451c078`。功能提交为 `1e3a2b3`，
Virtual UI 修正提交为 `34d5710`。使用既有工作目录构建，通过
`scripts/deploy_host.sh --target jp-meta --skip-build --manager-only` 发布。

## 生效行为

自动理财新增按实际账户配置的 `reserve_usdt`，前端显示为「保留 USDT 余额」。
计算申购金额时，先取合约 USDT 钱包余额减去保留金额，再与可划转金额、
保证金空间、交易所额度和单轮限额取最小值，向下保留两位小数。
最终金额必须严格超过触发下限且至少为 1 USDT。保留金额不会再次从
可划转金额或其他限额扣除。例如钱包 1,500 USDT、保留 1,000 USDT，
其余额度足够时兑换 500 USDT。

新字段接受有限非负金额，旧文件缺省为 0。上线后四个账户的保留金额均为 0，
原有启用状态、周期、触发下限和单轮限额保持原值；用户可在自动理财页按账户修改。
保存请求只发送五个可写配置字段，避免将运行状态发回严格的保存接口。
理财操作使用现有登录与账户配置权限，无额外操作 Token。

新 Virtual 账户自动分配 `virtual01`、`virtual02` 等编号，别名可编辑。
创建、编辑和跟随无需额外实验算法 Token，既有管理员、账户与策略权限、
市场校验和保留对冲策略校验继续生效。
页面默认打开首个已有账户；编号和别名分行显示，策略名称、下单模板和份数
分区排布，添加策略使用显式按钮，保存、撤销和删除位于操作区。
长名称支持换行，窄屏堆叠控件，刷新同步状态保留未保存编辑。
仍有跟随账户时禁用删除并解释原因。实际账户的跟随模式表单也采用响应式排布。

## 验证

- `cargo fmt --check`、`cargo check`、`git diff --check` 通过。
- `cargo test --quiet`：278 个库测试通过、3 个既有条件忽略；
  `nav_strategy_snapshot` 的 6 个测试通过。
- 6 个 PostgreSQL 集成测试在隔离的 PostgreSQL 16.15 数据库运行通过，
  覆盖并发自动编号、别名修改后保留跟随关系和既有数据库初始化行为。
- BFUSD 的 12 个测试通过，覆盖保留金额持久化、账户隔离、非法值、额度取最小值
  和小数截断。本地模拟交易所验证：余额 900、1,000、1,100 USDT，
  保留 1,000、触发 100 时无请求；余额 1,500 时划出并申购 500 USDT。
- 四个发布二进制 `cta_web`、`nav_rebuild`、`nav_snapshot`、
  `nav_strategy_snapshot` 的 release 构建通过。
- `npm run build`、`npm run lint` 通过；lint 仅有既有未使用变量提示。
- 在 el_dev Chromium 中加载同一生产构建，用模拟目录验证桌面 1440×1000、
  平板 768×1024、手机 390×1100。三种尺寸均无页面横向溢出、无 JavaScript
  页面错误；默认选择、刷新保留草稿、撤销、添加策略、份数校验、保存、
  自动编号新建及跟随/独立表单布局均通过。此过程没有启动 Vite。
- 线上请求全部通过 jp-meta 的 4191 Nginx。四个实际账户返回新字段及原有配置，
  负保留金额保存返回 400，未登录访问返回 401；BNB 状态可正常读取。
- 线上空 Virtual 模板的创建、别名修改和删除均成功，无额外 Token。
  验证后原目录、跟随关系和待发布队列恢复原状，跟随及队列数量均为 0。
- Manager 健康状态为 `ok`、4 个来源、无刷新错误。Manager、自动理财、Virtual
  以及 trade03 Viz、snapshot、Config 页面均返回 200；Viz WebSocket 返回 101。
- 网关返回的 HTML、JS、CSS 与发布文件 SHA-256 一致。前端资源为
  `index-D606wdPE.js`、`index-CAgHSgfj.css`。

## 运行隔离与回滚材料

仅重启 `crypto-cta-manager-web.service`，PID 从 535500 变为 636577。
131 个受保护的交易、行情、Exec、Viz、Config、Nginx、监控及基础服务进程
保留 PID 和启动时间。140 份既有运行配置的哈希保持一致，Manager live TOML
和 BNB 配置参数未改动。不执行数据库 DDL，不修改 Exec RocksDB，不切换真实
账户跟随模式。

正式回滚备份及验证记录保留于：
`/home/ubuntu/crypto_cta_manager/backups/usdt-reserve-virtual-20261009T073428Z`。
其中包含旧二进制、旧前端发布指向、旧配置、Manager unit、PostgreSQL 备份、
`BASELINE.json` 和 `VERIFICATION.json`。凭据文件未复制。
发布校验临时文件和 `.next` 文件已不存在，临时验证会话及 Virtual 模板已删除，
浏览器截图、测试脚本和临时资源在验证后清理。

发布清单中的 `cta_web` SHA-256：
`f330ce9f9fc58820929f73fa45b11b0d57ab1236bbbc4344f5527d23571002fa`。
