import * as vscode from 'vscode'
import * as net from 'net'
import { surferLogger } from './logger'
import { encodeLengthDelimitedFrame, SimulationFrameDecoder } from './simulationTransport'

class Wavefile implements vscode.CustomDocument {
  uri: vscode.Uri
  constructor(uri: vscode.Uri) {
    this.uri = uri
  }
  dispose(): void {}
}

/** Maps an open-dialog `kind` to the Surfer `inject_message` JSON to fire once
 *  the user picks a file, for kinds whose contents the extension host reads
 *  itself.  Returns `null` for kinds that are loaded by URL instead.
 *
 *  Injecting the bytes keeps the webview from having to fetch the file, which
 *  in turn means `localResourceRoots` does not have to be widened for it.  See
 *  `openDialogMessage` for why that matters. */
function dataDialogMessage(kind: string, bytes: Uint8Array): string | null {
  switch (kind) {
    case 'state_file':
      return JSON.stringify({ LoadStateFromData: Array.from(bytes) })
    case 'command_file':
      return JSON.stringify({ LoadCommandFromData: Array.from(bytes) })
    default:
      return null
  }
}

/** Whether `kind` is handled by `dataDialogMessage`. */
function isDataDialogKind(kind: string): boolean {
  return kind === 'state_file' || kind === 'command_file'
}

/** Maps an open-dialog `kind` to the Surfer `inject_message` JSON to fire once
 *  the user picks a file, for kinds that are loaded by URL.  Returns `null` for
 *  unknown kinds, and for kinds handled by `dataDialogMessage`.
 *
 *  Waveforms stay on the URL path deliberately: they can be very large, and
 *  marshalling one through `postMessage` as a JSON array of bytes would be far
 *  more expensive than letting the webview fetch it. */
function openDialogMessage(kind: string, fileUri: vscode.Uri, webview: vscode.Webview): string | null {
  const url = webview.asWebviewUri(fileUri).toString()
  switch (kind) {
    case 'waveform_clear':
      return JSON.stringify({ LoadWaveformFileFromUrl: [url, 'Clear'] })
    case 'waveform_keep_available':
      return JSON.stringify({ LoadWaveformFileFromUrl: [url, 'KeepAvailable'] })
    case 'waveform_keep_all':
      return JSON.stringify({ LoadWaveformFileFromUrl: [url, 'KeepAll'] })
    default:
      return null
  }
}

/** Whether `target` is already covered by one of `roots`, and so needs no new
 *  entry in `localResourceRoots`.
 *
 *  Compares `.path` rather than `.fsPath` so that remote schemes (SSH, WSL,
 *  vscode-server) are handled the same as local files.  The comparison is
 *  deliberately exact: on a case-insensitive filesystem a false negative only
 *  costs a redundant reload, whereas a false positive would leave the webview
 *  unable to fetch the file at all. */
function isCoveredByRoots(target: vscode.Uri, roots: readonly vscode.Uri[] | undefined): boolean {
  return (roots ?? []).some((root) => {
    if (target.scheme !== root.scheme || target.authority !== root.authority) {
      return false
    }
    const prefix = root.path.endsWith('/') ? root.path : `${root.path}/`
    return target.path === root.path || target.path.startsWith(prefix)
  })
}

function normalizeDialogFilters(
  filters: Array<{ name: string; extensions: string[] }> | undefined
): Record<string, string[]> {
  const filterRecord: Record<string, string[]> = {}

  for (const f of (filters ?? [])) {
    const normalized: string[] = []

    for (const extRaw of (f.extensions ?? [])) {
      const ext = (extRaw ?? '').trim().replace(/^\./, '')
      if (!ext) {
        continue
      }

      // VS Code dialog filters accept single extension segments (e.g. "ron").
      // Convert multi-part forms like "surf.ron" to their trailing segment.
      const tail = ext.split('.').filter(Boolean).pop()
      if (!tail) {
        continue
      }

      normalized.push(tail)
    }

    filterRecord[f.name] = Array.from(new Set(normalized))
  }

  return filterRecord
}

const SIMULATION_PROXY_TIMEOUT_MS = 20_000
const SIMULATION_PROXY_ATTEMPT_MS = 200

