import { LoaderCircle, Save, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { deletePositionStrategy, savePositionStrategy } from '../../api'
import { ConfigShell } from '../../components/ConfigShell'
import { StrategyPicker } from '../../components/StrategyPicker'
import { SymbolOrderStrategyOverridesEditor } from '../../components/SymbolOrderStrategyOverridesEditor'
import { TargetPositionsEditor } from '../../components/TargetPositionsEditor'
import { Button } from '../../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../../components/ui/Card'
import { FieldHint, Input, Label } from '../../components/ui/Field'
import { useConfigWrite } from '../../hooks/useConfigWrite'
import { useStrategyCatalog } from '../../hooks/useStrategyCatalog'
import { emptyPosition } from '../../lib/strategyDefaults'

export function PositionStrategyPage() {
  const { positions, orders, loading, error, reloadCatalog } = useStrategyCatalog()
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [selectedPosition, setSelectedPosition] = useState(emptyPosition)

  return (
    <ConfigShell
      section="position"
      title="仓位策略"
      description="维护原始目标仓位，并可为策略内的各 Symbol 选择独立下单模板。保存后会按绑定账户份数自动写入 Redis。"
      saving={saving}
      error={error ?? writeError}
      notice={notice}
    >
      {loading ? (
        <Card>
          <CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted">
            <LoaderCircle size={18} className="animate-spin-slow" />
            正在加载仓位策略
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
          <StrategyPicker
            title="策略库"
            emptyLabel="还没有仓位策略，先创建一个。"
            items={positions}
            selectedName={selectedPosition.strategy_name}
            onSelect={(name) => {
              const item = positions.find((entry) => entry.strategy_name === name)
              if (item) setSelectedPosition(item)
            }}
            onCreate={() => setSelectedPosition(emptyPosition())}
            renderMeta={(name) => {
              const item = positions.find((entry) => entry.strategy_name === name)
              if (!item) return ''
              const active = Object.values(item.targets).filter((value) => value.qty !== 0).length
              return `${active} 个非零目标`
            }}
          />

          <Card>
            <CardHeader>
              <CardTitle>编辑模板</CardTitle>
              <CardDescription>CTA 仓位策略名通常来自上游系统；这里保存每份策略的原始目标数量。</CardDescription>
            </CardHeader>
            <CardContent>
              <form
                className="grid gap-4"
                onSubmit={(event) => {
                  event.preventDefault()
                  void withWrite(async () => {
                    const saved = await savePositionStrategy({
                      ...selectedPosition,
                      targets: selectedPosition.targets,
                    })
                    setSelectedPosition(saved)
                    await reloadCatalog()
                    const published = saved.publishes?.length ?? 0
                    return published > 0
                      ? `已保存，并自动推送到 ${published} 个绑定账户`
                      : '已保存；当前没有绑定账户，未写入 Redis'
                  })
                }}
              >
                <Label>
                  策略名
                  <Input
                    value={selectedPosition.strategy_name}
                    onChange={(event) =>
                      setSelectedPosition({
                        ...selectedPosition,
                        strategy_name: event.target.value,
                      })
                    }
                  />
                </Label>
                <FieldHint>
                  账户发布数量 = 原始 qty × 该账户份数；signal 原样发布。
                </FieldHint>
                <TargetPositionsEditor
                  targets={selectedPosition.targets}
                  onChange={(targets) => {
                    const symbol_order_strategy_overrides = Object.fromEntries(
                      Object.entries(selectedPosition.symbol_order_strategy_overrides).filter(
                        ([symbol]) => symbol in targets,
                      ),
                    )
                    setSelectedPosition({
                      ...selectedPosition,
                      targets,
                      symbol_order_strategy_overrides,
                    })
                  }}
                />
                <SymbolOrderStrategyOverridesEditor
                  targets={selectedPosition.targets}
                  orderStrategies={orders}
                  value={selectedPosition.symbol_order_strategy_overrides}
                  onChange={(symbol_order_strategy_overrides) =>
                    setSelectedPosition({ ...selectedPosition, symbol_order_strategy_overrides })
                  }
                />
                <div className="flex flex-wrap gap-2">
                  <Button type="submit" variant="primary" disabled={saving}>
                    <Save size={15} /> 保存
                  </Button>
                  {selectedPosition.strategy_name && (
                    <Button
                      type="button"
                      variant="danger"
                      disabled={saving}
                      onClick={() =>
                        void withWrite(async () => {
                          await deletePositionStrategy(selectedPosition.strategy_name)
                          setSelectedPosition(emptyPosition())
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
