import * as net from 'net'
import * as path from 'path'
import type * as vscode from 'vscode'

import { WcpEndpoint } from './surverSession'

export class LocalWcpSession implements vscode.Disposable {
  private socket: net.Socket | undefined
  private receiveBuffer = Buffer.alloc(0)
  private readonly pendingServerMessages: string[] = []

  private constructor(
    private readonly server: net.Server,
    public readonly wcpEndpoint: WcpEndpoint,
    private readonly waveformPath: string,
    private readonly waveformUrl: string,
    private readonly onClientMessage: (message: string) => PromiseLike<boolean>,
    private readonly output: vscode.OutputChannel
  ) {}

  public static async start(
    waveform: vscode.Uri,
    waveformUrl: string,
    onClientMessage: (message: string) => PromiseLike<boolean>,
    output: vscode.OutputChannel
  ): Promise<LocalWcpSession> {
    const server = net.createServer()
    const port = await new Promise<number>((resolve, reject) => {
      const onError = (error: Error) => reject(error)
      server.once('error', onError)
      server.listen(0, '127.0.0.1', () => {
        server.off('error', onError)
        const address = server.address()
        if (address === null || typeof address === 'string') {
          reject(new Error('Failed to create a local WCP endpoint'))
          return
        }
        resolve(address.port)
      })
    })
    const session = new LocalWcpSession(
      server,
      { host: '127.0.0.1', port },
      waveform.fsPath,
      waveformUrl,
      onClientMessage,
      output
    )
    server.on('connection', (socket) => session.accept(socket))
    server.on('error', (error) => output.appendLine(`Local WCP server error: ${error}`))
    output.appendLine(`Local WCP relay listening on 127.0.0.1:${port}`)
    return session
  }

  public sendServerMessage(message: string): void {
    const decoded = JSON.parse(message) as Record<string, unknown>
    if (
      decoded.type === 'event' &&
      decoded.event === 'waveforms_loaded' &&
      decoded.source === this.waveformUrl
    ) {
      decoded.source = this.waveformPath
      message = JSON.stringify(decoded)
    }
    this.output.appendLine(`WCP server -> client: ${message}`)
    if (this.socket?.writable) {
      this.socket.write(`${message}\0`)
    } else {
      this.pendingServerMessages.push(message)
    }
  }

  private accept(socket: net.Socket): void {
    this.socket?.destroy()
    this.socket = socket
    this.receiveBuffer = Buffer.alloc(0)
    socket.setNoDelay(true)
    this.output.appendLine(`Local WCP client connected from ${socket.remoteAddress}:${socket.remotePort}`)

    for (const message of this.pendingServerMessages.splice(0)) {
      socket.write(`${message}\0`)
    }

    socket.on('data', (data) => {
      this.receiveBuffer = Buffer.concat([this.receiveBuffer, data])
      let frameEnd = this.receiveBuffer.indexOf(0)
      while (frameEnd !== -1) {
        let message = this.receiveBuffer.subarray(0, frameEnd).toString()
        this.receiveBuffer = this.receiveBuffer.subarray(frameEnd + 1)
        const decoded = JSON.parse(message) as Record<string, unknown>
        if (
          decoded.type === 'command' &&
          decoded.command === 'load' &&
          typeof decoded.source === 'string' &&
          path.resolve(decoded.source) === path.resolve(this.waveformPath)
        ) {
          decoded.source = this.waveformUrl
          message = JSON.stringify(decoded)
        }
        this.output.appendLine(`WCP client -> server: ${message}`)
        void this.onClientMessage(message).then((delivered) => {
          if (!delivered) {
            this.output.appendLine('Failed to deliver WCP message to the Surfer webview')
          }
        })
        frameEnd = this.receiveBuffer.indexOf(0)
      }
    })
    socket.on('error', (error) => this.output.appendLine(`Local WCP client error: ${error}`))
    socket.on('close', () => {
      if (this.socket === socket) {
        this.socket = undefined
      }
      this.output.appendLine('Local WCP client disconnected')
    })
  }

  public dispose(): void {
    this.socket?.destroy()
    this.server.close()
  }
}
