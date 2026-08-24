import { LoaderCircle, Save, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { deleteOrderStrategy, saveOrderStrategy } from '../../api'
import { ConfigShell } from '../../components/ConfigShell'
import { OrderParametersForm } from '../../components/OrderParametersForm'
import { StrategyPicker } from '../../components/StrategyPicker'
import { Alert } from '../../components/ui/Badge'
import { Button } from '../../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../../components/ui/Card'
import { FieldHint, Input, Label } from '../../components/ui/Field'
import { useConfigWrite } from '../../hooks/useConfigWrite'
import { useStrategyCatalog } from '../../hooks/useStrategyCatalog'
import { DEFAULT_ORDER_STRATEGY_NAME, orderParameterMeta } from '../../lib/orderParametersMeta'
import { emptyOrder } from '../../lib/strategyDefaults'

export function OrderStrategyPage() {
  const { orders, loading, error, reloadCatalog } = useStrategyCatalog()
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [selectedOrder, setSelectedOrder] = useState(emptyOrder)

  return (
    <ConfigShell
      section="order"
      title="下单策略"
      description="维护 Exec 执行参数模板。命名建议 default_ 前缀；多条仓位策略可共用一条默认模板。"
      saving={saving}
      error={error ?? writeError}
      notice={notice}
    >
      {loading ? (
        <Card>
          <CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted">
            <LoaderCircle size={18} className="animate-spin-slow" />
            正在加载下单策略
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
          <StrategyPicker
            title="模板库"
            emptyLabel="还没有下单策略。当前阶段通常只需一条 default 模板。"
            items={orders}
            selectedName={selectedOrder.strategy_name}
            onSelect={(name) => {
              const item = orders.find((entry) => entry.strategy_name === name)
              if (item) setSelectedOrder(item)
            }}
            onCreate={() => setSelectedOrder(emptyOrder())}
            renderMeta={(name) => {
              const item = orders.find((entry) => entry.strategy_name === name)
              if (!item) return ''
              return `${orderParameterMeta.single_order_usdt.label} ${item.order_parameters.single_order_usdt} USDT`
            }}
          />

          <Card>
            <CardHeader>
              <CardTitle>编辑模板</CardTitle>
              <CardDescription>每个字段下方都有说明。这不是 CTA 仓位策略名，而是执行层参数集合。</CardDescription>
            </CardHeader>
            <CardContent>
              <Alert tone="warning" className="mb-4">
                若两条模板参数完全相同，请合并为一条 default 模板，在账户组合里复用即可。
              </Alert>
              <form
                className="grid gap-4"
                onSubmit={(event) => {
                  event.preventDefault()
                  void withWrite(async () => {
                    const saved = await saveOrderStrategy(selectedOrder)
                    setSelectedOrder(saved)
                    await reloadCatalog()
                  })
                }}
              >
                <Label>
                  模板名
                  <Input
                    value={selectedOrder.strategy_name}
                    placeholder={DEFAULT_ORDER_STRATEGY_NAME}
                    onChange={(event) =>
                      setSelectedOrder({ ...selectedOrder, strategy_name: event.target.value })
                    }
                  />
                  <FieldHint>例如 {DEFAULT_ORDER_STRATEGY_NAME}，不要复用 CTA 仓位策略名。</FieldHint>
                </Label>
                <OrderParametersForm
                  value={selectedOrder.order_parameters}
                  onChange={(order_parameters) => setSelectedOrder({ ...selectedOrder, order_parameters })}
                />
                <div className="flex flex-wrap gap-2">
                  <Button type="submit" variant="primary" disabled={saving}>
                    <Save size={15} /> 保存
                  </Button>
                  {selectedOrder.strategy_name && (
                    <Button
                      type="button"
                      variant="danger"
                      disabled={saving}
                      onClick={() =>
                        void withWrite(async () => {
                          await deleteOrderStrategy(selectedOrder.strategy_name)
                          setSelectedOrder(emptyOrder())
                          await reloadCatalog()
                        })
                      }
                    >
                      <Trash2 size={15} /> 删除
                    </Button>
                  )}
                </div>
              </form>
            </CardContent>
          </Card>
        </div>
      )}
    </ConfigShell>
  )
}
