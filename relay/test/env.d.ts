// Types for `env` and `exports` from "cloudflare:workers" inside tests.
declare namespace Cloudflare {
  interface GlobalProps {
    mainModule: typeof import("../src/index");
    durableNamespaces: "RelayObject";
  }
  interface Env {
    RELAY: DurableObjectNamespace<import("../src/relay").RelayObject>;
  }
}
