import { useState, type ReactNode } from 'react'
import {
  ArrowUpRight,
  Check,
  Copy,
  Info,
  TriangleAlert,
  type LucideIcon,
} from 'lucide-react'
import { cn } from '../../lib/cn'
import { Button } from '../ui/Button'

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'DELETE'

const methodClass: Record<HttpMethod, string> = {
  GET: 'bg-sky-50 text-sky-800 ring-sky-200',
  POST: 'bg-emerald-50 text-emerald-800 ring-emerald-200',
  PUT: 'bg-amber-50 text-amber-800 ring-amber-200',
  DELETE: 'bg-rose-50 text-rose-800 ring-rose-200',
}

export function MethodBadge({ method }: { method: HttpMethod }) {
  return (
    <span
      className={cn(
        'inline-flex min-w-14 items-center justify-center rounded-md px-2 py-0.5 text-[11px] font-semibold tracking-wide ring-1 ring-inset',
        methodClass[method],
      )}
    >
      {method}
    </span>
  )
}

export function CodeBlock({
  children,
  label,
}: {
  children: string
  label?: string
}) {
  const [copied, setCopied] = useState(false)

  return (
    <div className="not-prose my-5 overflow-hidden rounded-2xl border border-border bg-[#f8fafc]">
      <div className="flex items-center justify-between gap-3 border-b border-border-soft px-4 py-2">
        <span className="text-[11px] font-medium tracking-wide text-muted">
          {label ?? '示例'}
        </span>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-muted"
          onClick={async () => {
            await navigator.clipboard.writeText(children)
            setCopied(true)
            window.setTimeout(() => setCopied(false), 1200)
          }}
        >
          {copied ? <Check size={13} /> : <Copy size={13} />}
          {copied ? '已复制' : '复制'}
        </Button>
      </div>
      <pre className="overflow-x-auto p-4 font-mono text-[12.5px] leading-6 text-ink">
        <code>{children}</code>
      </pre>
    </div>
  )
}

export function Callout({
  tone = 'info',
  title,
  children,
}: {
  tone?: 'info' | 'warning'
  title?: string
  children: ReactNode
}) {
  const Icon = tone === 'warning' ? TriangleAlert : Info
  return (
    <div
      className={cn(
        'not-prose my-5 flex gap-3 rounded-2xl border px-4 py-3.5 text-sm leading-6',
        tone === 'warning'
          ? 'border-amber-200 bg-warning-soft text-amber-950'
          : 'border-brand-ring/50 bg-brand-soft text-ink',
      )}
    >
      <Icon size={16} className={tone === 'warning' ? 'mt-0.5 text-warning' : 'mt-0.5 text-brand'} />
      <div className="min-w-0">
        {title ? <p className="font-semibold">{title}</p> : null}
        <div className={title ? 'mt-1 text-[13px] text-ink/80' : undefined}>{children}</div>
      </div>
    </div>
  )
}

export function Endpoint({
  method,
  path,
  note,
}: {
  method: HttpMethod
  path: string
  note?: string
}) {
  return (
    <div className="not-prose my-5 overflow-hidden rounded-2xl border border-border bg-surface">
      <div className="flex flex-wrap items-center gap-3 px-4 py-3">
        <MethodBadge method={method} />
        <code className="min-w-0 flex-1 break-all font-mono text-[12.5px] text-ink">{path}</code>
      </div>
      {note ? (
        <p className="border-t border-border-soft bg-canvas/70 px-4 py-2 text-xs text-muted">{note}</p>
      ) : null}
    </div>
  )
}

export function QuickLinks({
  items,
}: {
  items: Array<{ href: string; title: string; detail: string; icon: LucideIcon }>
}) {
  return (
    <div className="not-prose my-5 grid gap-3 sm:grid-cols-2">
      {items.map((item) => {
        const Icon = item.icon
        return (
          <a
            key={item.href + item.title}
            href={item.href}
            className="group flex items-start gap-3 rounded-2xl border border-border bg-surface p-4 transition-colors hover:border-brand-ring hover:bg-brand-soft/40"
          >
            <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-brand-soft text-brand">
              <Icon size={16} />
            </span>
            <span className="min-w-0">
              <span className="flex items-center gap-1 font-semibold text-ink">
                {item.title}
                <ArrowUpRight size={14} className="text-subtle transition-colors group-hover:text-brand" />
              </span>
              <span className="mt-1 block text-xs leading-5 text-muted">{item.detail}</span>
            </span>
          </a>
        )
      })}
    </div>
  )
}

export function FormulaGrid({
  items,
}: {
  items: Array<{ label: string; value: ReactNode }>
}) {
  return (
    <div className="not-prose my-5 grid gap-3 sm:grid-cols-2">
      {items.map((item) => (
        <div key={item.label} className="rounded-2xl border border-border bg-canvas/70 px-4 py-3">
          <p className="text-[11px] font-medium uppercase tracking-[0.14em] text-muted">{item.label}</p>
          <p className="mt-1.5 font-mono text-[13px] leading-6 text-ink">{item.value}</p>
        </div>
      ))}
    </div>
  )
}

export function Steps({ items }: { items: ReactNode[] }) {
  return (
    <ol className="not-prose my-5 space-y-3">
      {items.map((item, index) => (
        <li
          key={index}
          className="flex gap-3 rounded-2xl border border-border bg-surface px-4 py-3.5"
        >
          <span className="grid h-7 w-7 shrink-0 place-items-center rounded-full bg-brand-soft text-xs font-semibold text-brand">
            {index + 1}
          </span>
          <div className="min-w-0 pt-0.5 text-sm leading-6 text-ink">{item}</div>
        </li>
      ))}
    </ol>
  )
}

export function StatusTable({
  rows,
}: {
  rows: Array<{ code: string; meaning: string; tone?: 'ok' | 'warn' | 'bad' }>
}) {
  const toneClass = {
    ok: 'bg-emerald-50 text-emerald-800',
    warn: 'bg-amber-50 text-amber-800',
    bad: 'bg-rose-50 text-rose-800',
  }
  return (
    <div className="not-prose my-5 overflow-hidden rounded-2xl border border-border">
      {rows.map((row) => (
        <div
          key={row.code}
          className="flex items-start gap-4 border-b border-border-soft px-4 py-3 last:border-b-0"
        >
          <span
            className={cn(
              'mt-0.5 inline-flex min-w-12 justify-center rounded-md px-2 py-0.5 font-mono text-xs font-semibold',
              toneClass[row.tone ?? 'ok'],
            )}
          >
            {row.code}
          </span>
          <p className="text-sm leading-6 text-ink">{row.meaning}</p>
        </div>
      ))}
    </div>
  )
}
