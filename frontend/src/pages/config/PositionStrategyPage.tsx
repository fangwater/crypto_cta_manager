import { LoaderCircle, Save, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { deletePositionStrategy, savePositionStrategy } from '../../api'
import { ConfigShell } from '../../components/ConfigShell'
import { useAuth } from '../../components/AuthGate'
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
  const { user } = useAuth()
  const isAdmin = user.role === 'admin'
  const { positions, orders, loading, error, reloadCatalog } = useStrategyCatalog()
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [selectedPosition, setSelectedPosition] = useState(emptyPosition)
  const [experimentalToken, setExperimentalToken] = useState('')

  const originalPosition = positions.find(
    (item) => item.strategy_name === selectedPosition.strategy_name,
  )
  // Every logged-in user can create; saved strategies keep their publish gate.
  const canEdit = !originalPosition || isAdmin || originalPosition.can_publish === true
  const requiresExperimentalToken = Object.entries(
    selectedPosition.symbol_order_strategy_overrides,
  ).some(([symbol, orderStrategyName]) => {
    if (originalPosition?.symbol_order_strategy_overrides[symbol] === orderStrategyName) return false
    return orders.some(
      (order) =>
        order.strategy_name === orderStrategyName && ['pov', 'chase'].includes(order.order_parameters.algorithm),
    )
  })

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
        <div className="grid grid-cols-1 gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
          <StrategyPicker
            title="策略库"
            emptyLabel="还没有仓位策略，先创建一个。"
            items={positions}
            selectedName={selectedPosition.strategy_name}
            onSelect={(name) => {
              const item = positions.find((entry) => entry.strategy_name === name)
              if (item) {
                setSelectedPosition(item)
                setExperimentalToken('')
              }
            }}
            onCreate={() => {
              setSelectedPosition(emptyPosition())
              setExperimentalToken('')
            }}
            renderMeta={(name) => {
              const item = positions.find((entry) => entry.strategy_name === name)
              if (!item) return ''
              const active = Object.values(item.targets).filter((value) => value.qty !== 'clear' && value.qty !== 0).length
              return `${active} 个非零目标`
            }}
          />

          <Card>
            <CardHeader>
              <CardTitle>编辑模板</CardTitle>
              <CardDescription>所有登录用户均可创建私有策略，创建者可更新目标仓位、管理策略授权，并绑定到自己有配置权限的账户。</CardDescription>
            </CardHeader>
            <CardContent>
              <form
                className="grid gap-4"
                onSubmit={(event) => {
                  event.preventDefault()
                  void withWrite(async () => {
                    const saved = await savePositionStrategy(
                      {
                        ...selectedPosition,
                        targets: selectedPosition.targets,
                      },
                      experimentalToken,
                    )
                    setSelectedPosition(saved)
                    setExperimentalToken('')
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
                {requiresExperimentalToken && (
                  <Label>
                    实验算法 Token
                    <Input
                      type="password"
                      autoComplete="off"
                      value={experimentalToken}
                      onChange={(event) => setExperimentalToken(event.target.value)}
                      required
                    />
                    <FieldHint>新增 POV 或 Chase 的 Symbol 覆盖时必须提供。</FieldHint>
                  </Label>
                )}
                <div className="flex flex-wrap gap-2">
                  <Button type="submit" variant="primary" disabled={saving || !canEdit}>
                    <Save size={15} /> 保存
                  </Button>
                  {!canEdit && (
                    <p className="self-center text-xs text-muted">仅查看：需要该策略的推送或可配置权限才能保存</p>
                  )}
                  {isAdmin && selectedPosition.strategy_name && (
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
