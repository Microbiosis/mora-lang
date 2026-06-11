// Mora VS Code extension — 启动 mora-lsp
//
// 这是 dev 占位版。生产 VSIX 需用 `tsc` 编译 extension.ts。
// 真实使用：装 vscode LSP 客户端库（vscode-languageclient）做协议封装；
// 本文件只演示 child_process 启动 + stdio 接入。

const { spawn } = require('child_process');
const vscode = require('vscode');

let lspProcess = null;
let languageClient = null;

/** @param {vscode.ExtensionContext} context */
function activate(context) {
    const config = vscode.workspace.getConfiguration('mora');
    const lspPath = config.get('languageServer.path', 'mora-lsp');
    const lspArgs = config.get('languageServer.args', []);

    // 简易启动：直接 spawn，不走 LSP 协议封装
    // （生产推荐用 vscode-languageclient 库自动处理 JSON-RPC 帧）
    lspProcess = spawn(lspPath, lspArgs, {
        stdio: ['pipe', 'pipe', 'pipe'],
    });

    if (lspProcess.stderr) {
        lspProcess.stderr.on('data', (data) => {
            console.log(`[mora-lsp] ${data.toString().trimEnd()}`);
        });
    }

    if (lspProcess.on) {
        lspProcess.on('exit', (code) => {
            vscode.window.showInformationMessage(`mora-lsp exited with code ${code}`);
        });
    }

    vscode.window.showInformationMessage('Mora language server activated.');
    console.log('mora-lsp started, pid=' + lspProcess.pid);
}

function deactivate() {
    if (lspProcess) {
        lspProcess.kill();
    }
}

module.exports = { activate, deactivate };
