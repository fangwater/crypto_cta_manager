import { ArrowRight } from 'lucide-react'
import type { ReactNode } from 'react'
import { cn } from '../lib/cn'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'

export function AccountCardLayout({
  title, mark, sourceId, badges, children, dimmed,
}: {
  title: ReactNode
  mark: string
  sourceId?: string
  badges: ReactNode
  children: ReactNode
  dimmed?: boolean
}) {
  return (
    <Card className={cn(dimmed && 'opacity-80')}>
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-canvas text-sm font-bold text-brand">{mark}</div>
          <div className="min-w-0">
            <CardTitle className="text-base">{title}</CardTitle>
            {sourceId && <CardDescription className="break-all font-mono text-[11px]">{sourceId}</CardDescription>}
          </div>
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2">{badges}</div>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">{children}</CardContent>
    </Card>
  )
}

export function AccountMetric({ label, value, tone = '', title }: { label: string; value: string; tone?: string; title?: string }) {
  return (
    <div title={title}>
      <p className="text-xs text-muted">{label}</p>
      <p className={cn('mt-1 font-medium tabular-nums text-ink', tone)}>{value}</p>
    </div>
  )
}

export function AccountActionLink({
  href,
  icon,
  label,
  primary = false,
}: {
  href: string
  icon: ReactNode
  label: string
  primary?: boolean
}) {
  return (
    <a
      href={href}
      className={cn(
        'inline-flex items-center gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
        primary
          ? 'bg-brand text-white hover:bg-brand-hover'
          : 'border border-border bg-surface text-ink hover:bg-canvas',
      )}
    >
      {icon}
      {label}
      {primary && <ArrowRight size={14} />}
    </a>
  )
}
