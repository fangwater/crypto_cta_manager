import type { HTMLAttributes } from 'react'
import { cn } from '../../lib/cn'

export function Badge({
  className,
  tone = 'neutral',
  ...props
}: HTMLAttributes<HTMLSpanElement> & {
  tone?: 'neutral' | 'brand' | 'success' | 'warning'
}) {
  const toneClass = {
    neutral: 'border-border bg-canvas text-muted',
    brand: 'border-brand-ring/60 bg-brand-soft text-brand-hover',
    success: 'border-emerald-200 bg-emerald-50 text-emerald-800',
    warning: 'border-amber-200 bg-warning-soft text-warning',
  }[tone]

  return (
    <span
      className={cn(
        'inline-flex items-center rounded-full border px-2.5 py-0.5 text-[11px] font-medium',
        toneClass,
        className,
      )}
      {...props}
    />
  )
}

export function Alert({
  className,
  tone = 'error',
  ...props
}: HTMLAttributes<HTMLDivElement> & { tone?: 'error' | 'success' | 'warning' }) {
  const toneClass = {
    error: 'border-rose-200 bg-danger-soft text-rose-900',
    success: 'border-emerald-200 bg-emerald-50 text-emerald-900',
    warning: 'border-amber-200 bg-warning-soft text-amber-900',
  }[tone]

  return (
    <div
      className={cn('rounded-xl border px-4 py-3 text-sm', toneClass, className)}
      {...props}
    />
  )
}
