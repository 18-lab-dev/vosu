# Support private and self-hosted catalog sources

- Status: Proposed
- Date: 2026-11-21
- Extends: [ADR 0015](0015-user-managed-api-keys.md), [ADR 0017](0017-tool-host.md) (catalog sources and trust)

## Context

1.x catalog sources are a GitHub `owner/repo` shorthand or an HTTPS URL to a
`catalog.json`. Downloads require HTTPS, refuse private and reserved
destinations after every redirect, and carry no credentials. Users who publish
plugins, tools, or overlays for themselves keep them in private repositories on
GitHub, GitLab, or self-hosted forges, often on a LAN or VPN address.

Private release assets need forge-specific requests. On GitHub, a private asset
is fetched through the API asset endpoint with
`Accept: application/octet-stream`, which redirects to presigned storage that
must not receive the token. The downloader follows redirects itself
(`reqwest::redirect::Policy::none()` in `src/net/streaming.rs`), so reqwest's
automatic header stripping does not apply.

## Decision

### Sources

A source is one of:

- `owner/repo` or `owner/repo@tag`, which remain GitHub shorthand;
- a repository URL on any host, for example GitHub, GitLab (including nested
  groups), Codeberg, or a self-hosted GitLab, Gitea, or Forgejo instance, whose
  forge type vosu detects from the host and its API;
- an HTTPS URL ending in `/catalog-v2.json` on any web server.

### Credentials

- A source may have one credential: a token, sent as the forge expects, or a
  username and password for plain HTTPS servers.
- Credentials are stored as API keys are
  ([ADR 0015](0015-user-managed-api-keys.md)): a config-root file
  (`catalog-credentials.toml`) replaced atomically in the same directory, with
  mode 0600 on Linux.
- The dashboard API can set or remove a credential but never returns it.
  Credentials are never exposed to plugins, tools, or overlays, and catalog
  package URLs remain credential-free.
- A credential is sent only to the source's exact origin (scheme, host, and
  port). The redirect loop drops it whenever a redirect changes the origin.
- Forge-specific asset retrieval (the GitHub API asset endpoint, GitLab
  `Authorization: Bearer` on its API, Gitea and Forgejo `Authorization: token`
  on their release attachments) is verified against each forge when
  implemented.

### Private networks

A user-added source may allow private-network destinations for its own exact
host. HTTPS remains mandatory, and the exception never applies to package or
artwork URLs on other hosts. Certificate validation uses the operating system's
trust store, so an internal certificate authority installed there is accepted.
This narrows, for one user-chosen host, the private-destination rule that
`AGENTS.md` states for downloads; that statement is updated with the
implementation.

### Trust

Items from private sources are `user` trust. The user can mark a source as
trusted, after a warning that its plugins may run inside vosu with full memory
access ([ADR 0019](0019-plugin-runtime-and-trust.md)). Official trust remains
the exact-identity allowlist of the default sources; integrity checks still do
not establish author trust.

### User experience

The user pastes a repository URL. If the repository is private, the dashboard
says so, links to the forge's token page, and names the minimal read-only scope
for that forge. After the token is saved, the source behaves like any other.

## Consequences

- Private and self-hosted catalogs work on every forge without a vosu-run
  service.
- The download client gains credential scoping and a per-host private-network
  exception, both of which need regression tests for redirects across origins.
- A trusted private source can run in-process plugins; that is the user's
  explicit decision.

## Alternatives considered

- **Credentials embedded in URLs:** rejected; catalogs, logs, and dashboard
  views would expose them.
- **An operating-system keyring:** rejected for now; it adds a second secret
  store beside ADR 0015's and requires a desktop session service on Linux.
- **Reading credentials from git credential helpers:** rejected for now; the
  daemon would run user-configured helper programs.
