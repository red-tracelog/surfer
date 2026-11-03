import * as childProcess from 'child_process'
import * as crypto from 'crypto'
import * as http from 'http'
import * as net from 'net'
import * as vscode from 'vscode'

export interface WcpEndpoint {
  host: string
  port: number
}

export class SurverSession implements vscode.Disposable {
  private constructor(
    private readonly process: childProcess.ChildProcessWithoutNullStreams,
    public readonly url: string,
    public readonly wcpEndpoint: WcpEndpoint | undefined,
  ) {}

  public static async start(
    waveform: vscode.Uri,
    output: vscode.OutputChannel,
    enableWcp: boolean,
  ): Promise<SurverSession> {
    const configuration = vscode.workspace.getConfiguration('surfer')
    const executable = configuration.get<string>('surverPath', 'surver').trim() || 'surver'
    const port = await new Promise<number>((resolve, reject) => {
      const server = net.createServer()
      server.once('error', reject)
      server.listen(0, '127.0.0.1', () => {
        const address = server.address()
        if (address === null || typeof address === 'string') {
          server.close()
          reject(new Error('Failed to reserve a TCP port for Surver'))
          return
        }
        server.close((error) => error ? reject(error) : resolve(address.port))
      })
    })
    const token = crypto.randomBytes(18).toString('hex')
    const arguments_ = [
      '--port', String(port),
      '--bind-address', '127.0.0.1',
      '--token', token,
    ]
    if (enableWcp) {
      arguments_.push('--wcp-address', '127.0.0.1:0')
    }
    arguments_.push(waveform.fsPath)

    output.appendLine(`Starting ${executable} for ${waveform.fsPath}`)
    const process = childProcess.spawn(executable, arguments_, {
      cwd: vscode.workspace.getWorkspaceFolder(waveform)?.uri.fsPath,
      stdio: 'pipe',
    })
    let processError: Error | undefined
    let exitDescription: string | undefined
    process.once('error', (error) => {
      processError = error
    })
    process.once('exit', (code, signal) => {
      exitDescription = signal === null ? `exit code ${code}` : `signal ${signal}`
    })
    process.stdout.on('data', (data: Buffer) => output.append(data.toString()))
    process.stderr.on('data', (data: Buffer) => output.append(data.toString()))

    const localUrl = `http://127.0.0.1:${port}/${token}`
    const deadline = Date.now() + 30_000
    const endpointIsReady = (url: string) => new Promise<boolean>((resolve) => {
      const request = http.get(url, (response) => {
        response.resume()
        resolve(response.statusCode === 200)
      })
      request.setTimeout(1000, () => {
        request.destroy()
        resolve(false)
      })
      request.once('error', () => resolve(false))
    })
    while (!(await endpointIsReady(`${localUrl}/get_status`))) {
      if (processError !== undefined) {
        throw new Error(`Failed to start ${executable}: ${processError.message}`)
      }
      if (exitDescription !== undefined) {
        throw new Error(`${executable} stopped during startup with ${exitDescription}`)
      }
      if (Date.now() >= deadline) {
        process.kill()
        throw new Error(`Timed out waiting for ${executable} to start`)
      }
      await new Promise((resolve) => setTimeout(resolve, 100))
    }

    try {
      const wcpEndpoint = enableWcp
        ? await new Promise<WcpEndpoint>((resolve, reject) => {
            const request = http.get(`${localUrl}/wcp/address`, (response) => {
              const chunks: Buffer[] = []
              response.on('data', (chunk: Buffer) => chunks.push(chunk))
              response.on('end', () => {
                if (response.statusCode !== 200) {
                  reject(new Error(`Surver returned ${response.statusCode} for its WCP address`))
                  return
                }
                try {
                  const endpoint = JSON.parse(Buffer.concat(chunks).toString()) as WcpEndpoint
                  if (
                    typeof endpoint.host !== 'string' ||
                    !Number.isInteger(endpoint.port) ||
                    endpoint.port < 1 ||
                    endpoint.port > 65_535
                  ) {
                    reject(new Error('Surver returned an invalid WCP endpoint'))
                    return
                  }
                  resolve(endpoint)
                } catch (error) {
                  reject(error)
                }
              })
            })
            request.once('error', reject)
          })
        : undefined
      const externalUrl = await vscode.env.asExternalUri(vscode.Uri.parse(localUrl))
      return new SurverSession(
        process,
        externalUrl.toString().replace(/\/$/, ''),
        wcpEndpoint,
      )
    } catch (error) {
      process.kill()
      throw error
    }
  }

  public dispose(): void {
    if (!this.process.killed && this.process.exitCode === null) {
      this.process.kill()
    }
  }
}
