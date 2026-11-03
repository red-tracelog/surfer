import * as vscode from 'vscode'

let outputChannel: vscode.OutputChannel | undefined

function getOutputChannel(): vscode.OutputChannel {
  if (!outputChannel) {
    outputChannel = vscode.window.createOutputChannel('Surfer')
  }

  return outputChannel
}

function format(scope: string, message: string): string {
  return `[${scope}] ${message}`
}

export function registerLogger(context: vscode.ExtensionContext): void {
  const channel = getOutputChannel()
  if (!context.subscriptions.includes(channel)) {
    context.subscriptions.push(channel)
  }
}

export const surferLogger = {
  info(scope: string, message: string): void {
    const line = format(scope, message)
    getOutputChannel().appendLine(line)
    console.log(line)
  },

  warn(scope: string, message: string): void {
    const line = format(scope, message)
    getOutputChannel().appendLine(`WARN ${line}`)
    console.warn(line)
  },

  error(scope: string, message: string): void {
    const line = format(scope, message)
    getOutputChannel().appendLine(`ERROR ${line}`)
    console.error(line)
  },

  show(): void {
    getOutputChannel().show(true)
  },
}
