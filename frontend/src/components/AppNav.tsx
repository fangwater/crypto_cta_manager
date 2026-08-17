import { Activity, BookOpen, LayoutDashboard, Settings } from 'lucide-react'
import { cn } from '../lib/cn'

export type AppNavId = 'workspace' | 'manager' | 'config' | 'docs'

const links: Array<{
  id: AppNavId
  href: string
  label: string
  icon: typeof LayoutDashboard
}> = [
  { id: 'workspace', href: '/', label: '总览', icon: LayoutDashboard },
  { id: 'manager', href: '/manager/', label: '净值', icon: Activity },
  { id: 'config', href: '/manager/config/position/', label: '策略', icon: Settings },
  { id: 'docs', href: '/manager/docs/', label: '文档', icon: BookOpen },
]

export function AppNav({ active }: { active: AppNavId }) {
  return (
    <nav className="hidden items-center gap-1 rounded-xl border border-border bg-canvas/80 p-1 md:flex">
      {links.map((link) => {
        const Icon = link.icon
        const isActive = active === link.id
        return (
          <a
            key={link.id}
            href={link.href}
            title={link.label}
            className={cn(
              'inline-flex items-center gap-1.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
              isActive
                ? 'bg-surface text-brand shadow-sm'
                : 'text-muted hover:bg-surface/70 hover:text-ink',
            )}
          >
            <Icon size={15} strokeWidth={2.1} />
            <span>{link.label}</span>
          </a>
        )
      })}
    </nav>
  )
}
