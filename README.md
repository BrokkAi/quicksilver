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

## Research notes

The [`papers/`](papers/) directory contains research notes related to mutation
testing, causal program dependence, and higher-order mutants.

## License

This project is licensed under the GNU Lesser General Public License, version
3.0. See [LICENSE](LICENSE) for the complete license text.
