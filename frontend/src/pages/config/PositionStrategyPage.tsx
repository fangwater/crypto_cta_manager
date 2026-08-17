import { LoaderCircle, Save, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { deletePositionStrategy, savePositionStrategy } from '../../api'
import { ConfigShell } from '../../components/ConfigShell'
import { StrategyPicker } from '../../components/StrategyPicker'
import { TargetPositionsEditor } from '../../components/TargetPositionsEditor'
import { Button } from '../../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../../components/ui/Card'
import { FieldHint, Input, Label } from '../../components/ui/Field'
import { useConfigWrite } from '../../hooks/useConfigWrite'
import { useStrategyCatalog } from '../../hooks/useStrategyCatalog'
import { money } from '../../format'
import { emptyPosition } from '../../lib/strategyDefaults'

export function PositionStrategyPage() {
  const { positions, loading, error, reloadCatalog } = useStrategyCatalog()
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [selectedPosition, setSelectedPosition] = useState(emptyPosition)

  return (
    <ConfigShell
      section="position"
      title="仓位策略"
      description="维护各 CTA 仓位策略的目标持仓与参考权益。这里只定义模板，不会直接写入 Exec。"
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
              const active = Object.values(item.targets).filter((value) => value !== 0).length
              return `参考权益 ${money(item.equity_usdt)} USDT · ${active} 非零`
            }}
          />

          <Card>
            <CardHeader>
              <CardTitle>编辑模板</CardTitle>
              <CardDescription>CTA 仓位策略名通常来自上游系统；参考权益仅用于账户内的比例分配。</CardDescription>
            </CardHeader>
            <CardContent>
              <form
                className="grid gap-4"
                onSubmit={(event) => {
                  event.preventDefault()
                  void withWrite(async () => {
                    const saved = await savePositionStrategy({
                      ...selectedPosition,
                      equity_usdt: Number(selectedPosition.equity_usdt),
                      targets: selectedPosition.targets,
                    })
                    setSelectedPosition(saved)
                    await reloadCatalog()
                  })
                }}
              >
                <div className="grid gap-4 md:grid-cols-2">
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
                  <Label>
                    参考权益 USDT
                    <Input
                      type="number"
                      min="1"
                      step="1"
                      value={selectedPosition.equity_usdt}
                      onChange={(event) =>
                        setSelectedPosition({
                          ...selectedPosition,
                          equity_usdt: Number(event.target.value),
                        })
                      }
                    />
                  </Label>
                </div>
                <FieldHint>
                  一份仓位策略的名义。账户可配置份数 = 实时权益 × 杠杆率 / 这份参考权益。多条策略之间仍按各自参考权益分配比例。
                </FieldHint>
                <TargetPositionsEditor
                  targets={selectedPosition.targets}
                  onChange={(targets) => setSelectedPosition({ ...selectedPosition, targets })}
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
