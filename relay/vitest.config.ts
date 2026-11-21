import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";
import { b64urlDecode } from "./src/codec";

// An APNs key for the push tests, and an APNs stand-in the Worker's outbound
// requests go to (in Node, outside workerd).
const ECDSA = { name: "ECDSA", namedCurve: "P-256" } as const;
const apns = (await crypto.subtle.generateKey(ECDSA, true, ["sign", "verify"])) as CryptoKeyPair;
const pkcs8 = new Uint8Array((await crypto.subtle.exportKey("pkcs8", apns.privateKey)) as ArrayBuffer);
const APNS_KEY = `-----BEGIN PRIVATE KEY-----\n${btoa(String.fromCharCode(...pkcs8))}\n-----END PRIVATE KEY-----\n`;
const json = (part: string) => JSON.parse(new TextDecoder().decode(b64urlDecode(part) ?? new Uint8Array()));

/**
 * Like APNs: checks the provider token, topic and payload; answers 410
 * `Unregistered` for tokens starting `dead`, `BadDeviceToken` outside the
 * sandbox for tokens starting `5b`, and for tokens starting `ec` echoes the
 * request back as the reason (so tests can look at it).
 */
async function fakeApns(request: Request): Promise<Response> {
  const url = new URL(request.url);
  const refuse = (status: number, reason: string) =>
    new Response(JSON.stringify({ reason }), { status, headers: { "content-type": "application/json" } });
  if (url.host !== "apns.test") return refuse(599, `unexpected host ${url.host}`);
  const m = /^\/(sandbox|production)\/3\/device\/([0-9a-f]+)$/.exec(url.pathname);
  if (!m || request.method !== "POST") return refuse(404, "BadPath");
  const [, env, token] = m as unknown as [string, string, string];
  const auth = request.headers.get("authorization") ?? "";
  const jwt = auth.replace(/^bearer /, "").split(".");
  if (jwt.length !== 3) return refuse(403, "MissingProviderToken");
  const header = json(jwt[0]!);
  const claims = json(jwt[1]!);
  const ok = await crypto.subtle.verify(
    { name: "ECDSA", hash: "SHA-256" },
    apns.publicKey,
    b64urlDecode(jwt[2]!) ?? new Uint8Array(),
    new TextEncoder().encode(`${jwt[0]}.${jwt[1]}`),
  );
  if (!ok || header.alg !== "ES256" || header.kid !== "KEY1234567" || claims.iss !== "TEAM123456") {
    return refuse(403, "InvalidProviderToken");
  }
  if (Math.abs(Date.now() / 1000 - claims.iat) > 3600) return refuse(403, "ExpiredProviderToken");
  if (request.headers.get("apns-topic") !== "dev.agenthost.ios") return refuse(400, "TopicDisallowed");
  if (request.headers.get("apns-push-type") !== "alert") return refuse(400, "InvalidPushType");
  const body = await request.text();
  const payload = JSON.parse(body);
  if (payload.aps?.["mutable-content"] !== 1 || typeof payload.e !== "string") return refuse(400, "BadPayload");
  if (token.startsWith("dead")) return refuse(410, "Unregistered");
  if (token.startsWith("5b") && env !== "sandbox") return refuse(400, "BadDeviceToken");
  if (token.startsWith("ec")) {
    return refuse(
      400,
      JSON.stringify({ env, collapse: request.headers.get("apns-collapse-id"), payload }),
    );
  }
  return new Response(null, { status: 200 });
}

export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.toml" },
      miniflare: {
        bindings: {
          APNS_KEY,
          APNS_KEY_ID: "KEY1234567",
          APNS_TEAM_ID: "TEAM123456",
          APNS_TOPIC: "dev.agenthost.ios",
          APNS_URL: "https://apns.test",
        },
        outboundService: fakeApns as never,
      },
    }),
  ],
  test: { testTimeout: 15_000 },
});
