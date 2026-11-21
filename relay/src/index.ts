import { jsonResponse, RelayObject } from "./relay";

export { RelayObject };

export interface Env {
  RELAY: DurableObjectNamespace<RelayObject>;
}

const ROUTE = /^\/v1\/relay\/([^/]*)\/(host|device|pair)$/;
const HOOK_ROUTE = /^\/v1\/hook\/([^/]*)\/([^/]*)$/;
const RELAY_ID = /^[0-9a-f]{32}$/;
const HOOK_ID = /^[A-Za-z0-9_-]{22,64}$/;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") return new Response("ok");

    const hook = HOOK_ROUTE.exec(url.pathname);
    if (hook) {
      if (!RELAY_ID.test(hook[1]!)) return jsonResponse(400, { error: "bad relay_id" });
      if (!HOOK_ID.test(hook[2]!)) return jsonResponse(404, { error: "unknown hook" });
      if (request.method !== "POST") {
        return jsonResponse(405, { error: "method not allowed" }, { allow: "POST" });
      }
      return env.RELAY.get(env.RELAY.idFromName(hook[1]!)).fetch(request);
    }

    const m = ROUTE.exec(url.pathname);
    if (!m) return new Response("not found", { status: 404 });
    if (!RELAY_ID.test(m[1]!)) return new Response("bad relay_id", { status: 400 });
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("websocket required", { status: 426 });
    }
    const stub = env.RELAY.get(env.RELAY.idFromName(m[1]!));
    return stub.fetch(request);
  },
} satisfies ExportedHandler<Env>;