function parseSimulationBindAddress(bindAddress: string): { host: string; port: number } {
  const separator = bindAddress.lastIndexOf(':')
  if (separator < 0) {
    return { host: '127.0.0.1', port: 6967 }
  }

  const host = bindAddress.substring(0, separator) || '127.0.0.1'
  const port = Number(bindAddress.substring(separator + 1))
  return {
    host,
    port: Number.isFinite(port) && port > 0 ? port : 6967,
  }
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export class SurferWaveformViewerEditorProvider
  implements vscode.CustomReadonlyEditorProvider<Wavefile>
{
  public static register(context: vscode.ExtensionContext): vscode.Disposable {
    const provider = new SurferWaveformViewerEditorProvider(context)
    const providerRegistration = vscode.window.registerCustomEditorProvider(
      SurferWaveformViewerEditorProvider.viewType,
      provider,
      {
        webviewOptions: {
          retainContextWhenHidden: true,
        },
      }
    )

    return providerRegistration
  }
  private static readonly viewType = 'surfer.waveformViewer'

  constructor(private readonly context: vscode.ExtensionContext) {}
  async openCustomDocument(
    uri: vscode.Uri,
    _openContext: vscode.CustomDocumentOpenContext,
    _token: vscode.CancellationToken
  ): Promise<Wavefile> {
    return new Wavefile(uri)
  }
  async resolveCustomEditor(
    document: vscode.CustomDocument,
    webviewPanel: vscode.WebviewPanel,
    _token: vscode.CancellationToken
  ): Promise<void> {
    // Add the folder that the document lives in as a localResourceRoot
    const uriString = document.uri.toString()
    // TODO: this probably doesn't play well with windows
    const dirPath = uriString.substring(0, uriString.lastIndexOf('/'))

    webviewPanel.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.context.extensionUri, 'surfer'),
        vscode.Uri.parse(dirPath, true),
      ],
    }

    webviewPanel.webview.html = await this.getHtmlForWebview(webviewPanel.webview)

    const logInfo = (scope: string, message: string) => surferLogger.info(scope, message)
    const logWarn = (scope: string, message: string) => surferLogger.warn(scope, message)
    const logError = (scope: string, message: string) => surferLogger.error(scope, message)

    const documentUri = webviewPanel.webview.asWebviewUri(document.uri).toString()
    let simulationProxySocket: net.Socket | undefined
    let simulationProxyDecoder = new SimulationFrameDecoder()
    let simulationProxyConnecting = false
    let simulationProxyCancelled = false
    let simulationProxyPendingFrames: Buffer[] = []

    const stopSimulationProxy = () => {
      simulationProxyCancelled = true
      simulationProxyPendingFrames = []
      simulationProxyDecoder = new SimulationFrameDecoder()
      simulationProxyConnecting = false
      if (simulationProxySocket) {
        logInfo('simulation-proxy', 'stopping simulation proxy connection')
        simulationProxySocket.destroy()
        simulationProxySocket = undefined
      }
    }

    const postSimulationProxyMessage = (message: Record<string, unknown>) => {
      void webviewPanel.webview.postMessage(message)
    }

    const sendFrameToSimulationProxy = (frame: Buffer) => {
      const wire = encodeLengthDelimitedFrame(frame)
      const socket = simulationProxySocket
      if (!socket || socket.destroyed) {
        simulationProxyPendingFrames.push(frame)
        return
      }

      socket.write(wire, (error?: Error | null) => {
        if (error) {
          logWarn('simulation-proxy', `write callback error: ${error.message}`)
        }
      })
    }

    const flushSimulationProxyPendingFrames = () => {
      if (!simulationProxySocket || simulationProxySocket.destroyed) {
        return
      }

      for (const frame of simulationProxyPendingFrames) {
        sendFrameToSimulationProxy(frame)
      }
      simulationProxyPendingFrames = []
    }

    const handleSimulationProxyData = (chunk: Buffer) => {
      try {
        simulationProxyDecoder.push(chunk, (frame) => {
          postSimulationProxyMessage({
            command: 'simulationProxyFrame',
            frame: frame.toString('base64'),
          })
        })
      } catch (error) {
        logWarn('simulation-proxy', `invalid transport frame: ${error}`)
        stopSimulationProxy()
        postSimulationProxyMessage({ command: 'simulationProxyFailed' })
      }
    }

    const startSimulationProxy = async (bindAddress: string) => {
      if (simulationProxySocket || simulationProxyConnecting) {
        return
      }

      simulationProxyCancelled = false
      simulationProxyConnecting = true
      const { host, port } = parseSimulationBindAddress(bindAddress)
      const deadline = Date.now() + SIMULATION_PROXY_TIMEOUT_MS
      let lastError: unknown

      logInfo('simulation-proxy', `attempting to connect simulation proxy to ${host}:${port}`)

      while (Date.now() < deadline) {
        if (simulationProxyCancelled) {
          simulationProxyConnecting = false
          return
        }

        try {
          const socket = await new Promise<net.Socket>((resolve, reject) => {
            const candidate: net.Socket = net.createConnection({ host, port })
            candidate.once('connect', () => {
              resolve(candidate)
            })
            candidate.once('error', (error: Error) => {
              candidate.destroy()
              reject(error)
            })
          })

          if (simulationProxyCancelled) {
            socket.destroy()
            simulationProxyConnecting = false
            return
          }

          simulationProxySocket = socket
          simulationProxyConnecting = false

          socket.setNoDelay(true)
          socket.setKeepAlive(true)

          socket.on('data', (chunk: Buffer) => {
            handleSimulationProxyData(chunk)
          })
          socket.on('close', () => {
            logInfo('simulation-proxy', 'proxy socket closed')
            if (simulationProxySocket === socket) {
              try {
                simulationProxyDecoder.finish()
              } catch (error) {
                logWarn('simulation-proxy', `truncated transport frame: ${error}`)
              }
              simulationProxySocket = undefined
              simulationProxyDecoder = new SimulationFrameDecoder()
              postSimulationProxyMessage({ command: 'simulationProxyFailed' })
            }
          })
          socket.on('error', (error: Error) => {
            logWarn('simulation-proxy', `simulation proxy socket error: ${error}`)
          })

          logInfo('simulation-proxy', `connected to simulation proxy on ${host}:${port}`)
          flushSimulationProxyPendingFrames()
          return
        } catch (error) {
          lastError = error
          await delay(SIMULATION_PROXY_ATTEMPT_MS)
        }
      }

      simulationProxyConnecting = false
      logWarn(
        'simulation-proxy',
        `simulation proxy timed out connecting to ${host}:${port} after ${SIMULATION_PROXY_TIMEOUT_MS}ms. Last error: ${lastError}`
      )
      postSimulationProxyMessage({ command: 'simulationProxyFailed' })
    }

    // Set when widening `localResourceRoots` forces a webview reload: the
    // message that prompted the widening cannot be delivered to the outgoing
    // document, so it is held here and posted once the new one reports in.
    let pendingInject: string | undefined

    webviewPanel.webview.onDidReceiveMessage(async (message) => {
      switch (message.command) {
        case 'loaded': {
          console.log("Surfer got the loaded message from the web view")
          if (pendingInject !== undefined) {
            // Reload was ours. Resume the load that caused it rather than
            // reverting to the document this editor was opened on.
            const msg = pendingInject
            pendingInject = undefined
            webviewPanel.webview.postMessage({ command: 'InjectMessage', message: msg })
          } else {
            webviewPanel.webview.postMessage({command: "LoadUrl", url: documentUri})
          }
          break;
        }

        case 'showOpenDialog': {
          const { kind, filters } = message
          const filterRecord = normalizeDialogFilters(filters)
          const uris = await vscode.window.showOpenDialog({
            canSelectMany: false,
            filters: filterRecord,
            title: message.title ?? 'Open file',
          })
          if (uris && uris.length > 0) {
            const chosen = uris[0]

            // Assigning to `webview.options` is not a cheap metadata update: VS
            // Code compares the content options and, when they differ, re-sends
            // the webview HTML.  That restarts the WASM app, discarding the
            // current session along with any message posted right afterwards.
            // So only widen `localResourceRoots` when there is no alternative.
            if (isDataDialogKind(kind)) {
              // The extension host can read this kind itself, so the webview
              // never fetches it and the roots can stay as they are.
              try {
                const bytes = await vscode.workspace.fs.readFile(chosen)
                const msg = dataDialogMessage(kind, bytes)!
                webviewPanel.webview.postMessage({ command: 'InjectMessage', message: msg })
              } catch (e) {
                vscode.window.showErrorMessage(`Surfer: failed to read ${chosen.fsPath}: ${e}`)
              }
            } else {
              const msg = openDialogMessage(kind, chosen, webviewPanel.webview)
              if (msg === null) {
                console.log(`Surfer: unknown open-dialog kind '${kind}'`)
              } else if (isCoveredByRoots(vscode.Uri.joinPath(chosen, '..'), webviewPanel.webview.options.localResourceRoots)) {
                // Already reachable; no reload needed.
                webviewPanel.webview.postMessage({ command: 'InjectMessage', message: msg })
              } else {
                // Granting the new root reloads the webview, so hold the message
                // and post it once the fresh document announces itself.
                pendingInject = msg
                webviewPanel.webview.options = {
                  ...webviewPanel.webview.options,
                  localResourceRoots: [
                    ...(webviewPanel.webview.options.localResourceRoots ?? []),
                    vscode.Uri.joinPath(chosen, '..'),
                  ],
                }
              }
            }
          }
          break;
        }

        case 'saveState': {
          // Sent by the webview in response to 'RequestState'. Contains the
          // encoded state string and the target URI chosen in showSaveDialog.
          const targetUri = vscode.Uri.parse(message.targetUri, true)
          const encoded = new TextEncoder().encode(message.state)
          try {
            await vscode.workspace.fs.writeFile(targetUri, encoded)
          } catch (e) {
            logError('viewer', `failed to save state file: ${e}`)
            vscode.window.showErrorMessage(`Surfer: failed to save state file: ${e}`)
          }
          break;
        }

        case 'vscodeSaveStateFromWasm': {
          // Sent by the Rust code via surfer_notify_host. Contains the encoded state.
          // Show the save dialog and write the file.
          const filterRecord: Record<string, string[]> = {
            'Surfer state files': ['ron'],
          }
          const suggestedFileName =
            typeof message.fileName === 'string' && message.fileName.trim().length > 0
              ? message.fileName.trim()
              : 'surfer_state.surf.ron'
          const defaultUri = vscode.Uri.file(
            require('path').join(require('os').homedir(), suggestedFileName)
          )
          const uri = await vscode.window.showSaveDialog({
            filters: filterRecord,
            title: 'Save Surfer state',
            defaultUri,
          })
          if (uri) {
            const encoded = new TextEncoder().encode(message.data)
            try {
              await vscode.workspace.fs.writeFile(uri, encoded)
            } catch (e) {
              logError('viewer', `failed to save state file: ${e}`)
              vscode.window.showErrorMessage(`Surfer: failed to save state file: ${e}`)
            }
          }
          break;
        }

        case 'simulationProxyStart': {
          void startSimulationProxy(String(message.bindAddress ?? '127.0.0.1:6967'))
          break
        }

        case 'simulationProxySend': {
          const frameB64 = String(message.frame ?? '')
          const frame = Buffer.from(frameB64, 'base64')
          try {
            sendFrameToSimulationProxy(frame)
          } catch (error) {
            logWarn('simulation-proxy', `invalid outgoing transport frame: ${error}`)
            stopSimulationProxy()
            postSimulationProxyMessage({ command: 'simulationProxyFailed' })
          }
          break
        }

        case 'simulationProxyStop': {
          logInfo('simulation-proxy', 'received stop request from webview')
          stopSimulationProxy()
          break
        }

        default: {
          logWarn('viewer', `got unexpected command (${message.command}) from surfer web view`)
        }
      }
    })

    webviewPanel.onDidDispose(() => {
      stopSimulationProxy()
    })
  }

  // Get the static html used for the editor webviews.
  private async getHtmlForWebview(webview: vscode.Webview): Promise<string> {
    // Read index.html from disk
    const indexPath = vscode.Uri.joinPath(this.context.extensionUri, 'surfer', 'index.html')
    const contents = await vscode.workspace.fs.readFile(indexPath)

    // Replace local paths with paths derived from WebView URIs
    let html = new TextDecoder().decode(contents)

    const translate_url = (path: string[], in_html: string) => {
      const uriString = webview
        .asWebviewUri(vscode.Uri.joinPath(this.context.extensionUri, ...path))
        .toString()
      html = this.replaceAll(html, `["']${in_html}['"]`, `"${uriString}"`)
    }

    translate_url(["surfer", "manifest.json"], "/manifest.json");
    translate_url(["surfer", "surfer_bg.wasm"], "/surfer_bg.wasm");
    translate_url(["surfer", "surfer.js"], "/surfer.js");
    translate_url(["surfer", "sw.js"], "/sw.js");
    translate_url(["surfer", "integration.js"], "integration.js");

    // We need to wait until the new HTML has loaded before we can start injecting messages.
    // To detect this, we'll add a script at the every end of the html that sends a message
    // back to the host saying that the HTML is loaded.
    // A hacky but working way of doing this is to replace the </body> tag with a script
    // and a new </body> tag
    const vscodeApi = `acquireVsCodeApi()`
    const load_notifier = `
        (function() {
            const vscode = ${vscodeApi};
            // Store the vscode API globally for surfer_notify_host (from integration.js)
            window.__surfer_host_api = vscode;

            // Expose native VS Code file-dialog helpers to the Rust/WASM side.
            // These are called by the \`vscode\` feature build via wasm_bindgen externs.
            window.vscode_show_open_dialog = function(kind, filtersJson) {
                const filters = JSON.parse(filtersJson);
                vscode.postMessage({ command: 'showOpenDialog', kind, filters });
            };

            // Listen for 'RequestState' from the host and reply with the current encoded state via get_state().
            // 'InjectMessage' is deliberately not handled here: register_message_listener()
            // in integration.js already handles it for every embedder, and handling it in
            // both places injected each message twice.
            window.addEventListener('message', async (event) => {
                if (event.data?.command === 'RequestState') {
                    const state = await get_state();
                    vscode.postMessage({ command: 'saveState', targetUri: event.data.targetUri, state });
                }
            });

            vscode.postMessage({
                command: 'loaded',
            })
        }())`
    html = html.replaceAll("/*SURFER_SETUP_HOOKS*/", `${load_notifier}`)

    return html
  }

  private replaceAll(input: string, search: string, replacement: string): string {
    return input.replace(new RegExp(search, 'g'), replacement)
  }
}
