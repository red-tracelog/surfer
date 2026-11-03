import * as vscode from 'vscode'
import { SurferWaveformViewerEditorProvider } from './surferWaveformViewer'
import { registerLogger, surferLogger } from './logger'

export function activate(context: vscode.ExtensionContext) {
  registerLogger(context)
  surferLogger.info('extension', 'Surfer has been activated')
  context.subscriptions.push(SurferWaveformViewerEditorProvider.register(context))
}
