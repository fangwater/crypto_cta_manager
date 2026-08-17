import { Activity, BookOpen, LayoutDashboard, Settings } from 'lucide-react'

export type AppNavId = 'workspace' | 'manager' | 'config' | 'docs'

const links: Array<{
  id: AppNavId
  href: string
  label: string
  icon: typeof LayoutDashboard
}> = [
  { id: 'workspace', href: '/', label: '综合总览', icon: LayoutDashboard },
  { id: 'manager', href: '/manager/', label: '净值中心', icon: Activity },
  { id: 'config', href: '/manager/config/', label: '下单配置', icon: Settings },
  { id: 'docs', href: '/manager/docs/', label: 'GitBook', icon: BookOpen },
]

export function AppNav({ active }: { active: AppNavId }) {
  return (
    <>
      {links.map((link) => {
        const Icon = link.icon
        return (
          <a
            key={link.id}
            className={`header-nav-link ${active === link.id ? 'is-active' : ''}`}
            href={link.href}
            title={link.label}
          >
            <Icon size={16} />
            <span>{link.label}</span>
          </a>
        )
      })}
    </>
  )
}
