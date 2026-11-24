# Security policy

AgentOS runs coding agents on your computer and gives them limited access
to it, so security problems matter here: a way out of the sandbox, past the
egress proxy, around an approval, or into the relay's traffic.

## Reporting a vulnerability

Please report it privately through GitHub: on this repository's **Security**
tab, choose **Report a vulnerability**. Don't open a public issue.

Include what you can of: the component (host, sandbox, proxy, MCP gateway,
relay, CLI, desktop, iOS), the commit, your platform, the steps to reproduce,
and what an attacker gains. A proof of concept helps.

This is a personal project maintained in spare time, so there's no response
time guarantee, but reports are taken seriously and fixed as quickly as
possible.

## Scope

In scope: anything that breaks the guarantees in
[docs/security.md](docs/security.md), such as a session reading or writing
outside what its sandbox allows, reaching hosts its network profile
doesn't, running something outside the sandbox without approval, or the
relay (or anyone on the network) reading or tampering with tunnel traffic.

Out of scope: problems in the agents themselves or their providers, in
Anthropic's `sandbox-runtime`, bubblewrap or Seatbelt (report those
upstream), and anything that needs an attacker who already runs code as your
user outside the sandbox.

Only the latest commit on `main` is supported.
