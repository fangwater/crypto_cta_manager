import { cn } from '../../lib/cn'

export function Tabs<T extends string>({
  value,
  onChange,
  items,
  className,
}: {
  value: T
  onChange: (value: T) => void
  items: Array<{ id: T; label: string; hint?: string }>
  className?: string
}) {
  return (
    <div
      className={cn(
        'inline-flex flex-wrap gap-1 rounded-xl border border-border bg-surface p-1 shadow-sm',
        className,
      )}
    >
      {items.map((item) => {
        const active = item.id === value
        return (
          <button
            key={item.id}
            type="button"
            onClick={() => onChange(item.id)}
            className={cn(
              'rounded-lg px-4 py-2 text-left transition-colors',
              active
                ? 'bg-brand text-white shadow-sm'
                : 'text-muted hover:bg-canvas hover:text-ink',
            )}
          >
            <span className="block text-sm font-medium">{item.label}</span>
            {item.hint && (
              <span
                className={cn(
                  'mt-0.5 block text-[11px]',
                  active ? 'text-emerald-100' : 'text-subtle',
                )}
              >
                {item.hint}
              </span>
            )}
          </button>
        )
      })}
    </div>
  )
}
