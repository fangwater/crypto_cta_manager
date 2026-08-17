import type { ReactNode } from 'react'
import { Layers3, Link2, RefreshCw, Settings2, SlidersHorizontal } from 'lucide-react'
import { AppShell, PageIntro } from './AppShell'
import { Alert, Badge } from './ui/Badge'
import { configNav, readSourceId, routes, type ConfigSection } from '../lib/routes'
import { cn } from '../lib/cn'

const sectionIcons = {
  position: Layers3,
  order: SlidersHorizontal,
  bindings: Link2,
} as const

export function ConfigShell({
  section,
  title,
  description,
  saving,
  error,
  notice,
  children,
}: {
  section: ConfigSection
  title: string
  description: string
  saving?: boolean
  error?: string | null
  notice?: string | null
  children: ReactNode
}) {
  const sourceId = readSourceId()

  return (
    <AppShell
      active="config"
      title="策略配置"
      subtitle="仓位策略 · 执行算法 · 策略启用"
      icon={Settings2}
      actions={
        saving ? (
          <Badge tone="brand" className="hidden sm:inline-flex">
            <RefreshCw size={12} className="mr-1 animate-spin-slow" /> 写入中
          </Badge>
        ) : null
      }
    >
      <div className="grid gap-6 xl:grid-cols-[260px_minmax(0,1fr)]">
        <aside className="space-y-4">
          <nav className="rounded-2xl border border-border bg-surface p-2 shadow-[var(--shadow-card)]">
            <p className="px-3 py-2 text-[11px] font-semibold uppercase tracking-[0.16em] text-subtle">
              配置分区
            </p>
            {configNav.map((item) => {
              const Icon = sectionIcons[item.id]
              const href =
                item.id === 'bindings' && sourceId
                  ? routes.configBindings(sourceId)
                  : item.href
              const active = section === item.id
              return (
                <a
                  key={item.id}
                  href={href}
                  className={cn(
                    'mb-1 flex items-start gap-3 rounded-xl px-3 py-3 transition-colors last:mb-0',
                    active
                      ? 'bg-brand-soft text-brand-hover'
                      : 'text-muted hover:bg-canvas hover:text-ink',
                  )}
                >
                  <Icon size={16} className="mt-0.5 shrink-0" />
                  <span>
                    <span className="block text-sm font-medium">{item.label}</span>
                    <span className="mt-0.5 block text-[11px] leading-relaxed opacity-80">
                      {item.hint}
                    </span>
                  </span>
                </a>
              )
            })}
          </nav>
          <div className="rounded-2xl border border-border-soft bg-canvas/60 px-4 py-4 text-xs leading-relaxed text-muted">
            三步：① 仓位策略定义目标仓位 → ② 下单策略维护执行算法 → ③ 策略启用里关联并发布。
          </div>
        </aside>

        <div className="min-w-0 space-y-4">
          <PageIntro eyebrow="Strategy Config" title={title} description={description} />
          {error && <Alert tone="error">{error}</Alert>}
          {notice && <Alert tone="success">{notice}</Alert>}
          {children}
        </div>
      </div>
    </AppShell>
  )
}
