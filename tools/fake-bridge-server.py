# -*- coding: utf-8 -*-
"""假桥服务器（D-001b 外桥最小实测 e2e 用）

收任意 POST，一律回 200 {"dx":3}（含数字 3——桥演示生物读到会连走 3 拍向东）。
stdlib only，无依赖。监听 127.0.0.1:18080（bridges.json 登记的默认端点）。

用法：python tools/fake-bridge-server.py
日志：每请求一行 "[fake-bridge] POST received bytes=N -> replied"（stdout，
bridge-e2e.bat 重定向到 fake-bridge.log 做断言）。
"""
import http.server
import json

REPLY = json.dumps({"dx": 3}).encode("ascii")

IDLE_EXIT_S = 15  # 空闲自杀保险丝：15 秒无新请求自动退出（连跑 e2e 不撞文件锁，
# bat 不必杀 python——防误杀他进程）


class QuietServer(http.server.HTTPServer):
    """空闲超时退出版 HTTPServer（handle_timeout 由 handle_request 的 select 超时触发）"""

    def handle_timeout(self):
        print(f"[fake-bridge] idle {IDLE_EXIT_S}s, exiting", flush=True)
        raise SystemExit


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        # 读完整请求体（Content-Length 定界；世界客户端 Connection: close 短连接）
        n = int(self.headers.get("Content-Length", 0) or 0)
        body = self.rfile.read(n) if n > 0 else b""
        print(
            f"[fake-bridge] POST received bytes={len(body)} -> replied",
            flush=True,
        )
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(REPLY)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(REPLY)

    def log_message(self, *args):
        # 静默 BaseHTTPRequestHandler 默认访问日志——断言只认我们自己的行
        pass


if __name__ == "__main__":
    print("[fake-bridge] listening on 127.0.0.1:18080", flush=True)
    server = QuietServer(("127.0.0.1", 18080), Handler)
    server.timeout = IDLE_EXIT_S
    try:
        while True:
            server.handle_request()  # 阻塞至请求或空闲超时
    except SystemExit:
        pass
