# Third-party notices

SimpleTally itself is MIT licensed (see `LICENSE`). It ships or builds against the
third-party work below, each under its own terms.

## Bundled in the executable

**DM Sans** — SIL Open Font License 1.1.
Copyright 2014 The DM Sans Project Authors (<https://github.com/googlefonts/dm-fonts>).
Licence text: [`assets/fonts/OFL-DMSans.txt`](assets/fonts/OFL-DMSans.txt).

**JetBrains Mono** — SIL Open Font License 1.1.
Copyright 2020 The JetBrains Mono Project Authors (<https://github.com/JetBrains/JetBrainsMono>).
Licence text: [`assets/fonts/OFL-JetBrainsMono.txt`](assets/fonts/OFL-JetBrainsMono.txt).

Both font files are embedded into the binary at build time (`crates/app/src/ui/theme.rs`),
so the OFL travels with every copy of the app. The OFL permits this; it forbids selling the
fonts on their own, which SimpleTally does not do, and requires that the fonts keep their
reserved names, which they do — the files are unmodified.

**SQLite** — public domain, compiled in via `rusqlite`'s `bundled` feature.

## Derived work

The pencil icon in the category manager is drawn from coordinates in Lucide's `pencil`
icon (ISC licence, <https://github.com/lucide-icons/lucide>), flattened to straight
segments; the grip is that project's `grip-vertical` dot grid. No Lucide file is
distributed — the geometry is credited in `crates/app/src/ui/types.rs` where it is drawn.

## Rust dependencies

Resolved by Cargo and not vendored here; their licences (predominantly MIT or
MIT OR Apache-2.0) are recorded in `Cargo.lock` and on crates.io. `cargo tree` lists the
full set.
