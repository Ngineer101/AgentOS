# Contributing

Thanks for your interest. agenthost is a personal project, shared in the open
so others can run it, learn from it and build on it. That shapes how
contributions work.

## Fork it and make it yours

If you want agenthost to work differently (another agent, another platform,
a different UI, your own relay setup), **fork the repository and make your
changes in your fork**. The licence (MIT or Apache-2.0) lets you change and
redistribute it freely. Forks don't need to stay in sync with this
repository, and you don't need to ask.

## Issues are the most helpful contribution

If you've found a bug or have an idea for this project, please open an
issue. A detailed issue is worth more here than a pull request. A good one
includes:

- **What you did**: the commands, or the steps in the app, so it can be
  reproduced.
- **What you expected**, and **what happened instead**: the exact error
  text, the output of `agenthost status`, and relevant lines from the host
  log (`<data dir>/logs/host.log`; with `AGENTHOST_ACP_TRACE=1` for agent
  problems). Remove anything private first.
- **Your setup**: OS and version, which component (host, CLI, desktop, iOS,
  relay), the commit you built, and the agent CLI and its version.
- For ideas: the problem you're trying to solve, not only the solution you
  have in mind.

## Pull requests

Pull requests aren't the main way this project takes changes, and unsolicited
ones may be closed without being merged, even good ones; that's about keeping
the project manageable, not about the quality of the work. If you think a
change really belongs here, open an issue first to discuss it. A small fix
for an obvious bug (a typo, a broken link, a clear one-line error) is fine to
send directly.

If you do send one: keep it focused, run `cargo test --workspace` (and
`npm test` in `relay/` or `cargo test` in `desktop/` if you touched those),
and explain what it changes and how you checked it. Contributions are
licensed as described in the [README](README.md#licence).

## Security issues

Please don't open public issues for vulnerabilities. See
[SECURITY.md](SECURITY.md).
