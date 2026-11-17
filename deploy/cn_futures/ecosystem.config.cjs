// Secrets stay in the host-only environment file loaded by the launcher.
const path = require('node:path')
const root = process.env.CN_FUTURES_DEPLOY_ROOT || path.resolve(__dirname, '../..')
const common = {
  cwd: root,
  interpreter: 'none',
  autorestart: true,
  min_uptime: '10s',
  restart_delay: 5000,
  kill_timeout: 60000,
  time: true,
}
const service = (name, role) => ({
  ...common,
  name,
  script: '/bin/bash',
  args: [path.join(root, 'scripts/run_native_service.sh'), role],
  out_file: path.join(root, `logs/${name}.out.log`),
  error_file: path.join(root, `logs/${name}.err.log`),
})
module.exports = {
  apps: [
    service('cn-futures-manager', 'manager'),
    service('cn-futures-sync', 'pull'),
    service('cn-futures-exporter', 'serve'),
    {
      ...common,
      name: 'cn-futures-sync-tunnel',
      script: '/usr/bin/ssh',
      args: ['-NT', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes',
        '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
        '-L', '127.0.0.1:18212:127.0.0.1:18212',
        process.env.CN_FUTURES_SYNC_SSH_HOST || 'sh_dev'],
      out_file: path.join(root, 'logs/cn-futures-sync-tunnel.out.log'),
      error_file: path.join(root, 'logs/cn-futures-sync-tunnel.err.log'),
    },
  ],
}
