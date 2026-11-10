import * as vscode from 'vscode'
import * as cp from 'child_process'
import * as crypto from 'crypto'
import * as http from 'http'
import * as net from 'net'

export type LocalMode = 'auto' | 'surver' | 'surfer' | 'off'

export interface LocalProcess {
  // For surver: the URL the embedded wasm Surfer should load. Undefined for native surfer.
  url?: string
  dispose(): void
}

let output: vscode.OutputChannel | undefined
function log(msg: string) {
  output ??= vscode.window.createOutputChannel('Surfer')
  output.appendLine(msg)
}

export function getConfig() {
  const cfg = vscode.workspace.getConfiguration('surfer')
  return {
    mode: cfg.get<LocalMode>('localMode', 'auto'),
    surverPath: cfg.get<string>('surverPath', '') || 'surver',
    surferPath: cfg.get<string>('surferPath', '') || 'surfer',
  }
}

// Check that an executable exists and runs, by invoking `<exe> --version`.
export function isInstalled(exe: string): Promise<boolean> {
  return new Promise((resolve) => {
    const p = cp.spawn(exe, ['--version'], { stdio: 'ignore' })
    p.on('error', () => resolve(false))
    p.on('exit', (code) => resolve(code === 0))
  })
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer()
    srv.on('error', reject)
    srv.listen(0, '127.0.0.1', () => {
      const port = (srv.address() as net.AddressInfo).port
      srv.close(() => resolve(port))
    })
  })
}

function canConnect(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const s = net.connect(port, '127.0.0.1')
    s.on('connect', () => {
      s.destroy()
      resolve(true)
    })
    s.on('error', () => resolve(false))
  })
}

function pipeLogs(name: string, p: cp.ChildProcess) {
  p.stdout?.on('data', (d) => log(`[${name}] ${d}`.trimEnd()))
  p.stderr?.on('data', (d) => log(`[${name}] ${d}`.trimEnd()))
}

// surver sends no CORS headers, so the webview (a different origin) cannot fetch from it directly.
// This tiny reverse proxy forwards requests to surver and adds the headers the browser needs.
function startCorsProxy(targetPort: number): Promise<{ port: number; server: http.Server }> {
  const server = http.createServer((req, res) => {
    const cors: http.OutgoingHttpHeaders = {
      'Access-Control-Allow-Origin': '*',
      'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
      'Access-Control-Allow-Headers': req.headers['access-control-request-headers'] ?? '*',
      'Access-Control-Allow-Private-Network': 'true',
      // Surfer's client checks these custom response headers
      'Access-Control-Expose-Headers': 'server, x-surfer-version, x-wellen-version, content-type',
    }
    if (req.method === 'OPTIONS') {
      res.writeHead(204, cors)
      res.end()
      return
    }
    const upstream = http.request(
      {
        host: '127.0.0.1',
        port: targetPort,
        path: req.url,
        method: req.method,
        headers: { ...req.headers, host: `127.0.0.1:${targetPort}` },
      },
      (up) => {
        res.writeHead(up.statusCode ?? 502, { ...up.headers, ...cors })
        up.pipe(res)
      }
    )
    upstream.on('error', (e) => {
      res.writeHead(502, cors)
      res.end(String(e))
    })
    req.pipe(upstream)
  })
  return new Promise((resolve, reject) => {
    server.on('error', reject)
    server.listen(0, '127.0.0.1', () => {
      resolve({ port: (server.address() as net.AddressInfo).port, server })
    })
  })
}

export async function startSurver(exe: string, file: string): Promise<LocalProcess> {
  const port = await freePort()
  const token = crypto.randomBytes(8).toString('hex') // surver requires >= 8 chars
  const proc = cp.spawn(
    exe,
    ['--port', String(port), '--bind-address', '127.0.0.1', '--token', token, file],
    { stdio: ['ignore', 'pipe', 'pipe'] }
  )
  pipeLogs('surver', proc)

  let exited = false
  const failed = new Promise<never>((_, reject) => {
    proc.on('error', reject)
    proc.on('exit', (code) => {
      exited = true
      reject(new Error(`surver exited with code ${code}`))
    })
  })
  const ready = (async () => {
    for (let i = 0; i < 100 && !exited; i++) {
      if (await canConnect(port)) {
        return
      }
      await new Promise((r) => setTimeout(r, 100))
    }
    throw new Error('Timed out waiting for surver to start')
  })()

  try {
    await Promise.race([ready, failed])
  } catch (e) {
    proc.kill()
    throw e
  }
  let proxy: { port: number; server: http.Server }
  try {
    proxy = await startCorsProxy(port)
  } catch (e) {
    proc.kill()
    throw e
  }
  return {
    url: `http://127.0.0.1:${proxy.port}/${token}`,
    dispose: () => {
      proxy.server.close()
      proc.kill()
    },
  }
}

export async function startNativeSurfer(exe: string, file: string): Promise<LocalProcess> {
  const proc = cp.spawn(exe, [file], { stdio: ['ignore', 'pipe', 'pipe'] })
  pipeLogs('surfer', proc)
  // Surface immediate launch failures (e.g. no display) instead of silently doing nothing.
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, 1000)
    proc.on('error', (e) => {
      clearTimeout(timer)
      reject(e)
    })
    proc.on('exit', (code) => {
      clearTimeout(timer)
      reject(new Error(`surfer exited with code ${code}`))
    })
  })
  proc.removeAllListeners('exit')
  return { dispose: () => proc.kill() }
}

// Pick and start a local backend according to settings.
// Returns undefined when the embedded wasm Surfer should be used on its own.
export async function startLocal(
  uri: vscode.Uri
): Promise<{ kind: 'surver' | 'surfer'; proc: LocalProcess } | undefined> {
  const { mode, surverPath, surferPath } = getConfig()
  if (mode === 'off' || uri.scheme !== 'file') {
    return undefined
  }

  const tryStart = async (kind: 'surver' | 'surfer') => {
    const exe = kind === 'surver' ? surverPath : surferPath
    if (!(await isInstalled(exe))) {
      log(`${exe} not found`)
      if (mode === kind) {
        vscode.window.showWarningMessage(
          `Surfer: '${exe}' not found, falling back to the built-in viewer. Set surfer.${kind}Path or change surfer.localMode.`
        )
      }
      return undefined
    }
    try {
      const proc =
        kind === 'surver'
          ? await startSurver(exe, uri.fsPath)
          : await startNativeSurfer(exe, uri.fsPath)
      return { kind, proc }
    } catch (e) {
      log(`Failed to start ${exe}: ${e}`)
      vscode.window.showWarningMessage(
        `Surfer: failed to start ${exe} (${e}), falling back to the built-in viewer.`
      )
      return undefined
    }
  }

  if (mode === 'surver' || mode === 'auto') {
    return tryStart('surver')
  }
  return tryStart('surfer')
}
