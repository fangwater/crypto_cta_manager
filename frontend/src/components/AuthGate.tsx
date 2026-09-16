import { LogIn, ShieldCheck } from 'lucide-react'
import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import {
  getAuthStatus,
  login,
  logout,
  register,
  type AuthStatus,
  type AuthUser,
} from '../api'
import { Alert } from './ui/Badge'
import { Button } from './ui/Button'
import { Card, CardContent } from './ui/Card'
import { Input, Label } from './ui/Field'

interface AuthContextValue {
  user: AuthUser
  logout: () => Promise<void>
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function useAuth() {
  const value = useContext(AuthContext)
  if (!value) throw new Error('useAuth must be used inside AuthGate')
  return value
}

export function AuthGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<AuthStatus | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    getAuthStatus(controller.signal)
      .then(setStatus)
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => controller.abort()
  }, [])

  if (error) {
    return <AuthMessage title="认证服务不可用" detail={error} />
  }
  if (!status) return <AuthMessage title="CTA Manager" detail="正在检查登录状态…" loading />
  if (!status.authenticated || !status.user) {
    return <LoginPanel setupRequired={status.setup_required} onAuthenticated={setStatus} />
  }

  return (
    <AuthContext.Provider
      value={{
        user: status.user,
        logout: async () => {
          await logout()
          setStatus({ authenticated: false, setup_required: false, user: null })
        },
      }}
    >
      {children}
    </AuthContext.Provider>
  )
}

function LoginPanel({
  setupRequired,
  onAuthenticated,
}: {
  setupRequired: boolean
  onAuthenticated: (status: AuthStatus) => void
}) {
  const [mode, setMode] = useState<'login' | 'register'>(setupRequired ? 'register' : 'login')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (mode === 'register' && password !== confirm) {
      setError('两次输入的密码不一致')
      return
    }
    setBusy(true)
    setError(null)
    try {
      const response = mode === 'register'
        ? await register(username, password)
        : await login(username, password)
      onAuthenticated({ authenticated: true, setup_required: false, user: response.user })
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="grid min-h-screen place-items-center bg-canvas px-5 py-10">
      <Card className="w-full max-w-md">
        <CardContent className="p-7 sm:p-9">
          <div className="mb-7 flex items-center gap-3">
            <div className="grid h-11 w-11 place-items-center rounded-xl bg-brand-soft text-brand">
              {mode === 'register' ? <ShieldCheck size={22} /> : <LogIn size={22} />}
            </div>
            <div>
              <h1 className="text-xl font-semibold text-ink">CTA Manager</h1>
              <p className="mt-1 text-xs text-muted">
                {setupRequired ? '首次使用：创建管理员账号' : '登录后查看被授权的账户'}
              </p>
            </div>
          </div>
          {error && <Alert tone="error" className="mb-4">{error}</Alert>}
          <form className="space-y-4" onSubmit={submit}>
            <Label>账号<Input autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} required /></Label>
            <Label>密码<Input type="password" autoComplete={mode === 'register' ? 'new-password' : 'current-password'} value={password} onChange={(event) => setPassword(event.target.value)} required /></Label>
            {mode === 'register' && (
              <Label>确认密码<Input type="password" autoComplete="new-password" value={confirm} onChange={(event) => setConfirm(event.target.value)} required /></Label>
            )}
            <Button className="w-full" variant="primary" size="lg" disabled={busy} type="submit">
              {busy ? '处理中…' : mode === 'register' ? '创建管理员并登录' : '登录'}
            </Button>
          </form>
          {!setupRequired && (
            <p className="mt-5 text-center text-xs text-muted">需要账号？请联系管理员创建</p>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

function AuthMessage({ title, detail, loading = false }: { title: string; detail: string; loading?: boolean }) {
  return (
    <div className="grid min-h-screen place-items-center bg-canvas px-5">
      <div className="text-center">
        <h1 className="text-xl font-semibold text-ink">{title}</h1>
        <p className="mt-2 text-sm text-muted">{detail}</p>
        {loading && <div className="mx-auto mt-5 h-1.5 w-40 animate-pulse rounded-full bg-brand-ring" />}
      </div>
    </div>
  )
}
