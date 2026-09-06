export function normalizePath(pathname: string) {
  return pathname.replace(/\/+$/, '') || '/'
}

export function readSourceId() {
  return new URLSearchParams(window.location.search).get('source')?.trim() || ''
}

export const routes = {
  workspace: '/manager/workspace/',
  account: (sourceId: string) =>
    `/manager/account/?source=${encodeURIComponent(sourceId)}`,
  nav: (sourceId?: string) =>
    sourceId ? `/manager/?source=${encodeURIComponent(sourceId)}` : '/manager/',
  positions: (sourceId?: string) =>
    sourceId
      ? `/manager/positions/?source=${encodeURIComponent(sourceId)}`
      : '/manager/positions/',
  executionCost: (sourceId?: string) =>
    sourceId
      ? `/manager/execution-cost/?source=${encodeURIComponent(sourceId)}`
      : '/manager/execution-cost/',
  configPosition: '/manager/config/position/',
  configOrder: '/manager/config/order/',
  configBindings: (sourceId?: string) =>
    sourceId
      ? `/manager/config/bindings/?source=${encodeURIComponent(sourceId)}`
      : '/manager/config/bindings/',
  docs: '/manager/docs/',
} as const

export type ConfigSection = 'position' | 'order' | 'bindings'

export const configNav: Array<{
  id: ConfigSection
  href: string
  label: string
  hint: string
}> = [
  {
    id: 'position',
    href: routes.configPosition,
    label: '仓位策略',
    hint: '每份策略的原始目标仓位',
  },
  {
    id: 'order',
    href: routes.configOrder,
    label: '下单策略',
    hint: '执行算法参数模板',
  },
  {
    id: 'bindings',
    href: routes.configBindings(),
    label: '策略启用',
    hint: '为策略选择执行算法并发布',
  },
]
