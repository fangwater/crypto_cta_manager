import { lazy, StrictMode, Suspense } from 'react'
import { createRoot } from 'react-dom/client'
import { AuthGate } from './components/AuthGate'
import './index.css'

const App = lazy(() => import.meta.env.MODE === 'cn-futures' ? import('./native/App') : import('./App'))

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AuthGate>
      <Suspense fallback={<p className="p-6 text-sm text-muted" role="status">正在加载管理平台…</p>}>
        <App />
      </Suspense>
    </AuthGate>
  </StrictMode>,
)
