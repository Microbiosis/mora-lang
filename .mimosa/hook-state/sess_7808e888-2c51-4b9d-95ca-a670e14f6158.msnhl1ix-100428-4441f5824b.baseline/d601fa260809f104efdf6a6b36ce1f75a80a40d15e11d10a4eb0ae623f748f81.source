#!/usr/bin/env python3
"""LSP 端到端测试：v0.04 脚本（含 p"" / with / tool / break）。"""
import subprocess
import sys
import json
import os

LSP = r"d:\Github\mora-lang\target\debug\mora-lsp.exe"
# 用 temp dir 避免污染 examples/ 目录
import tempfile
_tmpdir = tempfile.mkdtemp(prefix="mora_lsp_v04_")
CODE_PATH = os.path.join(_tmpdir, "lsp_v04_test.mora")
with open(CODE_PATH, "w", encoding="utf-8") as f:
    f.write("""let name = "World"
let r = p"hello {name}"
print(r)

with model = "gpt-4o"
  let x = p"nested"
end

tool search(q: string): string do
  return "ok"
end
""")

p = subprocess.Popen(
    [LSP],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)

def send(msg):
    body = msg.encode("utf-8")
    header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
    p.stdin.write(header + body)
    p.stdin.flush()

def read():
    # 读 headers
    cl = None
    while True:
        line = p.stdout.readline()
        if not line:
            return ""
        line = line.decode("ascii", errors="ignore").rstrip("\r\n")
        if line == "":
            break
        if ":" in line:
            k, v = line.split(":", 1)
            if k.strip().lower() == "content-length":
                cl = int(v.strip())
    if cl is None:
        return ""
    body = p.stdout.read(cl).decode("utf-8", errors="ignore")
    return body

def req(id, method, params):
    return json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params})

def notif(method, params):
    return json.dumps({"jsonrpc": "2.0", "method": method, "params": params})

# 1. initialize
send(req(1, "initialize", {"capabilities": {}}))
init = read()
print(f"[1] init: {len(init)} bytes")
assert "capabilities" in init, "no capabilities"

# 2. initialized
send(notif("initialized", {}))

# 3. didOpen with v0.04 code
with open(CODE_PATH, "r", encoding="utf-8") as f:
    code = f.read()
send(notif("textDocument/didOpen", {
    "textDocument": {
        "uri": f"file:///{CODE_PATH.replace(os.sep, '/')}",
        "languageId": "mora",
        "version": 1,
        "text": code,
    }
}))
diag = read()
print(f"[2] diagnostics: {diag[:200]}")
assert "publishDiagnostics" in diag, "no diagnostics"
# v0.04 代码合法，不应有 typeck 错
assert "type mismatch" not in diag, f"unexpected typeck error in v0.04 code: {diag}"

# 4. completion at line 0 col 0 — 测 4 个新关键字都在
send(req(3, "textDocument/completion", {
    "textDocument": {"uri": f"file:///{CODE_PATH.replace(os.sep, '/')}"},
    "position": {"line": 0, "character": 0},
}))
comp = read()
print(f"[3] completion: {len(comp)} bytes")
for kw in ["with", "stream", "tool", "break", "continue"]:
    assert f'"{kw}"' in comp, f"keyword '{kw}' missing from completion"
    print(f"  ✓ keyword '{kw}' in completion")

# 5. hover at p"..." position
send(req(4, "textDocument/hover", {
    "textDocument": {"uri": f"file:///{CODE_PATH.replace(os.sep, '/')}"},
    "position": {"line": 1, "character": 10},  # 鼠标在 prompt 字符串上
}))
hover = read()
print(f"[4] hover: {len(hover)} bytes (id=4? {'\"id\":4' in hover})")

# 6. shutdown
send(req(99, "shutdown", None))
read()
send(notif("exit", None))
p.wait(timeout=5)

print("\n=== V0.04 LSP E2E TEST PASSED ===")
