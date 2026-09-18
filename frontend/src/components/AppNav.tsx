import { Activity, BookOpen, LayoutDashboard, Settings, Shield, Scale } from 'lucide-react'
import { useAuth } from './AuthGate'
import { cn } from '../lib/cn'

export type AppNavId = 'workspace' | 'manager' | 'execution-cost' | 'config' | 'docs' | 'admin'

const links: Array<{
  id: AppNavId
  href: string
  label: string
  icon: typeof LayoutDashboard
}> = [
  { id: 'workspace', href: '/manager/workspace/', label: '总览', icon: LayoutDashboard },
  { id: 'manager', href: '/manager/', label: '净值', icon: Activity },
  { id: 'execution-cost', href: '/manager/acquisition-cost/', label: '成本', icon: Scale },
  { id: 'config', href: '/manager/config/position/', label: '策略', icon: Settings },
  { id: 'docs', href: '/manager/docs/', label: '文档', icon: BookOpen },
  { id: 'admin', href: '/manager/admin/', label: '权限', icon: Shield },
]

export function AppNav({ active }: { active: AppNavId }) {
  const { user, logout } = useAuth()
  return (
    <div className="flex items-center gap-2">
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
      <button type="button" onClick={() => void logout()} className="hidden rounded-lg border border-border bg-surface px-3 py-2 text-xs text-muted hover:text-ink sm:inline-flex">退出</button>
    </div>
  )
}
