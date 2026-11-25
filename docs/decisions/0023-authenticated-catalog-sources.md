# Support private and self-hosted catalog sources

- Status: Accepted (vosu 2.0)
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
- Credentials are keyed by the verbatim configured source, never a parsed
  alias. Their authorized network origin is `(scheme, host, effective port)`.
  **A6b.1, approved 2026-11-24:** GitHub shorthand and repository URLs on
  `https://github.com` derive exactly `https://api.github.com:443` for release
  metadata and API catalog-asset requests only. Static GitHub-host URLs keep
  their literal origin; other forges keep their repository origin. No arbitrary
  API override or inferred enterprise host is permitted.
- Validate the initial asset URL as well as every redirect. Any origin change
  strips all credential headers permanently for that request chain, including
  A→B→A. Probe/package/art/updater/counter requests never carry source secrets;
  use immutable typed purpose policy, not a shared authenticated client.
- Forge-specific asset retrieval (the GitHub API asset endpoint, GitLab
  `Authorization: Bearer` on its API, Gitea and Forgejo `Authorization: token`
  on their release attachments) is verified against each forge when
  implemented.

### Private networks

A user-added source may allow private-network destinations for its own exact
configured host, only for catalog, unauthenticated forge-probe and catalog-asset
requests. HTTPS and OS trust-store certificate/hostname verification remain
mandatory, including installed internal CAs and existing public roots. The
exception **never** applies to package or artwork URLs, even on the same host,
nor to suffixes/subdomains/CNAME target names or foreign redirect hosts.
Same-host different-port requests may retain private-host permission but lose
credentials because their origins differ. Validate/pin DNS answers at every
hop and retain no-proxy/manual redirects. `AGENTS.md` is amended by the
transport implementation, not by weakening its unrelated download invariants.

### Trust

Items from private sources are `user` trust. The user can mark a source as
trusted, after a warning that its plugins may run inside vosu with full memory
access ([ADR 0019](0019-plugin-runtime-and-trust.md)). Official trust remains
the exact-identity allowlist of the default sources; integrity checks still do
not establish author trust.

### User experience

The user pastes a repository URL. A 404 alone does not establish a private
repository exists; offer credential guidance without asserting existence or
echoing response bodies. Link to the forge token page and name its verified
minimal read-only scopes. Once a credential is saved, the source uses the same
validation/plan flow. Private package/art authentication is not supported:
authors need unauthenticated public HTTPS or presigned artifacts.

## Consequences

- Private and self-hosted catalogs work on every forge without a vosu-run
  service.
- The download client gains credential scoping and a per-host private-network
  exception, both of which need regression tests for redirects across origins.
- A trusted private source can run in-process plugins; that is the user's
  explicit decision.
- **A6b.2–A6b.8, approved 2026-11-24:** the normative
  [source and credential lifecycle](../v2/catalog-v2.md#sources-and-credentials)
  freezes grammar/detection, bounded secret persistence/repair, dedicated
  source flags, exact-identity removal, ranked graph search, immutable plans,
  transactional publication, revocation pending user restart and package limits.
  Windows uses inherited directory ACLs and shared atomic replacement, not an
  invented owner-only DACL or encrypted store.

## Alternatives considered

- **Credentials embedded in URLs:** rejected; catalogs, logs, and dashboard
  views would expose them.
- **An operating-system keyring:** rejected for now; it adds a second secret
  store beside ADR 0015's and requires a desktop session service on Linux.
- **Reading credentials from git credential helpers:** rejected for now; the
  daemon would run user-configured helper programs.
