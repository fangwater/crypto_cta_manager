import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

const apiTarget = process.env.CTA_API_TARGET ?? 'http://127.0.0.1:18201'

export default defineConfig(({ mode }) => ({
  base: '/manager/',
  plugins: [react(), tailwindcss(), {
    name: 'manager-domain-title',
    transformIndexHtml: {
      order: 'pre',
      handler: (html: string) => mode === 'cn-futures'
        ? html.replace('<title>CTA NAV</title>', '<title>国内期货 CTA</title>') : html,
    },
  }],
  server: {
    host: '127.0.0.1',
    port: 5174,
    proxy: {
      '/manager/api': {
        target: apiTarget,
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/manager\/api/, '/api'),
      },
    },
  },
}))
