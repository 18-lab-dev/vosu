# vosu-plugin-api

The Rust SDK for [vosu](https://github.com/18-lab-dev/vosu) 2.0 native
plugins: plugin ABI v1 types that mirror the C header `vosu_plugin.h` field by
field, the flat and CBOR message codecs, the `Plugin` and `ConcurrentPlugin`
traits with their `export_plugin!` entry point, and the package, catalog and
namespace-schema validators that the vosu host itself runs.

The SDK's major version is the ABI major (1); it is versioned independently of
the vosu application (2.x). Depend on an exact prerelease while 1.0 is in
alpha. Use the pin below once `1.0.0-alpha.3` is published:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
vosu-plugin-api = "=1.0.0-alpha.3"
```

## A plugin

```rust
use vosu_plugin_api::{
    Init, Message, Plugin, PluginError, VtableHostServices, export_plugin,
};

struct MyPlugin {
    host: VtableHostServices,
}

impl Plugin for MyPlugin {
    fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self { host })
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        if let Message::SessionAttached(_attached) = message {
            // Identify the session, submit read plans, publish state ...
        }
        Ok(())
    }
}

export_plugin!(MyPlugin);
```

`Plugin::on_message` takes `&mut self` and the SDK serializes calls behind its
own lock. A plugin whose manifest sets `concurrent_dispatch = true` implements
`ConcurrentPlugin` (`&self`, internally synchronized) instead and exports it
with `export_plugin!(concurrent MyPlugin)`. `export_plugin!` generates the one
symbol the host loads, `vosu_plugin_entry_v1`; a panic inside plugin code
never crosses the FFI boundary.

Host services (`HostServices`) cover session identification, read plans,
signature scans, state publication and subscriptions, resources, timers,
logging and action calls. `HostServices::send_flat`, `PublishBatchWriter` and
`encode_read_plan_submit_into` keep the hot paths allocation-free.

## Validating a package before publishing

`manifest` validates a package's `manifest.toml` against its explicit
registration context, `manifest::schema` compiles namespace schemas into
sampling-lane records, and `catalog` validates a `catalog-v2.json` and
compares every entry with the package it describes. The
`vosu-package-check` example runs all of them over a publisher's catalog and
its already-extracted packages.
Packages declaring `[runtime.linux]` need the checker from
`1.0.0-alpha.3` or later; older checkers reject the runtime table.

```sh
cargo install --locked --version "=1.0.0-alpha.3" vosu-plugin-api \
    --example vosu-package-check --root "$PWD/sdk-tools"
sdk-tools/bin/vosu-package-check --catalog "$PWD/dist/catalog-v2.json" \
    --packages-root "$PWD/dist/extracted" --host-version 2.0.0-alpha.1 \
    --source "<your exact source identity>" --registry-context "$PWD/registry-context.json"
```

Each package for target `T` is read from
`<packages-root>/<plugins|tools|overlays>/<id>/<T>/`. The registry-context
file (format 1) lists the official source identities, the registrations and
namespace revisions to validate against, and the host release's
`common-defs.schema.json` document; its exact format is documented in the
example's `context.rs`. A pass grants no trust: only the host decides which
source is official.

## Not in this crate

Synchronous read plans for vosu's own bundled readers and the registration
glue for plugins compiled into the vosu executable are workspace-internal and
are not published. The C header `vosu_plugin.h` and the packaging JSON
schemas ship as vosu release assets.

## Documentation

The normative contracts are the plugin ABI (`docs/v2/plugin-abi.md`), API v1
(`docs/v2/api-v1.md`) and catalog v2 (`docs/v2/catalog-v2.md`) in the vosu
repository.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
