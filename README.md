# cargo-quicksilver

Impact-selected mutation testing for Cargo workspaces. A mutant only runs the
tests that execute it. Build and test pools are separate.

```
cargo install --path .
cargo quicksilver probe
cargo quicksilver list
cargo quicksilver estimate --package <crate> --sample 150
```

See `cargo quicksilver --help`.
