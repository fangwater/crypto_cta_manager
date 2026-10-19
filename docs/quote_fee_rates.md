# 按报价币估算交易手续费

每个账户保留默认 Maker、Taker 和理论 TWAP 费率，并可在账户配置的“估算费率”中
分别设置 USDT、USDC、USD。覆盖项以 `quote_fee_rates` 保存在 Manager PostgreSQL
的 `cta_order_sources`；不存在覆盖项时继续使用账户默认值。

线性合约按 symbol 后缀选报价币；币本位合约使用 USD；RapidX 裸资产现货使用
USDT。所有设置仍按 source_id 隔离。零手续费和负费率（返佣）均有效。
事实成交继续优先使用交易所流动性角色，缺失时使用现有订单类型回退。
内部对敲不收交易费用。成交数量、FIFO、策略归属和资金费计算不变。

事实 NAV、策略 NAV、逐笔策略损益以及执行成本分析使用同一报价币选择规则。
保存新费率会重算事实历史，并使 FIFO 检查点及查询缓存失效。其他账户的缓存可以复用。
Dashboard 和账户页显示默认费率及所有报价币覆盖项。

理论 TWAP 新目标归档在每个账户上保存默认费率和 `theoretical_fee_rates_by_quote`。
停止绑定、手动发布、自动发布以及虚拟账户跟随变更均保存这一组冻结费率。
历史消息已经保存的费率仍然有效，不回写、不覆盖；完全缺失费率的旧消息按查询时的
账户及报价币设置回退。理论 NAV 缓存键包含各报价币费率。

## 已有数据库维护

部署新二进制前，备份 `cta_order_sources`，显式执行
`scripts/apply_quote_fee_schema.sql`。这是一个空对象默认值的新增 JSONB 列，
不修改现有费率、仓位快照或历史目标。常规启动和报表查询不执行 DDL。
新数据库由 `migrations/schema.sql` 定义当前结构。

账户设置 API 的 `PUT /api/catalog/accounts/{source_id}/fee-rates` 请求例子：

```json
{
  "maker_fee_rate": 0.0001,
  "taker_fee_rate": 0.0003,
  "theoretical_twap_fee_rate": 0.0004,
  "quote_fee_rates": {
    "USDC": {
      "maker_fee_rate": 0,
      "taker_fee_rate": 0.000165,
      "theoretical_twap_fee_rate": 0.0000825
    }
  }
}
```

`quote_fee_rates` 省略时保留已有覆盖项；发送空对象显式清除所有覆盖项。
提供对象时替换覆盖项集合。每项要求三种费率都是有限小数，键只能是大写
USDT、USDC、USD。UI 可独立取消某种报价币的覆盖。

TOML 的每个 `[[sources]]` 也支持同结构 `quote_fee_rates`，仅作为新账户的初始化值
及无 PostgreSQL 诊断模式的配置；已注册账户以 PostgreSQL 为准，重启不会覆盖用户设置。
费率是人工确认的账户估算配置，不自动同步交易所变化，也不表示实际逐笔扣费账单。
