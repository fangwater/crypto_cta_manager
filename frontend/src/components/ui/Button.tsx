import type { ButtonHTMLAttributes } from 'react'
import { cn } from '../../lib/cn'

type Variant = 'primary' | 'secondary' | 'ghost' | 'danger'
type Size = 'sm' | 'md' | 'lg'

const variantClass: Record<Variant, string> = {
  primary:
    'bg-brand text-white shadow-sm hover:bg-brand-hover focus-visible:ring-brand-ring disabled:bg-brand/50',
  secondary:
    'border border-border bg-surface text-ink hover:bg-canvas focus-visible:ring-brand-ring disabled:opacity-50',
  ghost:
    'text-muted hover:bg-canvas hover:text-ink focus-visible:ring-brand-ring disabled:opacity-50',
  danger:
    'border border-rose-200 bg-danger-soft text-danger hover:bg-rose-100 focus-visible:ring-rose-200 disabled:opacity-50',
}

const sizeClass: Record<Size, string> = {
  sm: 'h-8 gap-1.5 px-3 text-xs',
  md: 'h-9 gap-2 px-3.5 text-sm',
  lg: 'h-10 gap-2 px-4 text-sm',
}

export function Button({
  className,
  variant = 'secondary',
  size = 'md',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: Variant
  size?: Size
}) {
  return (
    <button
      className={cn(
        'inline-flex items-center justify-center whitespace-nowrap rounded-lg font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 disabled:cursor-not-allowed',
        variantClass[variant],
        sizeClass[size],
        className,
      )}
      {...props}
    />
  )
}
