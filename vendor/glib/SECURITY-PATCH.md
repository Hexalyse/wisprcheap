# GLib 0.18 security backport

This is the crates.io `glib` 0.18.5 source (MIT; original LICENSE and COPYRIGHT retained).
Original crate SHA-256: `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.

The only source change is the two-line upstream fix for
[GHSA-wrw7-89jp-8q8g / RUSTSEC-2024-0429](https://github.com/advisories/GHSA-wrw7-89jp-8q8g):
`VariantStrIter::impl_get` declares its output pointer mutable and passes `&mut p` to
`g_variant_get_child`. This avoids undefined behavior and optimized-build crashes.
Upstream fix: https://github.com/gtk-rs/gtk-rs-core/pull/1343.

GTK3, Tao, tray-icon and gtk-layer-shell require the GLib 0.18 ABI/API family. Upgrading
only GLib to 0.20 cannot satisfy those dependencies. The workspace patch applies to every
transitive GLib 0.18 consumer without changing the upstream version number or dismissing
the GitHub alert. Version-only scanners may continue reporting the advisory; inspect the
resolved source and this backport. No advisory is globally ignored.

Run the upstream iterator regression tests **with optimizations** on a host with GLib
development libraries:

```sh
cargo test --manifest-path vendor/glib/Cargo.toml --release --lib variant_iter::tests
```

Remove this patch when the Linux desktop dependencies can migrate away from GTK3.
