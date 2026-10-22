# 最终 RapidX 三分钟参考核验

代码与前端为同一次替换，见 `release.json`。相对第一轮版本，TWAP 无成交分钟
使用该分钟收盘价；VWAP 全窗口无成交使用三根收盘价均值；页面不显示回退提示。

发布和捕获脚本复用 `../rapidx_three_minute_20261022/`，远端工作目录名替换为
`rapidx_three_minute_close_20261022`。同一 `window.json` 保持固定历史区间，原始
`before.json` 使用第一轮发布前基线，最终响应保存为本机归档中的 `after_close.json`。
`compare.py` 要求三个 RapidX 账户的两条曲线完整、成本缺价为零，并验证实际报告
和普通账户五分钟理论结果未变。`browser_verify.mjs` 检查 8 个桌面/手机场景。

最终结果：8 份实际 NAV 对照一致；三个 RapidX 账户的双参考均完整，394 个
参考价核算通过；8 个浏览器场景通过。`repeat_verification.jsonl` 记录重复查询
未新增行情请求、健康状态及六个 Viz/Config 网关响应。
`protected_final.jsonl` 记录 106 个进程、27 份配置、六张表未变。
验证会话均已删除；独立浏览器已关闭、临时目录已清理。
原始响应及截图保留在仓库外，见 `archive_manifest.json`。

`repeat_body.py` 接在第一轮 `remote_common.py` 和 `capture_body.py` 的会话初始化
代码之后执行（去掉后者从 `try: emit('health', ...)` 开始的捕获主体），将远端
目录名替换为本轮目录，传入 `verify` 参数。会话在 `finally` 中删除。
