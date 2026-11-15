import { RelayObject } from "./relay";

export { RelayObject };

export interface Env {
  RELAY: DurableObjectNamespace<RelayObject>;
}

const ROUTE = /^\/v1\/relay\/([^/]*)\/(host|device|pair)$/;
const RELAY_ID = /^[0-9a-f]{32}$/;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") return new Response("ok");

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
