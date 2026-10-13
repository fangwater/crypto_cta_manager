# jp-meta 按币种和策略定位 delta

发布标识 `20261013T060934Z`。操作者明确授权上线币种与策略联合筛选修复；
仅更新 jp-meta 的 `cta_web` 和前端，el01 不变，参考净值保持开启。
发布来自现有工作区，不是纯 HEAD 构建。已验证的源码/产物哈希见
`artifacts/ctk_delta_jp_meta_20261013T060934Z/manifest.json`。

## 修复

上方币种/策略汇总统计整个查询区间，旧版单个目标复盘却只列出当前页的
25 个 delta，较旧的 CTK 或 small 目标容易被最新一批 mid 目标遮住。
新版先按 `rowSymbol`、`rowStrategy` 联合筛选明细，再用 `rowSort` 排序、分页。
点击币种柱形直接定位该币，选择价格差金额最大的目标。复盘区支持独立的
策略下拉框与翻页。原有汇总金额、匹配和理论价格计算不变。

## 验证基准

固定 trade03 一日区间：`startMs=1791784860000`，
`endMs=1791871260000`。更新前有 28,872 笔可比成交、1,558 页目标，
生成耗时 3,373 ms。Manager/Viz/snapshot/Config 均为 HTTP 200，
Viz WebSocket 为 101。已记录 118 个受保护进程及 23 份交易 TOML、
Manager/Nginx 配置的哈希，更新后应保持一致。

本地检查：`cargo check`、10 项成本回归、7 项前端分析测试、前端构建、
release 后端构建通过。Lint 有两条既有未使用声明警告，Vite 有既有包体积提示。

切换时发现 Nginx 的现有 document root 要求发布目录包含 `manager/` 子目录，
最初资源放在发布根目录导致页面短暂 500、资源 404。已将已校验的资源
原子放入 `manager/`，未修改或重启 Nginx；部署脚本同步修正目录布局。

## 线上结果

运行 PID 为 `3078848`，运行二进制 SHA-256 为
`17a787e7a0e6e9985a09124485ef53b36da9fe622367b36ad34855927c884e07`。
API 首次启动等待约 150 秒。目标索引在 `06:17:47 UTC` 完成首次重建，
耗时 320,905 ms、处理 4,452,075 条；此后已就绪，查询没有历史加载警告。

- 公网页面、JS、CSS、favicon 均为 HTTP 200，逐文件哈希匹配发布包。
- 固定区间的原有完整报告（含成交点、汇总和价格曲线）与更新前相同。
- 明细筛选不改变 totals、points、by_symbol 或 by_strategy。
- `CTKUSDT + rbf_small` 固定区间有 259 个 delta、11 页，全部页无重复；
  价格成本差合计 `99.36057284177858 U`，与按原有策略查询得到的 CTK
  汇总一致。逐页金额排序、全部非抽样点的数量加权均价核对通过。
- CTK 在该区间没有 mid 目标，正确返回空结果；不能把“无数据”当成筛选失败。
- Chromium 155 实际操作币种、策略、排序和翻页，small/mid 切换与空结果
  清除旧图通过。浏览器用稍晚的默认一日范围得到 262 个 CTK/small 目标，
  同样 11 页。桌面 1440×1000 和手机 390×1100 已检查，手机无横向溢出，
  实际成交图已绘制，没有 JS 异常，9 次成本请求均明确限定 trade03。
- Manager health/dashboard、trade03 Viz/snapshot/Config 为 200，WebSocket 101；
  另查五个已启用 CTA 账户的 snapshot 网关均为 200。

## 并发操作观察

Manager TOML、服务单元、Nginx Manager snippet、路由映射和 23 份交易 TOML
哈希保持不变，所有仍存在的原进程 PID/启动标识一致。但验证过程中观察到
一个旧 `viz_server`（PID `2392348`）退出，以及
`/etc/nginx/sites-available/crypto_proxy_4191.conf` 被修改、Nginx worker 替换。
部署脚本只重启 `crypto-cta-manager-web.service`，没有操作这些进程或配置。
未推断旧 Viz 退出的原因，也未重启它。进程和站点配置变化单独记录，
不声称全部 118 个受保护进程与全部配置均未变更。

浏览器临时会话、浏览器/profile、远端截图和验证用会话均已清理。
发布暂存及临时回滚文件在成功验证后清理；最终发布目录保留
`RELEASE-MANIFEST.json` 和 `DEPLOYMENT-VALIDATION.json`。
