// Types for `env` and `exports` from "cloudflare:workers" inside tests.
declare namespace Cloudflare {
  interface GlobalProps {
    mainModule: typeof import("../src/index");
    durableNamespaces: "RelayObject";
  }
  interface Env {
    RELAY: DurableObjectNamespace<import("../src/relay").RelayObject>;
    APNS_KEY?: string;
    APNS_KEY_ID?: string;
    APNS_TEAM_ID?: string;
    APNS_TOPIC?: string;
    APNS_URL?: string;
  }
}
