import type { ReactNode } from 'react'
import type { LucideIcon } from 'lucide-react'
import { AppNav, type AppNavId } from './AppNav'
import { cn } from '../lib/cn'

export function AppShell({
  active,
  title,
  subtitle,
  icon: Icon,
  children,
  actions,
  className,
}: {
  active: AppNavId
  title: string
  subtitle: string
  icon: LucideIcon
  children: ReactNode
  actions?: ReactNode
  className?: string
}) {
  return (
    <div className="min-h-screen bg-canvas">
      <header className="sticky top-0 z-40 border-b border-border bg-surface/90 backdrop-blur-md">
        <div className="mx-auto flex w-full max-w-7xl items-center justify-between gap-6 px-4 py-3 sm:px-6 lg:px-8">
          <div className="flex min-w-0 items-center gap-3">
            <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl border border-brand-ring/50 bg-brand-soft text-brand">
              <Icon size={18} strokeWidth={2.2} />
            </div>
            <div className="min-w-0">
              <h1 className="truncate text-base font-semibold tracking-tight text-ink">{title}</h1>
              <p className="truncate text-xs text-muted">{subtitle}</p>
            </div>
          </div>
          <div className="flex min-w-0 items-center gap-3">
            <AppNav active={active} />
            {actions}
          </div>
        </div>
      </header>
      <main className={cn('mx-auto w-full max-w-7xl px-4 py-6 sm:px-6 lg:px-8', className)}>
        {children}
      </main>
    </div>
  )
}

export function PageIntro({
  eyebrow,
  title,
  description,
  actions,
}: {
  eyebrow: string
  title: string
  description?: string
  actions?: ReactNode
}) {
  return (
    <div className="mb-6 flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
      <div>
        <p className="text-[11px] font-semibold uppercase tracking-[0.18em] text-brand">{eyebrow}</p>
        <h2 className="mt-2 text-2xl font-semibold tracking-tight text-ink">{title}</h2>
        {description && <p className="mt-2 max-w-2xl text-sm leading-relaxed text-muted">{description}</p>}
      </div>
      {actions}
    </div>
  )
}

export function StatTile({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint?: string
}) {
  return (
    <div className="rounded-xl border border-border-soft bg-canvas/70 px-4 py-3">
      <p className="text-xs text-muted">{label}</p>
      <p className="mt-1 text-lg font-semibold tabular-nums text-ink">{value}</p>
      {hint && <p className="mt-1 text-[11px] text-subtle">{hint}</p>}
    </div>
  )
}
