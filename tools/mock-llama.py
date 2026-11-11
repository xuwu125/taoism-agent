#!/usr/bin/env python3
"""最小 OpenAI 兼容 mock：只给符箓台的人工验收用。

用途：本机没有 LFM 权重与 `llama-server` 时，也能让人把
「探活 -> 成符盖印 -> 焚符 -> 增量显示 -> 回执 -> 火光」这条界面链路走完。

边界（写清楚，免得被当成模型效果）：
- 只监听 127.0.0.1，不联网、不下载、不写文件；
- 回复是固定文本，**不代表模型能力**；验收结论只能写「界面状态正确」，
  不能写「推理正常」；
- 不校验 Authorization，不模拟工具调用，不模拟错误分支（断流另有单测与 e2e）。

用法：
    python tools/mock-llama.py --port 18871
    # 符箓台右上「端点」填 http://127.0.0.1:18871 -> 探活 -> 应用

自检：
    curl -s http://127.0.0.1:18871/health
    curl -N -X POST http://127.0.0.1:18871/v1/chat/completions \
         -H 'Content-Type: application/json' \
         -d '{"model":"m","stream":true,"stream_options":{"include_usage":true},"messages":[]}'
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPLY = "这是本地 mock 的固定回执：界面状态可验，模型效果不可验。"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "fulu-mock/1"

    def log_message(self, fmt: str, *args) -> None:  # 只打一行摘要，不刷屏
        sys.stderr.write("[mock] %s %s\n" % (self.command, self.path))

    # ------------------------------------------------------------------ GET
    def do_GET(self) -> None:
        if self.path.startswith("/health"):
            self._json(200, {"status": "ok"})
        elif self.path.startswith("/v1/models"):
            self._json(
                200,
                {
                    "object": "list",
                    "data": [
                        {
                            "id": "lfm3-1.7b-q4km",
                            "object": "model",
                            "owned_by": "mock",
                        }
                    ],
                },
            )
        else:
            self._json(404, {"error": {"message": "not found"}})

    # ----------------------------------------------------------------- POST
    def do_POST(self) -> None:
        if not self.path.startswith("/v1/chat/completions"):
            self._json(404, {"error": {"message": "not found"}})
            return

        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            self._json(400, {"error": {"message": "bad json"}})
            return

        streaming = bool(body.get("stream"))
        include_usage = bool((body.get("stream_options") or {}).get("include_usage"))
        sys.stderr.write(
            "[mock] stream=%s include_usage=%s model=%s messages=%d\n"
            % (
                streaming,
                include_usage,
                body.get("model"),
                len(body.get("messages") or []),
            )
        )
        sys.stderr.flush()

        if streaming:
            self._sse(include_usage)
        else:
            self._json(200, self._completion(include_usage))

    # ------------------------------------------------------------- helpers
    def _completion(self, with_usage: bool) -> dict:
        # 故意短：示例符判据是 output.len <= 200，短回执能让符脚判定达标，
        # 这样界面才走得到「火光」那一步。
        payload = {
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": "lfm3-1.7b-q4km",
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": REPLY},
                    "finish_reason": "stop",
                }
            ],
        }
        if with_usage:
            payload["usage"] = {
                "prompt_tokens": 11,
                "completion_tokens": len(REPLY),
                "total_tokens": 11 + len(REPLY),
            }
        return payload

    def _sse(self, include_usage: bool) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True

        # 按 6 字切段，人眼能看出是「逐段到达」而不是一次性贴上去。
        for index in range(0, len(REPLY), 6):
            delta = {
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "model": "lfm3-1.7b-q4km",
                "choices": [
                    {
                        "index": 0,
                        "delta": {"content": REPLY[index : index + 6]},
                        "finish_reason": None,
                    }
                ],
            }
            self._chunk("data: " + json.dumps(delta, ensure_ascii=False) + "\n\n")
            time.sleep(0.12)

        tail = {
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "model": "lfm3-1.7b-q4km",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        }
        if include_usage:
            tail["usage"] = {
                "prompt_tokens": 11,
                "completion_tokens": len(REPLY),
                "total_tokens": 11 + len(REPLY),
            }
        self._chunk("data: " + json.dumps(tail, ensure_ascii=False) + "\n\n")
        self._chunk("data: [DONE]\n\n")
        self._chunk("")  # 结束块

    def _chunk(self, text: str) -> None:
        data = text.encode("utf-8")
        self.wfile.write(b"%x\r\n%s\r\n" % (len(data), data))
        self.wfile.flush()

    def _json(self, status: int, payload: dict) -> None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()
        self.close_connection = True


def main() -> int:
    parser = argparse.ArgumentParser(description="fulu 符箓台的本地 mock（仅回环）")
    parser.add_argument("--port", type=int, default=18871)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    sys.stderr.write("[mock] 只监听 127.0.0.1:%d，固定回执，不代表模型效果\n" % args.port)
    sys.stderr.flush()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        sys.stderr.write("[mock] 停止\n")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
