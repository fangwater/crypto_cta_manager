import { Plus, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import type { CatalogOrderStrategy, TargetPosition } from '../types'
import { Button } from './ui/Button'
import { FieldHint, Label, Select } from './ui/Field'

export function SymbolOrderStrategyOverridesEditor({
  targets,
  orderStrategies,
  value,
  onChange,
}: {
  targets: Record<string, TargetPosition>
  orderStrategies: CatalogOrderStrategy[]
  value: Record<string, string>
  onChange: (value: Record<string, string>) => void
}) {
  const symbols = useMemo(() => Object.keys(targets).sort(), [targets])
  const [newSymbol, setNewSymbol] = useState('')
  const [newOrderStrategy, setNewOrderStrategy] = useState('')
  const availableSymbols = symbols.filter((symbol) => !(symbol in value))

  useEffect(() => {
    if (!availableSymbols.includes(newSymbol)) {
      setNewSymbol(availableSymbols[0] ?? '')
    }
  }, [availableSymbols, newSymbol])

  useEffect(() => {
    if (!orderStrategies.some((strategy) => strategy.strategy_name === newOrderStrategy)) {
      setNewOrderStrategy(orderStrategies[0]?.strategy_name ?? '')
    }
  }, [newOrderStrategy, orderStrategies])

  const addOverride = () => {
    if (!newSymbol || !newOrderStrategy) return
    onChange({ ...value, [newSymbol]: newOrderStrategy })
  }

  const removeOverride = (symbol: string) => {
    const next = { ...value }
    delete next[symbol]
    onChange(next)
  }

  const updateOverride = (symbol: string, orderStrategyName: string) => {
    if (!orderStrategyName) {
      removeOverride(symbol)
      return
    }
    onChange({ ...value, [symbol]: orderStrategyName })
  }

  return (
    <section className="border-t border-border pt-5">
      <div className="flex flex-wrap items-end gap-2">
        <Label className="min-w-[180px] flex-1">
          Symbol
          <Select value={newSymbol} onChange={(event) => setNewSymbol(event.target.value)}>
            {availableSymbols.length === 0 ? (
              <option value="">没有可覆盖的目标 Symbol</option>
            ) : (
              availableSymbols.map((symbol) => (
                <option key={symbol} value={symbol}>
                  {symbol}
                </option>
              ))
            )}
          </Select>
        </Label>
        <Label className="min-w-[220px] flex-1">
          下单策略模板
          <Select value={newOrderStrategy} onChange={(event) => setNewOrderStrategy(event.target.value)}>
            {orderStrategies.length === 0 ? (
              <option value="">请先创建下单策略模板</option>
            ) : (
              orderStrategies.map((strategy) => (
                <option key={strategy.strategy_name} value={strategy.strategy_name}>
                  {strategy.strategy_name}
                </option>
              ))
            )}
          </Select>
        </Label>
        <Button
          type="button"
          variant="secondary"
          onClick={addOverride}
          disabled={!newSymbol || !newOrderStrategy}
        >
          <Plus size={15} /> 添加覆盖
        </Button>
      </div>
      <FieldHint className="mt-2">
        账户 binding 的下单策略是默认模板。指定 Symbol 后，Exec 使用这里选择的命名模板。
      </FieldHint>

      <div className="mt-5 grid gap-3">
        {Object.entries(value)
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([symbol, orderStrategyName]) => (
            <div key={symbol} className="grid gap-2 border-t border-border pt-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto] sm:items-end">
              <Label>
                Symbol
                <Select value={symbol} disabled>
                  <option value={symbol}>{symbol}</option>
                </Select>
              </Label>
              <Label>
                下单策略模板
                <Select
                  value={orderStrategyName}
                  onChange={(event) => updateOverride(symbol, event.target.value)}
                >
                  {orderStrategies.map((strategy) => (
                    <option key={strategy.strategy_name} value={strategy.strategy_name}>
                      {strategy.strategy_name}
                    </option>
                  ))}
                </Select>
              </Label>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                title={`移除 ${symbol} 下单策略覆盖`}
                onClick={() => removeOverride(symbol)}
              >
                <Trash2 size={15} /> 移除
              </Button>
            </div>
          ))}
      </div>
    </section>
  )
}
